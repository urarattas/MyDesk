/**
 * Protocol-level checks that need no browser.
 *
 *  - /healthz must not leak room/socket counts unless HEALTHZ_DETAIL=1
 *  - a frame with RSV1 set must be rejected with close code 1002 (RFC 6455 §5.2:
 *    no extension was negotiated, so the reserved bits must be zero)
 *
 *   node test/protocol.mjs
 */
import { spawn } from "node:child_process";
import net from "node:net";
import crypto from "node:crypto";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PORT = Number(process.env.PROTO_PORT || 18094);

const results = [];
const check = (name, ok, extra = "") => {
  results.push({ name, ok });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${extra ? "  — " + extra : ""}`);
};

function startServer(env) {
  const p = spawn(process.execPath, ["server.js"], {
    cwd: ROOT,
    env: { ...process.env, PORT: String(PORT), HOST: "127.0.0.1", ...env },
    stdio: ["ignore", "pipe", "pipe"],
  });
  p.stdout.on("data", () => {});
  p.stderr.on("data", () => {});
  return p;
}

async function waitHealthy() {
  for (let i = 0; i < 80; i++) {
    try {
      const r = await fetch(`http://127.0.0.1:${PORT}/healthz`);
      if (r.ok) return true;
    } catch (e) { /* wait */ }
    await sleep(120);
  }
  return false;
}

/** 手写一次 upgrade，然后发一个带 RSV1 的帧 */
function expectClose1002() {
  return new Promise((resolve, reject) => {
    const sock = net.connect(PORT, "127.0.0.1");
    let buf = Buffer.alloc(0);
    let upgraded = false;
    const timer = setTimeout(() => { sock.destroy(); reject(new Error("timeout")); }, 5000);
    sock.on("connect", () => {
      const key = crypto.randomBytes(16).toString("base64");
      sock.write(
        `GET /ws HTTP/1.1\r\nHost: 127.0.0.1:${PORT}\r\nUpgrade: websocket\r\n` +
        `Connection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n\r\n`
      );
    });
    sock.on("data", (d) => {
      buf = Buffer.concat([buf, d]);
      if (!upgraded) {
        const idx = buf.indexOf("\r\n\r\n");
        if (idx === -1) return;
        const head = buf.subarray(0, idx).toString();
        if (!/^HTTP\/1\.1 101/.test(head)) { clearTimeout(timer); reject(new Error(head.split("\r\n")[0])); return; }
        upgraded = true;
        buf = buf.subarray(idx + 4);
        // FIN + RSV1 + text，掩码、长度 2
        const mask = crypto.randomBytes(4);
        const payload = Buffer.from("hi");
        const frame = Buffer.concat([
          Buffer.from([0x80 | 0x40 | 0x01, 0x80 | payload.length]),
          mask,
          Buffer.from([payload[0] ^ mask[0], payload[1] ^ mask[1]]),
        ]);
        sock.write(frame);
      }
      if (buf.length >= 4) {
        const opcode = buf[0] & 0x0f;
        const code = buf.readUInt16BE(2);
        clearTimeout(timer);
        sock.destroy();
        resolve({ opcode, code });
      }
    });
    sock.on("error", (e) => { clearTimeout(timer); reject(e); });
  });
}

try {
  // 1) 默认不暴露计数
  let srv = startServer({});
  if (!(await waitHealthy())) throw new Error("server did not start");
  const plain = await (await fetch(`http://127.0.0.1:${PORT}/healthz`)).json();
  check("healthz keeps only ok", Object.keys(plain).join(",") === "ok", JSON.stringify(plain));

  // 2) RSV 置位必须回 1002
  const res = await expectClose1002();
  check("RSV bits rejected with 1002", res.opcode === 0x8 && res.code === 1002, JSON.stringify(res));
  srv.kill("SIGTERM");
  await sleep(300);

  // 3) 显式开启后才有计数
  srv = startServer({ HEALTHZ_DETAIL: "1" });
  if (!(await waitHealthy())) throw new Error("server did not start (detail)");
  const detail = await (await fetch(`http://127.0.0.1:${PORT}/healthz`)).json();
  check("HEALTHZ_DETAIL=1 adds counters",
    typeof detail.rooms === "number" && typeof detail.sockets === "number",
    JSON.stringify(detail));
  srv.kill("SIGTERM");
} catch (err) {
  console.error("fatal:", err.message);
  results.push({ name: "run", ok: false });
}

const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
