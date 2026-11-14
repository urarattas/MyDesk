#!/usr/bin/env node
/**
 * MyDesk signaling server.
 *
 * Serves the single-file app and relays SDP/ICE between exactly two peers that
 * share a room code. No dependencies: HTTP plus a minimal RFC 6455 WebSocket
 * implementation is enough for this protocol.
 *
 * Protocol (JSON over WebSocket at /ws):
 *   client -> server  {type:"create"}                      -> {type:"room", code}
 *   client -> server  {type:"join", code}                  -> {type:"joined"}
 *                                                             and {type:"peer"} to the host
 *   client -> server  {type:"offer"|"answer"|"ice"|"bye"}  relayed verbatim to the other peer
 *   server -> client  {type:"peer-left"} | {type:"error", message}
 *
 * Environment:
 *   PORT              listen port        (default 8080)
 *   HOST              bind address       (default 0.0.0.0)
 *   ROOM_TTL_MS       empty room TTL     (default 30 min)
 *   ALLOW_ANY_ORIGIN  skip Origin check  (default 0)
 *   STRICT_ORIGIN     reject on Origin/host mismatch instead of warning (default 0)
 */
"use strict";

const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");

const PORT = Number(process.env.PORT || 8080);
const HOST = process.env.HOST || "0.0.0.0";
const ROOM_TTL_MS = Number(process.env.ROOM_TTL_MS || 30 * 60 * 1000);
const ALLOW_ANY_ORIGIN = process.env.ALLOW_ANY_ORIGIN === "1";
const STRICT_ORIGIN = process.env.STRICT_ORIGIN === "1";
const APP_FILE = path.join(__dirname, "MyDesk.html");
const MAX_FRAME = 256 * 1024;
const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const ROOM_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no 0/O/1/I

let appHtml;
try {
  appHtml = fs.readFileSync(APP_FILE);
} catch (err) {
  console.error(`[fatal] cannot read ${APP_FILE}: ${err.message}`);
  process.exit(1);
}

/* ------------------------------------------------------------------ */
/* Rooms                                                               */
/* ------------------------------------------------------------------ */

/** @type {Map<string, {host: Conn, guest: Conn|null, createdAt: number}>} */
const rooms = new Map();
/** @type {Set<Conn>} */
const conns = new Set();

function newRoomCode() {
  for (;;) {
    let code = "";
    for (let i = 0; i < 6; i += 1) {
      code += ROOM_ALPHABET[crypto.randomInt(ROOM_ALPHABET.length)];
    }
    if (!rooms.has(code)) return code;
  }
}

function peerOf(conn) {
  if (!conn.room) return null;
  const room = rooms.get(conn.room);
  if (!room) return null;
  return room.host === conn ? room.guest : room.host;
}

/** Detach a connection from its room and tell the other side. */
function leaveRoom(conn) {
  const code = conn.room;
  if (!code) return;
  conn.room = null;
  const room = rooms.get(code);
  if (!room) return;
  rooms.delete(code);
  const other = room.host === conn ? room.guest : room.host;
  if (other) {
    other.room = null;
    try { other.send({ type: "peer-left" }); } catch (e) { /* ignore */ }
  }
}

/* ------------------------------------------------------------------ */
/* Minimal WebSocket connection                                        */
/* ------------------------------------------------------------------ */

let nextId = 1;

class Conn {
  constructor(socket) {
    this.id = nextId++;
    this.socket = socket;
    this.buffer = Buffer.alloc(0);
    this.alive = true;
    this.room = null;
    this.role = null;
    this.fragments = [];
    this.fragmentOpcode = 0;
    this.isAlive = true;

    conns.add(this);
    socket.setNoDelay(true);
    socket.on("data", (chunk) => this.onData(chunk));
    socket.on("close", () => this.cleanup());
    socket.on("error", () => this.cleanup());
    socket.on("end", () => this.cleanup());
  }

  get open() {
    return this.alive && !this.socket.destroyed;
  }

  onData(chunk) {
    if (!this.alive) return;
    this.buffer = Buffer.concat([this.buffer, chunk]);
    for (;;) {
      const frame = this.readFrame();
      if (!frame) return;
      this.onFrame(frame);
      if (!this.alive) return;
    }
  }

  readFrame() {
    const buf = this.buffer;
    if (buf.length < 2) return null;

    const fin = (buf[0] & 0x80) !== 0;
    const opcode = buf[0] & 0x0f;
    const masked = (buf[1] & 0x80) !== 0;
    let length = buf[1] & 0x7f;
    let offset = 2;

    if (length === 126) {
      if (buf.length < offset + 2) return null;
      length = buf.readUInt16BE(offset);
      offset += 2;
    } else if (length === 127) {
      if (buf.length < offset + 8) return null;
      const big = buf.readBigUInt64BE(offset);
      if (big > BigInt(MAX_FRAME)) { this.close(1009); return null; }
      length = Number(big);
      offset += 8;
    }

    if (length > MAX_FRAME || !masked) { this.close(1009); return null; }
    if (buf.length < offset + 4) return null;
    const mask = buf.subarray(offset, offset + 4);
    offset += 4;

    if (buf.length < offset + length) return null;
    const payload = Buffer.from(buf.subarray(offset, offset + length));
    for (let i = 0; i < payload.length; i += 1) payload[i] ^= mask[i & 3];

    this.buffer = buf.subarray(offset + length);
    return { fin, opcode, payload };
  }

