/**
 * Regression test: a `#room=...` link must not connect on its own.
 *
 * Anyone can hand out `https://<server>/#room=ABCD2345`. If the page joined
 * automatically, the sender (who is the host) could push an SDP offer and drag
 * the visitor into a session without a single click. The page may prefill the
 * code, but the join has to be the user's action.
 *
 *   node test/link-join.mjs
 *
 * Needs Chrome/Chromium (CHROME_BIN); skips otherwise.
 */
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const APP_PORT = Number(process.env.LINK_PORT || 18095);
const CDP_PORT = Number(process.env.LINK_CDP_PORT || 9358);
const CODE = "ABCD2345";

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

const server = spawn(process.execPath, ["server.js"], {
  cwd: ROOT,
  env: { ...process.env, PORT: String(APP_PORT), HOST: "127.0.0.1" },
  stdio: ["ignore", "pipe", "pipe"],
});
let serverLog = "";
server.stdout.on("data", (d) => { serverLog += d; });
server.stderr.on("data", (d) => { serverLog += d; });

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
  for (let i = 0; i < 80; i++) {
    try { const r = await fetch(`http://127.0.0.1:${APP_PORT}/healthz`); if (r.ok) break; } catch (e) { /* wait */ }
    await sleep(150);
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
  await send("Page.navigate", { url: `http://127.0.0.1:${APP_PORT}/#room=${CODE}` });

  const ev = async (expr) => {
    const r = await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true, userGesture: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    return r.result.value;
  };

  await sleep(2200); // 老代码会在 400ms 后自动 join

  const value = await ev(`document.getElementById('joinCode').value`);
  check("code is prefilled", value === CODE, value);

  const pc = await ev(`window.MyDesk.state.pc === null`);
  check("no peer connection created", pc === true);
  check("no websocket opened by itself", !/\[ws\] #\d+ open/.test(serverLog), serverLog.trim().split("\n").pop() || "");
  check("server saw no join", !/joined by/.test(serverLog));

  // 点了才连：这里点「加入」，房间不存在，应当报错而不是卡住
  await ev(`document.getElementById('btnJoinRoom').click()`);
  await sleep(1200);
  const logText = await ev(`document.getElementById('logBox').innerText`);
  check("clicking does attempt a join", /房间不存在|正在协商|加入房间|房间号/.test(logText), logText.split("\n").pop());
} catch (err) {
  console.error("fatal:", err.message);
  console.error(serverLog.slice(-400));
  results.push({ name: "run", ok: false });
} finally {
  browser.kill("SIGKILL");
  server.kill("SIGTERM");
}

const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
