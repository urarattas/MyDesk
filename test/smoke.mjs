/**
 * End-to-end smoke test for the signaling server.
 *
 * Starts server.js on a free port, drives two WebSocket clients through the
 * full create -> join -> offer/answer/ICE relay -> disconnect flow, and checks
 * the static app endpoint. Uses only Node built-ins (WebSocket is global since
 * Node 22), so it runs in CI without installing anything.
 *
 *   node test/smoke.mjs
 */
import { spawn } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PORT = Number(process.env.SMOKE_PORT || 18080);
const BASE = `http://127.0.0.1:${PORT}`;

const results = [];
function check(name, fn) {
  return Promise.resolve()
    .then(fn)
    .then(() => {
      results.push({ name, ok: true });
      console.log(`PASS  ${name}`);
    })
    .catch((err) => {
      results.push({ name, ok: false, err });
      console.log(`FAIL  ${name}  — ${err.message}`);
    });
}

/** Queue incoming messages so tests never race the socket. */
function track(ws) {
  const queue = [];
  const waiters = [];
  ws.addEventListener("message", (ev) => {
    const msg = JSON.parse(ev.data);
    const waiter = waiters.shift();
    if (waiter) waiter.resolve(msg);
    else queue.push(msg);
  });
  return {
    ws,
    send(obj) { ws.send(JSON.stringify(obj)); },
    next(timeout = 4000, label = "message") {
      if (queue.length) return Promise.resolve(queue.shift());
      return new Promise((resolve, reject) => {
        const entry = { resolve };
        const timer = setTimeout(() => {
          const i = waiters.indexOf(entry);
          if (i >= 0) waiters.splice(i, 1);
          reject(new Error(`timeout waiting for ${label}`));
        }, timeout);
        entry.resolve = (msg) => { clearTimeout(timer); resolve(msg); };
        waiters.push(entry);
      });
    },
    close() { try { ws.close(); } catch (e) { /* ignore */ } }
  };
}

function openSocket() {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(`ws://127.0.0.1:${PORT}/ws`);
    ws.addEventListener("open", () => resolve(track(ws)), { once: true });
    ws.addEventListener("error", () => reject(new Error("websocket failed to open")), { once: true });
  });
}

const server = spawn(process.execPath, ["server.js"], {
  cwd: ROOT,
  env: { ...process.env, PORT: String(PORT), HOST: "127.0.0.1" },
  stdio: ["ignore", "pipe", "pipe"]
});
let serverLog = "";
server.stdout.on("data", (d) => { serverLog += d.toString(); });
server.stderr.on("data", (d) => { serverLog += d.toString(); });

async function waitForServer() {
  for (let i = 0; i < 100; i += 1) {
    try {
      const res = await fetch(`${BASE}/healthz`);
      if (res.ok) return;
    } catch (e) { /* not up yet */ }
    await sleep(100);
  }
  throw new Error(`server did not start:\n${serverLog}`);
}

let host;
let guest;

try {
  await waitForServer();

  await check("serves the app", async () => {
    const res = await fetch(`${BASE}/`);
    assert.equal(res.status, 200);
    assert.match(res.headers.get("content-type"), /text\/html/);
    const html = await res.text();
    assert.ok(html.includes("MyDesk"), "app body missing");
    assert.ok(html.length > 10_000, "app body too small");
  });

  await check("healthz reports rooms", async () => {
    const res = await fetch(`${BASE}/healthz`);
    const body = await res.json();
    assert.equal(body.ok, true);
    assert.equal(typeof body.rooms, "number");
  });

  await check("404 for unknown path", async () => {
    const res = await fetch(`${BASE}/nope`);
    assert.equal(res.status, 404);
  });

  await check("host creates a room", async () => {
    host = await openSocket();
    host.send({ type: "create" });
    const msg = await host.next(4000, "room");
    assert.equal(msg.type, "room");
    assert.match(msg.code, /^[A-Z2-9]{6}$/);
    host.code = msg.code;
  });

  await check("guest joins the room", async () => {
    guest = await openSocket();
    guest.send({ type: "join", code: host.code });
    const joined = await guest.next(4000, "joined");
    assert.equal(joined.type, "joined");
    assert.equal(joined.code, host.code);
    const peer = await host.next(4000, "peer");
    assert.equal(peer.type, "peer");
  });

  await check("third peer is rejected", async () => {
    const extra = await openSocket();
    try {
      extra.send({ type: "join", code: host.code });
      const msg = await extra.next(4000, "error");
      assert.equal(msg.type, "error");
      assert.match(msg.message, /房间/);
    } finally {
      extra.close();
    }
  });

  await check("unknown room is rejected", async () => {
    const extra = await openSocket();
    try {
      extra.send({ type: "join", code: "ZZZZZZ" });
      const msg = await extra.next(4000, "error");
      assert.equal(msg.type, "error");
    } finally {
      extra.close();
    }
  });

  await check("offer is relayed host -> guest", async () => {
    host.send({ type: "offer", sdp: "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n" });
    const msg = await guest.next(4000, "offer");
    assert.equal(msg.type, "offer");
    assert.match(msg.sdp, /^v=0/);
  });

  await check("answer is relayed guest -> host", async () => {
    guest.send({ type: "answer", sdp: "v=0\r\no=- 3 4 IN IP4 127.0.0.1\r\n" });
    const msg = await host.next(4000, "answer");
    assert.equal(msg.type, "answer");
    assert.match(msg.sdp, /^v=0/);
  });

  await check("ICE candidate is relayed guest -> host", async () => {
    guest.send({ type: "ice", candidate: { candidate: "candidate:1 1 udp 1 127.0.0.1 5000 typ host" } });
    const msg = await host.next(4000, "ice");
    assert.equal(msg.type, "ice");
    assert.match(msg.candidate.candidate, /typ host/);
  });

  await check("large payload survives the frame parser", async () => {
    const big = "x".repeat(120_000);
    guest.send({ type: "offer", sdp: big });
    const msg = await host.next(6000, "big offer");
    assert.equal(msg.sdp.length, big.length);
  });

  await check("peer-left is delivered when the guest leaves", async () => {
    guest.close();
    const msg = await host.next(4000, "peer-left");
    assert.equal(msg.type, "peer-left");
  });

  await check("room is gone after everyone leaves", async () => {
    host.close();
    await sleep(300);
    const res = await fetch(`${BASE}/healthz`);
    const body = await res.json();
    assert.equal(body.rooms, 0);
  });
} catch (err) {
  console.error("fatal:", err.message);
  console.error(serverLog);
  process.exitCode = 1;
} finally {
  try { host && host.close(); } catch (e) { /* ignore */ }
  try { guest && guest.close(); } catch (e) { /* ignore */ }
  server.kill("SIGTERM");
}

const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} passed`);
if (failed.length) {
  if (serverLog) console.log("--- server log ---\n" + serverLog);
  process.exitCode = 1;
}
