/**
 * Regression test for the WebSocket upgrade behind a reverse proxy / tunnel.
 *
 * Cloudflare Tunnel (and other proxies) often rewrite the Host header, add a
 * port, or only send X-Forwarded-Host. The origin used to compare Origin and
 * Host verbatim and answer 403, which the tunnel surfaced as a 502.
 *
 *   node test/proxy.mjs
 */
import { spawn } from "node:child_process";
import net from "node:net";
import assert from "node:assert/strict";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PORT = Number(process.env.PROXY_PORT || 18095);

function handshake(port, headers) {
  return new Promise((resolve) => {
    const sock = net.connect(port, "127.0.0.1", () => {
      sock.write([
        "GET /ws HTTP/1.1",
        ...Object.entries(headers).map(([k, v]) => `${k}: ${v}`),
        "", ""
      ].join("\r\n"));
    });
    let buf = "";
    const done = (r) => { try { sock.destroy(); } catch (e) { /* ignore */ } resolve(r); };
    sock.on("data", (d) => {
      buf += d.toString();
      if (buf.includes("\r\n\r\n")) done(buf.split("\r\n")[0]);
    });
    sock.on("error", (e) => done("ERR " + e.message));
    setTimeout(() => done("TIMEOUT"), 3000);
  });
}

const base = {
  Host: `localhost:${PORT}`,
  Upgrade: "websocket",
  Connection: "Upgrade",
  "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
  "Sec-WebSocket-Version": "13",
};

const server = spawn(process.execPath, ["server.js"], {
  cwd: ROOT,
  // 测试要读 rooms/sockets 计数，显式打开明细（生产默认不暴露）
  env: { ...process.env, PORT: String(PORT), HOST: "127.0.0.1", HEALTHZ_DETAIL: "1" },
  stdio: ["ignore", "pipe", "pipe"]
});
let log = "";
server.stdout.on("data", (d) => { log += d.toString(); });
server.stderr.on("data", (d) => { log += d.toString(); });

let failed = 0;
try {
  for (let i = 0; i < 50; i += 1) {
    try { const r = await fetch(`http://127.0.0.1:${PORT}/healthz`); if (r.ok) break; } catch (e) { /* wait */ }
    await sleep(100);
  }

  const cases = [
    ["direct", { ...base, Origin: `http://localhost:${PORT}` }],
    ["tunnel keeps public host", { ...base, Host: "desk.example.com", Origin: "https://desk.example.com" }],
    ["tunnel rewrites host", { ...base, Origin: "https://desk.example.com" }],
    ["host carries a port", { ...base, Host: "desk.example.com:443", Origin: "https://desk.example.com" }],
    ["only x-forwarded-host", { ...base, Origin: "https://desk.example.com", "X-Forwarded-Host": "desk.example.com" }],
  ];

  for (const [name, headers] of cases) {
    const line = await handshake(PORT, headers);
    const ok = line.includes("101");
    if (!ok) failed++;
    console.log(`${ok ? "PASS" : "FAIL"}  ${name.padEnd(24)} ${line}`);
  }

  const plain = await fetch(`http://127.0.0.1:${PORT}/ws`);
  assert.equal(plain.status, 426, "plain GET /ws should answer 426");
  console.log("PASS  plain GET /ws           426 Upgrade Required");

  const health = await (await fetch(`http://127.0.0.1:${PORT}/healthz`)).json();
  assert.equal(health.ok, true);
  assert.equal(typeof health.sockets, "number");
  console.log("PASS  /healthz has live counters");
} catch (err) {
  console.error("fatal:", err.message);
  failed++;
} finally {
  server.kill("SIGTERM");
}

console.log(`\n${failed ? "FAILED " + failed : "ok"}`);
if (failed && log) console.log("--- server log ---\n" + log.slice(-800));
process.exit(failed ? 1 : 0);
