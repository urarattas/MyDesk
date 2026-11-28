/**
 * Regression test for the CSP that ships with the desktop app.
 *
 * The Tauri webview loads MyDesk.html with the policy from tauri.conf.json. If the
 * policy is too tight the app loses WebRTC, canvas capture or the signaling
 * socket; if it is missing entirely an XSS in the page gets everything. This test
 * reads the real policy out of tauri.conf.json, serves the page behind a proxy
 * that adds it, and drives the server mode through it.
 *
 *   node test/csp.mjs
 *
 * Needs Chrome/Chromium (CHROME_BIN) and node's global WebSocket; skips otherwise.
 */
import { spawn } from "node:child_process";
import http from "node:http";
import net from "node:net";
import { readFileSync, existsSync } from "node:fs";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const APP_PORT = Number(process.env.CSP_APP_PORT || 18098);
const PROXY_PORT = Number(process.env.CSP_PROXY_PORT || 18099);
const CDP_PORT = Number(process.env.CSP_CDP_PORT || 9356);

const conf = JSON.parse(readFileSync(path.join(ROOT, "desktop/crates/mydesk-desktop/tauri.conf.json"), "utf8"));
const CSP = conf?.app?.security?.csp;
if (!CSP || typeof CSP !== "string") {
  console.error("tauri.conf.json has no app.security.csp");
  process.exit(1);
}

function findChrome() {
  const env = process.env.CHROME_BIN;
  if (env && existsSync(env)) return env;
  for (const p of [
    "/usr/bin/google-chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser",
    path.join(ROOT, ".devtest/chrome-headless-shell-linux64/chrome-headless-shell"),
  ]) if (existsSync(p)) return p;
  return null;
}
const chromeBin = findChrome();
if (!chromeBin) {
  console.log("SKIP: no Chrome/Chromium found (set CHROME_BIN)");
  process.exit(0);
}