  onFrame({ fin, opcode, payload }) {
    switch (opcode) {
      case 0x0: { // continuation
        this.fragments.push(payload);
        if (!fin) return;
        const full = Buffer.concat(this.fragments);
        const op = this.fragmentOpcode;
        this.fragments = [];
        this.fragmentOpcode = 0;
        if (op === 0x1) this.onText(full.toString("utf8"));
        return;
      }
      case 0x1: // text
      case 0x2: // binary
        if (!fin) {
          this.fragmentOpcode = opcode;
          this.fragments = [payload];
          return;
        }
        if (opcode === 0x1) this.onText(payload.toString("utf8"));
        return;
      case 0x8:
        this.close(1000);
        return;
      case 0x9:
        this.sendFrame(0xa, payload);
        return;
      case 0xa:
        this.isAlive = true;
        return;
      default:
        this.close(1003);
    }
  }

  onText(text) {
    let msg;
    try {
      msg = JSON.parse(text);
    } catch (e) {
      return;
    }
    if (!msg || typeof msg.type !== "string") return;
    handleMessage(this, msg);
  }

  sendFrame(opcode, payload) {
    if (!this.open) return;
    const len = payload.length;
    let header;
    if (len < 126) {
      header = Buffer.from([0x80 | opcode, len]);
    } else if (len < 65536) {
      header = Buffer.alloc(4);
      header[0] = 0x80 | opcode;
      header[1] = 126;
      header.writeUInt16BE(len, 2);
    } else {
      header = Buffer.alloc(10);
      header[0] = 0x80 | opcode;
      header[1] = 127;
      header.writeBigUInt64BE(BigInt(len), 2);
    }
    try {
      this.socket.write(Buffer.concat([header, payload]));
    } catch (e) {
      this.cleanup();
    }
  }

  send(obj) {
    this.sendFrame(0x1, Buffer.from(JSON.stringify(obj)));
  }

  close(code = 1000) {
    if (!this.open) return;
    const body = Buffer.alloc(2);
    body.writeUInt16BE(code);
    this.sendFrame(0x8, body);
    this.alive = false;
    try { this.socket.end(); } catch (e) { /* ignore */ }
    this.cleanup();
  }

  cleanup() {
    if (!conns.has(this)) return;
    conns.delete(this);
    console.log(`[ws] #${this.id} closed (${conns.size} sockets)`);
    this.alive = false;
    leaveRoom(this);
    try { this.socket.destroy(); } catch (e) { /* ignore */ }
  }
}

/* ------------------------------------------------------------------ */
/* Signaling                                                           */
/* ------------------------------------------------------------------ */

function handleMessage(conn, msg) {
  switch (msg.type) {
    case "create": {
      if (conn.room) return;
      const code = newRoomCode();
      conn.room = code;
      conn.role = "host";
      rooms.set(code, { host: conn, guest: null, createdAt: Date.now() });
      conn.send({ type: "room", code });
      console.log(`[room] ${code} created by #${conn.id}`);
      return;
    }
    case "join": {
      if (conn.room) return;
      const code = String(msg.code || "").trim().toUpperCase();
      const room = rooms.get(code);
      if (!room) return conn.send({ type: "error", message: "房间不存在或已过期" });
      if (room.guest) return conn.send({ type: "error", message: "房间已满" });
      if (!room.host.open) {
        rooms.delete(code);
        return conn.send({ type: "error", message: "房间已失效" });
      }
      conn.room = code;
      conn.role = "guest";
      room.guest = conn;
      conn.send({ type: "joined", code });
      room.host.send({ type: "peer" });
      console.log(`[room] ${code} joined by #${conn.id}`);
      return;
    }
    case "offer":
    case "answer":
    case "ice":
    case "bye": {
      const peer = peerOf(conn);
      if (peer && peer.open) peer.send(msg);
      return;
    }
    default:
      conn.send({ type: "error", message: "unknown message type" });
  }
}

/* ------------------------------------------------------------------ */
/* HTTP + upgrade                                                      */
/* ------------------------------------------------------------------ */

