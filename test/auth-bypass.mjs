/**
 * Regression test: the room "allow this peer?" prompt must not be bypassable by
 * pushing an SDP offer straight at the host.
 *
 * The server is a dumb relay, so the gateway lives in the page: only the side
 * that *joined* a room may answer an offer, and only the side that *created* it
 * may accept an answer. Before that check, anyone holding the room code could
 * skip the prompt by sending `{type:"offer"}` themselves.
 *
 *   node test/auth-bypass.mjs
 *
 * Needs a Chrome/Chromium binary; set CHROME_BIN or have google-chrome /
 * chromium in PATH. Skipped (exit 0) when nothing is available.
 */
import { spawn } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const APP_PORT = Number(process.env.AUTH_PORT || 18097);
const CDP_PORT = Number(process.env.AUTH_CDP_PORT || 9355);
const APP = `http://127.0.0.1:${APP_PORT}/`;

function findChrome() {
  const env = process.env.CHROME_BIN;
  if (env && existsSync(env)) return env;
  for (const p of [
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/mnt/data/Code/Code/MyDesk/.devtest/chrome-headless-shell-linux64/chrome-headless-shell",
  ]) {
    if (existsSync(p)) return p;
  }
  return null;
}

const chrome = findChrome();
if (!chrome) {
  console.log("SKIP: no Chrome/Chromium found (set CHROME_BIN)");
  process.exit(0);
}

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

const results = [];
const check = (name, ok, extra = "") => {
  results.push({ name, ok });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${extra ? "  — " + extra : ""}`);
};

const server = spawn(process.execPath, ["server.js"], {
  cwd: ROOT,
  env: { ...process.env, PORT: String(APP_PORT), HOST: "127.0.0.1", TRUSTED_ORIGINS: "" },
  stdio: ["ignore", "pipe", "pipe"],
});
let serverLog = "";
server.stdout.on("data", (d) => { serverLog += d; });
server.stderr.on("data", (d) => { serverLog += d; });

const browser = spawn(chrome, [
  "--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
  `--remote-debugging-port=${CDP_PORT}`, "--remote-allow-origins=*", "about:blank",
], { stdio: ["ignore", "ignore", "pipe"] });

try {
  for (let i = 0; i < 80; i++) {
    try { const r = await fetch(`${APP}healthz`); if (r.ok) break; } catch (e) { /* wait */ }
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
  await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 800, deviceScaleFactor: 1, mobile: false });
  await send("Page.navigate", { url: APP });

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

  await sleep(900);
  await ev(`document.getElementById('btnCreateRoom').click()`);
  // 占位符是 6 个短横线，必须匹配字母表而不是长度
  await waitFor(`/^[A-Z2-9]{6}$/.test(document.getElementById('roomCode').textContent.trim())`, 15000, "room code");
  const code = await ev(`document.getElementById('roomCode').textContent.trim()`);
  check("host created a room", /^[A-Z2-9]{6}$/.test(code), code);

  // 攻击者：加入房间后直接推 offer，跳过「允许连接」
  const attacker = new WebSocket(`ws://127.0.0.1:${APP_PORT}/ws`);
  const inbox = [];
  attacker.onmessage = (e) => inbox.push(JSON.parse(e.data));
  await new Promise((res, rej) => { attacker.onopen = res; attacker.onerror = rej; });
  attacker.send(JSON.stringify({ type: "join", code }));
  await waitFor(`document.getElementById('peerAsk') && !document.getElementById('peerAsk').hidden`, 8000, "approval prompt");
  check("host shows the approval prompt", true);

  attacker.send(JSON.stringify({ type: "offer", sdp: "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\n" }));
  await sleep(800);

  const pcAfterAttack = await ev(`window.MyDesk.state.pc === null`);
  check("unsolicited offer is ignored", pcAfterAttack === true, "pc=" + (await ev(`window.MyDesk.state.pc ? "created" : "null"`)));
  check("no answer sent to the attacker", inbox.filter((m) => m.type === "answer").length === 0);

  // 正常路径仍然可用：点「允许连接」后主机会主动发 offer
  await ev(`document.getElementById('btnPeerAllow').click()`);
  await sleep(800);
  const offers = inbox.filter((m) => m.type === "offer");
  check("approved peer does receive a real offer", offers.length === 1 && offers[0].sdp.includes("v=0"));

  attacker.close();
} catch (err) {
  console.error("fatal:", err.message);
  console.error(serverLog.slice(-600));
  results.push({ name: "run", ok: false });
} finally {
  browser.kill("SIGKILL");
  server.kill("SIGTERM");
}

const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