const results = [];
const check = (name, ok, extra = "") => {
  results.push({ name, ok });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${extra ? "  — " + extra : ""}`);
};

/* 应用服务器（信令 + 页面），和 CI 里跑的是同一个 server.js */
const app = spawn(process.execPath, ["server.js"], {
  cwd: ROOT,
  env: { ...process.env, PORT: String(APP_PORT), HOST: "127.0.0.1" },
  stdio: ["ignore", "pipe", "pipe"],
});
let appLog = "";
app.stdout.on("data", (d) => { appLog += d; });
app.stderr.on("data", (d) => { appLog += d; });

/* 只做一件事：给响应加 CSP，并把 upgrade 原样转发 */
const proxy = http.createServer((req, res) => {
  const up = http.request(
    { host: "127.0.0.1", port: APP_PORT, path: req.url, method: req.method, headers: req.headers },
    (r) => {
      res.writeHead(r.statusCode || 502, { ...r.headers, "content-security-policy": CSP });
      r.pipe(res);
    },
  );
  up.on("error", () => { res.writeHead(502); res.end("bad gateway"); });
  req.pipe(up);
});
proxy.on("upgrade", (req, socket, head) => {
  const up = net.connect(APP_PORT, "127.0.0.1", () => {
    const lines = [`${req.method} ${req.url} HTTP/1.1`];
    for (const [k, v] of Object.entries(req.headers)) lines.push(`${k}: ${v}`);
    up.write(lines.join("\r\n") + "\r\n\r\n");
    if (head && head.length) up.write(head);
    up.pipe(socket);
    socket.pipe(up);
  });
  up.on("error", () => socket.destroy());
});

const browser = spawn(chromeBin, [
  "--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
  `--remote-debugging-port=${CDP_PORT}`, "--remote-allow-origins=*", "about:blank",
], { stdio: ["ignore", "ignore", "pipe"] });

class CDP {
  static async connect(url) {
    const ws = new WebSocket(url);
    await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error("ws")); });
    return new CDP(ws);
  }
  constructor(ws) {
    this.ws = ws; this.id = 0; this.pending = new Map();
    ws.onmessage = (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && this.pending.has(m.id)) {
        const { res, rej } = this.pending.get(m.id);
        this.pending.delete(m.id);
        m.error ? rej(new Error(m.error.message)) : res(m.result);
      }
    };
  }
  send(method, params = {}, sessionId) {
    const id = ++this.id;
    const p = new Promise((res, rej) => this.pending.set(id, { res, rej }));
    this.ws.send(JSON.stringify({ id, method, params, sessionId }));
    return p;
  }
}

try {
  await new Promise((res) => proxy.listen(PROXY_PORT, "127.0.0.1", res));
  for (let i = 0; i < 80; i++) {
    try { const r = await fetch(`http://127.0.0.1:${APP_PORT}/healthz`); if (r.ok) break; } catch (e) { /* wait */ }
    await sleep(120);
  }

  let wsUrl;
  for (let i = 0; i < 80; i++) {
    try {
      const r = await fetch(`http://127.0.0.1:${CDP_PORT}/json/version`);
      wsUrl = (await r.json()).webSocketDebuggerUrl;
      if (wsUrl) break;
    } catch (e) { /* wait */ }
    await sleep(200);
  }
  const cdp = await CDP.connect(wsUrl);
  const { targetId } = await cdp.send("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await cdp.send("Target.attachToTarget", { targetId, flatten: true });
  const send = (m, p) => cdp.send(m, p, sessionId);
  await send("Page.enable");
  await send("Runtime.enable");

  // 页面里装一个 CSP 违规采集器（必须在加载前注入）
  await send("Page.addScriptToEvaluateOnNewDocument", {
    source: `
      window.__cspViolations = [];
      document.addEventListener("securitypolicyviolation", (e) => {
        window.__cspViolations.push({ directive: e.violatedDirective, blocked: e.blockedURI, sample: (e.sample||"").slice(0,80) });
      });
    `,
  });

  await send("Page.navigate", { url: `http://127.0.0.1:${PROXY_PORT}/` });

  const ev = async (expr) => {
    const r = await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true, userGesture: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    return r.result.value;
  };
  const waitFor = async (expr, ms = 15000, label = expr) => {
    const t0 = Date.now();
    while (Date.now() - t0 < ms) {
      try { if (await ev(expr)) return true; } catch (e) { /* retry */ }
      await sleep(150);
    }
    throw new Error("timeout: " + label);
  };

  await waitFor(`document.getElementById('pillText') && document.getElementById('pillText').textContent.length > 0`, 10000, "boot");

  const headerCsp = await fetch(`http://127.0.0.1:${PROXY_PORT}/`).then((r) => r.headers.get("content-security-policy"));
  check("proxy really sent the policy", headerCsp === CSP);

  const booted = await ev(`document.getElementById('btnCreateRoom') !== null`);
  check("page boots under the CSP", booted === true);

  // 建房间会走 ws://（connect-src），被 CSP 拦下就不会有房间号
  await ev(`document.getElementById('btnCreateRoom').click()`);
  await waitFor(`/^[A-Z2-9]{8}$/.test(document.getElementById('roomCode').textContent.trim())`, 15000, "room code");
  const code = (await ev(`document.getElementById('roomCode').textContent.trim()`));
  check("WebSocket allowed by connect-src", /^[A-Z2-9]{8}$/.test(code), code);

  const violations = await ev(`JSON.stringify(window.__cspViolations || [])`);
  check("no CSP violations", JSON.parse(violations).length === 0, violations.slice(0, 200));
} catch (err) {
  console.error("fatal:", err.message);
  console.error(appLog.slice(-500));
  results.push({ name: "run", ok: false });
} finally {
  browser.kill("SIGKILL");
  proxy.close();
  app.kill("SIGTERM");
}

const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