/** 只保留主机名：去掉协议、端口和逗号分隔的代理链 */
function hostOnly(value) {
  const v = String(value || "").split(",")[0].trim();
  if (!v) return "";
  return v.replace(/^[a-z]+:\/\//i, "").split("/")[0].split(":")[0].toLowerCase();
}

/**
 * 经反向代理/隧道时 Host 经常被改写（cloudflared 的 httpHostHeader、带端口、
 * 只保留公网域名等），所以这里只比对主机名，并优先使用 X-Forwarded-Host。
 * 协议本身没有“环境权限”：房间号才是凭证，因此默认只告警，不拒绝。
 */
function originAllowed(req) {
  if (ALLOW_ANY_ORIGIN) return true;
  const origin = req.headers.origin;
  if (!origin) return true;                     // 非浏览器客户端
  let originHost;
  try { originHost = new URL(origin).hostname.toLowerCase(); } catch (e) { return true; }
  const host = hostOnly(req.headers["x-forwarded-host"] || req.headers.host);
  if (!host) return true;
  return originHost === host;
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url || "/", "http://localhost");

  if (url.pathname === "/healthz") {
    const body = Buffer.from(JSON.stringify({
      ok: true,
      rooms: rooms.size,
      sockets: conns.size,
      uptime: Math.round(process.uptime())
    }));
    res.writeHead(200, { "Content-Type": "application/json", "Cache-Control": "no-store" });
    res.end(req.method === "HEAD" ? undefined : body);
    return;
  }

  if (url.pathname === "/ws") {
    res.writeHead(426, {
      "Content-Type": "text/plain; charset=utf-8",
      "Upgrade": "websocket",
      "Connection": "Upgrade"
    });
    res.end("This endpoint only accepts a WebSocket upgrade.\n");
    return;
  }

  if (url.pathname === "/" || url.pathname === "/index.html") {
    res.writeHead(200, {
      "Content-Type": "text/html; charset=utf-8",
      "Content-Length": appHtml.length,
      "Cache-Control": "no-cache",
      "X-Content-Type-Options": "nosniff",
      "Referrer-Policy": "no-referrer"
    });
    res.end(req.method === "HEAD" ? undefined : appHtml);
    return;
  }

  res.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
  res.end("Not found\n");
});

server.on("upgrade", (req, socket, head) => {
  const url = new URL(req.url || "/", "http://localhost");
  const key = req.headers["sec-websocket-key"];

  if (url.pathname !== "/ws" || !key) {
    console.warn(`[ws] rejected upgrade: path=${url.pathname} key=${key ? "ok" : "missing"}`);
    socket.write("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
    socket.destroy();
    return;
  }

  if (!originAllowed(req)) {
    const detail = `origin=${req.headers.origin || "-"} host=${req.headers.host || "-"} xfh=${req.headers["x-forwarded-host"] || "-"}`;
    if (STRICT_ORIGIN) {
      console.warn(`[ws] rejected upgrade (origin mismatch, STRICT_ORIGIN=1): ${detail}`);
      socket.write("HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n");
      socket.destroy();
      return;
    }
    console.warn(`[ws] origin mismatch, allowed (set STRICT_ORIGIN=1 to reject): ${detail}`);
  }

  const accept = crypto.createHash("sha1").update(key + WS_GUID).digest("base64");
  socket.write(
    "HTTP/1.1 101 Switching Protocols\r\n" +
    "Upgrade: websocket\r\n" +
    "Connection: Upgrade\r\n" +
    `Sec-WebSocket-Accept: ${accept}\r\n\r\n`
  );

  if (head && head.length) {
    // Data that arrived with the handshake belongs to the WebSocket stream.
    const conn = new Conn(socket);
    socket.unshift(head);
    console.log(`[ws] #${conn.id} open (${conns.size} sockets)`);
  } else {
    const conn = new Conn(socket);
    console.log(`[ws] #${conn.id} open (${conns.size} sockets)`);
  }
});

server.on("clientError", (err, socket) => {
  if (socket.writable) socket.end("HTTP/1.1 400 Bad Request\r\n\r\n");
});

/* ------------------------------------------------------------------ */
/* Housekeeping                                                        */
/* ------------------------------------------------------------------ */

// Expire rooms where nobody ever joined.
setInterval(() => {
  const now = Date.now();
  for (const [code, room] of rooms) {
    if (!room.guest && now - room.createdAt > ROOM_TTL_MS) {
      rooms.delete(code);
      try { room.host.close(1000); } catch (e) { /* ignore */ }
      console.log(`[room] ${code} expired`);
    }
  }
}, 60_000).unref();

// WebSocket keepalive; browsers answer pings automatically.
setInterval(() => {
  for (const conn of conns) {
    if (!conn.isAlive) {
      conn.close(1001);
      continue;
    }
    conn.isAlive = false;
    conn.sendFrame(0x9, Buffer.alloc(0));
  }
}, 30_000).unref();

/* ------------------------------------------------------------------ */
/* Start / stop                                                        */
/* ------------------------------------------------------------------ */

server.listen(PORT, HOST, () => {
  console.log(`MyDesk signaling server listening on http://${HOST}:${PORT}`);
});

function shutdown(signal) {
  console.log(`[shutdown] ${signal}`);
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(0), 3000).unref();
}
process.on("SIGINT", () => shutdown("SIGINT"));
process.on("SIGTERM", () => shutdown("SIGTERM"));
