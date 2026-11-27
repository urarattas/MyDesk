/**
 * Regression tests for the hardening work in server.js. Each case maps to a
 * concrete incident class:
 *
 *   - a single malformed request target used to throw ERR_INVALID_URL inside the
 *     request listener, which is an uncaught exception: one packet killed the
 *     whole signaling process;
 *   - a WebSocket fragment stream that never sets FIN was accepted forever, so a
 *     client could grow the server heap without limit;
 *   - `join` had no throttling at all: one socket could guess room codes as fast
 *     as the CPU allowed (measured ~10k/s);
 *   - a cross-site page could open /ws because the Origin check only warned.
 *
 *   node test/hardening.mjs
 */
import { spawn } from "node:child_process";
import net from "node:net";
import assert from "node:assert/strict";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PORT = Number(process.env.HARDENING_PORT || 18096);
const BASE = `http://127.0.0.1:${PORT}`;

const results = [];
function check(name, fn) {
  return Promise.resolve()
    .then(fn)
    .then(() => { results.push({ name, ok: true }); console.log(`PASS  ${name}`); })
    .catch((err) => { results.push({ name, ok: false, err }); console.log(`FAIL  ${name}  — ${err.message}`); });
}

/** Frame the client sends to the server: always masked, per RFC 6455. */
function clientFrame(fin, opcode, payload) {
  const len = payload.length;
  const b0 = (fin ? 0x80 : 0) | opcode;
  let header;
  if (len < 126) header = Buffer.from([b0, 0x80 | len]);
  else if (len < 65536) {
    header = Buffer.alloc(4);
    header[0] = b0;
    header[1] = 0x80 | 126;
    header.writeUInt16BE(len, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = b0;
    header[1] = 0x80 | 127;
    header.writeBigUInt64BE(BigInt(len), 2);
  }
  const mask = Buffer.from([3, 1, 4, 1]);
  const body = Buffer.from(payload);
  for (let i = 0; i < body.length; i += 1) body[i] ^= mask[i & 3];
  return Buffer.concat([header, mask, body]);
}

function encode(obj) {
  return clientFrame(true, 0x1, Buffer.from(JSON.stringify(obj)));
}

/** Minimal client: tracks the HTTP status line, JSON messages and close code. */
function openPipe(headers = {}) {
  return new Promise((resolve, reject) => {
    const sock = net.connect(PORT, "127.0.0.1");
    const state = { status: "", msgs: [], closeCode: null, closed: false };
    let buf = Buffer.alloc(0);
    let handshook = false;
    sock.on("error", () => {});
    sock.on("close", () => { state.closed = true; });
    sock.on("data", (chunk) => {
      buf = Buffer.concat([buf, chunk]);
      if (!handshook) {
        const i = buf.indexOf("\r\n\r\n");
        if (i < 0) return;
        state.status = buf.subarray(0, i).toString().split("\r\n")[0];
        handshook = true;
        buf = buf.subarray(i + 4);
      }
      for (;;) {
        if (buf.length < 2) return;
        const opcode = buf[0] & 0x0f;
        let len = buf[1] & 0x7f;
        let off = 2;
        if (len === 126) { if (buf.length < 4) return; len = buf.readUInt16BE(2); off = 4; }
        else if (len === 127) { if (buf.length < 10) return; len = Number(buf.readBigUInt64BE(2)); off = 10; }
        if (buf.length < off + len) return;
        const payload = buf.subarray(off, off + len);
        buf = buf.subarray(off + len);
        if (opcode === 0x1) { try { state.msgs.push(JSON.parse(payload.toString())); } catch (e) { /* ignore */ } }
        if (opcode === 0x8) state.closeCode = payload.length >= 2 ? payload.readUInt16BE(0) : 0;
      }
    });
    sock.on("connect", () => {
      const base = {
        Host: `127.0.0.1:${PORT}`,
        Upgrade: "websocket",
        Connection: "Upgrade",
        "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
        "Sec-WebSocket-Version": "13",
        ...headers,
      };
      sock.write(`GET /ws HTTP/1.1\r\n${Object.entries(base).map(([k, v]) => `${k}: ${v}`).join("\r\n")}\r\n\r\n`);
      resolve({ sock, state, send: (o) => sock.write(encode(o)), wait: (ms = 150) => sleep(ms) });
    });
    sock.on("error", reject);
  });
}

/** Raw HTTP request, resolved with the status line and the header block. */
function rawRequest(requestText) {
  return new Promise((resolve) => {
    const sock = net.connect(PORT, "127.0.0.1", () => sock.write(requestText));
    let buf = "";
    sock.on("data", (d) => { buf += d.toString(); });
    sock.on("error", () => resolve({ status: "", head: "", body: buf }));
    sock.on("close", () => {
      const i = buf.indexOf("\r\n\r\n");
      const head = i < 0 ? buf : buf.slice(0, i);
      resolve({ status: head.split("\r\n")[0] || "", head, body: i < 0 ? "" : buf.slice(i + 4) });
    });
    setTimeout(() => { try { sock.destroy(); } catch (e) {} resolve({ status: "<timeout>", head: buf, body: "" }); }, 2500);
  });
}

const server = spawn(process.execPath, ["server.js"], {
  cwd: ROOT,
  env: {
    ...process.env,
    PORT: String(PORT),
    HOST: "127.0.0.1",
    JOIN_FAIL_BURST: "5",
    JOIN_RATE: "1000",
    MAX_ROOMS: "8",
  },
  stdio: ["ignore", "pipe", "pipe"],
});
let serverLog = "";
server.stdout.on("data", (d) => { serverLog += d.toString(); });
server.stderr.on("data", (d) => { serverLog += d.toString(); });

async function waitForServer() {
  for (let i = 0; i < 100; i += 1) {
    try { const res = await fetch(`${BASE}/healthz`); if (res.ok) return; } catch (e) { /* not up yet */ }
    await sleep(100);
  }
  throw new Error(`server did not start:\n${serverLog}`);
}

let host;
let guest;

try {
  await waitForServer();

  await check("malformed request target is a 400, not a crash", async () => {
    const res = await rawRequest("GET //[ HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    assert.match(res.status, / 400 /, `unexpected status: ${res.status}`);
    const health = await fetch(`${BASE}/healthz`);
    assert.equal(health.status, 200, "process should still be alive");
  });

  await check("app response refuses framing", async () => {
    const res = await rawRequest(`GET / HTTP/1.1\r\nHost: 127.0.0.1:${PORT}\r\nConnection: close\r\n\r\n`);
    const head = res.head.toLowerCase();
    assert.match(head, /x-frame-options: deny/);
    assert.match(head, /frame-ancestors 'none'/);
  });

  await check("cross-site upgrade is rejected", async () => {
    const pipe = await openPipe({ "Sec-Fetch-Site": "cross-site" });
    await pipe.wait(250);
    pipe.sock.destroy();
    assert.match(pipe.state.status, / 403 /, `unexpected status: ${pipe.state.status}`);
  });

  await check("same-origin upgrade is accepted", async () => {
    const pipe = await openPipe({ "Sec-Fetch-Site": "same-origin" });
    await pipe.wait(250);
    pipe.sock.destroy();
    assert.match(pipe.state.status, / 101 /, `unexpected status: ${pipe.state.status}`);
  });

  await check("host creates a room, guest joins, big offer is relayed", async () => {
    host = await openPipe();
    host.send({ type: "create" });
    await host.wait(250);
    const room = host.state.msgs.find((m) => m.type === "room");
    assert.ok(room && room.code, "no room message");
    guest = await openPipe();
    guest.send({ type: "join", code: room.code });
    await guest.wait(350);
    assert.ok(guest.state.msgs.some((m) => m.type === "joined"), "guest did not join");
    // host 现在要显式允许才会发 offer，这里直接绕过页面逻辑发一条大 offer
    const sdp = "v=0\r\n" + "z".repeat(119000);
    const payload = Buffer.from(JSON.stringify({ type: "offer", sdp }));
    const third = Math.floor(payload.length / 3);
    guest.sock.write(clientFrame(false, 0x1, payload.subarray(0, third)));
    guest.sock.write(clientFrame(false, 0x0, payload.subarray(third, third * 2)));
    guest.sock.write(clientFrame(true, 0x0, payload.subarray(third * 2)));
    await guest.wait(500);
    const offer = host.state.msgs.find((m) => m.type === "offer");
    assert.ok(offer, "host did not receive the offer");
    assert.equal(offer.sdp.length, sdp.length, "fragmented message was truncated");
  });

  await check("fragment stream without FIN is capped and closed", async () => {
    const pipe = await openPipe();
    await pipe.wait(200);
    pipe.sock.write(clientFrame(false, 0x1, Buffer.alloc(60000, 0x61)));
    for (let i = 0; i < 40; i += 1) {
      if (pipe.state.closed || pipe.sock.destroyed) break;
      pipe.sock.write(clientFrame(false, 0x0, Buffer.alloc(60000, 0x61)));
      await sleep(40);
    }
    await pipe.wait(600);
    assert.ok(pipe.state.closed || pipe.sock.destroyed, "server kept the unbounded fragment stream open");
    const health = await fetch(`${BASE}/healthz`);
    assert.equal(health.status, 200, "server died while capping fragments");
    pipe.sock.destroy();
  });

  await check("join guessing is throttled on one socket", async () => {
    const pipe = await openPipe();
    await pipe.wait(200);
    const alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    for (let i = 0; i < 12; i += 1) {
      const code = Array.from({ length: 6 }, () => alphabet[Math.floor(Math.random() * alphabet.length)]).join("");
      pipe.send({ type: "join", code });
      await sleep(25);
    }
    await pipe.wait(500);
    const errors = pipe.state.msgs.filter((m) => m.type === "error").map((m) => m.message || "");
    const throttled = errors.filter((m) => m.includes("频繁")).length;
    assert.ok(errors.length > 0, "no error replies at all");
    assert.ok(throttled > 0, `expected a throttle message, got: ${JSON.stringify(errors)}`);
    pipe.sock.destroy();
  });
} catch (err) {
  console.error("fatal:", err.message);
  console.error(serverLog);
  process.exitCode = 1;
} finally {
  try { host && host.sock.destroy(); } catch (e) { /* ignore */ }
  try { guest && guest.sock.destroy(); } catch (e) { /* ignore */ }
  server.kill("SIGTERM");
}

const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} passed`);
if (failed.length) {
  if (serverLog) console.log("--- server log ---\n" + serverLog);
  process.exitCode = 1;
}
