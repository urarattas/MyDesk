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
// 额外放行的来源（逗号分隔，形如 http://tauri.localhost）。
// 桌面客户端跑在 WebView 里，页面来源是 tauri.localhost，和服务器 origin 天然不同；
// STRICT_ORIGIN=1 时需要在服务器上显式信任它。
const TRUSTED_ORIGINS = (process.env.TRUSTED_ORIGINS || "")
  .split(",")
  .map((s) => s.trim().toLowerCase().replace(/\/+$/, ""))
  .filter(Boolean);
const APP_FILE = process.env.MYDESK_HTML || path.join(__dirname, "MyDesk.html");
const MAX_FRAME = 256 * 1024;              // 单帧上限
const MAX_MESSAGE = 512 * 1024;            // 单条消息（含全部续帧）上限
const MAX_BUFFERED = MAX_FRAME + 14;       // 未成形帧允许占用的最大接收缓存
const MAX_CONNS = Number(process.env.MAX_CONNS || 512);        // 0 = 不限
const MAX_ROOMS = Number(process.env.MAX_ROOMS || 256);        // 0 = 不限
const JOIN_RATE = Number(process.env.JOIN_RATE || 40);         // 全机 join 令牌/秒
const JOIN_FAIL_BURST = Number(process.env.JOIN_FAIL_BURST || 12);
const JOIN_WINDOW_MS = 60 * 1000;
const JOIN_COOLDOWN_MS = 5 * 60 * 1000;
const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const ROOM_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no 0/O/1/I
const ROOM_LEN = 8;                                        // 32^8 ≈ 1.1e12

let appHtml = null;
let appMtime = 0;

/**
 * 每次请求按 mtime 判断要不要重读：把 HTML 以卷挂载进来后，改完文件立刻生效，
 * 既不用重建镜像也不用重启容器。
 */
function loadApp() {
  try {
    const st = fs.statSync(APP_FILE);
    if (!appHtml || st.mtimeMs !== appMtime) {
      appHtml = fs.readFileSync(APP_FILE);
      appMtime = st.mtimeMs;
      console.log(`[app] serving ${APP_FILE} (${appHtml.length} bytes)`);
    }
    return appHtml;
  } catch (err) {
    return null;
  }
}

loadApp();

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
    for (let i = 0; i < ROOM_LEN; i += 1) {
      code += ROOM_ALPHABET[crypto.randomInt(ROOM_ALPHABET.length)];
    }
    if (!rooms.has(code)) return code;
  }
}

/**
 * 全机 join 令牌桶。房间码有 32^8 ≈ 1.1e12 种，如果没有限制，
 * 攻击者开几十条连接就能在十几分钟内撞进某个正在等待的房间；
 * 这里把整个进程的猜测速率压到 JOIN_RATE/s（默认 40，可配）。
 */
let joinTokens = JOIN_RATE;
let joinTokenAt = Date.now();
function takeJoinToken() {
  if (!(JOIN_RATE > 0)) return true;
  const now = Date.now();
  if (now > joinTokenAt) {
    joinTokens = Math.min(JOIN_RATE, joinTokens + ((now - joinTokenAt) / 1000) * JOIN_RATE);
    joinTokenAt = now;
  }
  if (joinTokens < 1) return false;
  joinTokens -= 1;
  return true;
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
    this.acc = Buffer.allocUnsafe(4096);   // 接收缓存（off..accLen 为未消费区间）
    this.accLen = 0;
    this.off = 0;
    this.alive = true;
    this.room = null;
    this.role = null;
    this.fragments = [];
    this.fragmentBytes = 0;
    this.fragmentOpcode = 0;
    this.isAlive = true;
    this.joinFails = 0;
    this.joinWindowStart = 0;
    this.joinBlockedUntil = 0;

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

  /** 单连接 join 失败配额：窗口内失败太多次就冷却一段时间。 */
  allowJoinAttempt() {
    const now = Date.now();
    if (this.joinBlockedUntil > now) return false;
    if (now - this.joinWindowStart > JOIN_WINDOW_MS) {
      this.joinWindowStart = now;
      this.joinFails = 0;
    }
    this.joinFails += 1;
    if (this.joinFails > JOIN_FAIL_BURST) {
      this.joinBlockedUntil = now + JOIN_COOLDOWN_MS;
      this.joinFails = 0;
      console.warn(`[ws] #${this.id} join 尝试过于频繁，冷却 ${Math.round(JOIN_COOLDOWN_MS / 1000)}s`);
      return false;
    }
    return true;
  }

  /**
   * 追加到接收缓存。扩容前先把已消费的前缀压掉，只在缓存真不够时才增大，
   * 避免旧实现「每来一个 chunk 就 Buffer.concat 整个积压」的 O(n²) 拷贝。
   */
  appendChunk(chunk) {
    const need = this.accLen + chunk.length;
    if (need > this.acc.length) {
      this.compact();
      if (this.accLen + chunk.length > this.acc.length) {
        let cap = this.acc.length;
        while (cap < this.accLen + chunk.length) cap *= 2;
        if (cap > MAX_BUFFERED) cap = MAX_BUFFERED;
        if (cap < this.accLen + chunk.length) { this.close(1009); return; }
        const next = Buffer.allocUnsafe(cap);
        this.acc.copy(next, 0, this.off, this.accLen);
        this.accLen -= this.off;
        this.off = 0;
        this.acc = next;
      }
    }
    chunk.copy(this.acc, this.accLen);
    this.accLen += chunk.length;
  }

  /** 把 [off, accLen) 挪到开头 */
  compact() {
    if (this.off === 0) return;
    if (this.off === this.accLen) { this.off = 0; this.accLen = 0; return; }
    this.acc.copy(this.acc, 0, this.off, this.accLen);
    this.accLen -= this.off;
    this.off = 0;
  }

  onData(chunk) {
    if (!this.alive) return;
    this.appendChunk(chunk);
    while (this.alive) {
      const frame = this.readFrame();
      if (!frame) return;
      this.onFrame(frame);
    }
  }

  readFrame() {
    const buf = this.acc;
    const start = this.off;
    const avail = this.accLen - start;
    if (avail < 2) return null;

    const fin = (buf[start] & 0x80) !== 0;
    const rsv = buf[start] & 0x70;
    // 没有协商任何扩展（permessage-deflate 也没开），RSV1-3 置位就是协议错误
    if (rsv) { this.close(1002); return null; }
    const opcode = buf[start] & 0x0f;
    const masked = (buf[start + 1] & 0x80) !== 0;
    let length = buf[start + 1] & 0x7f;
    let header = 2;

    if (length === 126) {
      if (avail < 4) return null;
      length = buf.readUInt16BE(start + 2);
      header = 4;
    } else if (length === 127) {
      if (avail < 10) return null;
      const big = buf.readBigUInt64BE(start + 2);
      if (big > BigInt(MAX_FRAME)) { this.close(1009); return null; }
      length = Number(big);
      header = 10;
    }

    if (length > MAX_FRAME || !masked) { this.close(1009); return null; }
    if (avail < header + 4 + length) return null;

    const maskAt = start + header;
    const bodyAt = maskAt + 4;
    const payload = Buffer.allocUnsafe(length);
    for (let i = 0; i < length; i += 1) payload[i] = buf[bodyAt + i] ^ buf[maskAt + (i & 3)];

    this.off = start + header + 4 + length;
    if (this.off === this.accLen) { this.off = 0; this.accLen = 0; }
    return { fin, opcode, payload };
  }

  onFrame({ fin, opcode, payload }) {
    // 控制帧不允许分片，长度也必须 ≤ 125（RFC 6455 §5.5）
    if (opcode >= 0x8 && (!fin || payload.length > 125)) { this.close(1002); return; }
    switch (opcode) {
      case 0x0: { // continuation
        this.fragments.push(payload);
        this.fragmentBytes += payload.length;
        // 单帧受 MAX_FRAME 约束，但续帧数量没有天然上限，必须自己封顶，
        // 否则「永不 FIN 的续帧流」可以无限吃内存。
        if (this.fragmentBytes > MAX_MESSAGE) { this.close(1009); return; }
        if (!fin) return;
        const full = Buffer.concat(this.fragments);
        const op = this.fragmentOpcode;
        this.fragments = [];
        this.fragmentBytes = 0;
        this.fragmentOpcode = 0;
        if (op === 0x1) this.onText(full.toString("utf8"));
        return;
      }
      case 0x1: // text
      case 0x2: // binary
        if (!fin) {
          this.fragmentOpcode = opcode;
          this.fragments = [payload];
          this.fragmentBytes = payload.length;
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
      if (MAX_ROOMS && rooms.size >= MAX_ROOMS) {
        return conn.send({ type: "error", message: "服务器繁忙，请稍后再试" });
      }
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
      if (!takeJoinToken() || !conn.allowJoinAttempt()) {
        return conn.send({ type: "error", message: "尝试过于频繁，请稍后再试" });
      }
      const code = String(msg.code || "").trim().toUpperCase();
      if (code.length !== ROOM_LEN) return conn.send({ type: "error", message: "房间号格式不正确" });
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
  if (TRUSTED_ORIGINS.includes(String(origin).trim().toLowerCase().replace(/\/+$/, ""))) return true;
  let originHost;
  try { originHost = new URL(origin).hostname.toLowerCase(); } catch (e) { return true; }
  const host = hostOnly(req.headers["x-forwarded-host"] || req.headers.host);
  if (!host) return true;
  return originHost === host;
}

/**
 * 浏览器一定会带 Sec-Fetch-Site，页面脚本无法伪造。跨站页面开 WebSocket
 * 拿到的值不是 same-origin —— 直接拒绝「跨站 WebSocket 劫持」，而且不受
 * 反向代理改写 Host/Origin 的影响（这正是 Origin 比对无法做到的部分）。
 * 非浏览器客户端不带这个头，保持原有行为。
 */
function crossSiteUpgrade(req) {
  if (ALLOW_ANY_ORIGIN) return false;
  const origin = req.headers.origin;
  if (origin && TRUSTED_ORIGINS.includes(String(origin).trim().toLowerCase().replace(/\/+$/, ""))) {
    return false;   // 显式信任的桌面客户端来源
  }
  const site = req.headers["sec-fetch-site"];
  if (!site) return false;
  return site !== "same-origin" && site !== "none";
}

/**
 * 只解析一次 request-target。畸形目标（例如 "GET //[ HTTP/1.1"）会让
 * new URL() 抛 ERR_INVALID_URL，而抛在 request/upgrade 监听器里等于
 * 未捕获异常 —— 一个数据包就能打掉整个进程。这里统一退化为 400。
 */
function parseUrl(req) {
  try {
    return new URL(req.url || "/", "http://localhost");
  } catch (e) {
    return null;
  }
}

const server = http.createServer((req, res) => {
  const url = parseUrl(req);
  if (!url) {
    res.writeHead(400, { "Content-Type": "text/plain; charset=utf-8", "Connection": "close" });
    res.end("Bad request target\n");
    return;
  }

  if (url.pathname === "/healthz") {
    const info = { ok: true };
    // 房间数/连接数默认不暴露：公网部署时它们只是免费的侦察信息。
    // 需要排障就设 HEALTHZ_DETAIL=1。
    if (process.env.HEALTHZ_DETAIL === "1") {
      info.rooms = rooms.size;
      info.sockets = conns.size;
      info.uptime = Math.round(process.uptime());
    }
    const body = Buffer.from(JSON.stringify(info));
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

  if (url.pathname === "/" || url.pathname === "/index.html" || url.pathname === "/MyDesk.html") {
    const html = loadApp();
    if (!html) {
      res.writeHead(503, { "Content-Type": "text/plain; charset=utf-8" });
      res.end(`Cannot read ${APP_FILE}. If this is a bind mount, check that the path is a file, not a directory.\n`);
      return;
    }
    res.writeHead(200, {
      "Content-Type": "text/html; charset=utf-8",
      "Content-Length": html.length,
      "Cache-Control": "no-cache",
      "X-Content-Type-Options": "nosniff",
      "Referrer-Policy": "no-referrer",
      // 远程控制界面不能被别人套 iframe：否则「允许对方控制我」可以被
      // 透明覆盖层点掉。这里只收窄注入面，不限制内联脚本/样式。
      "X-Frame-Options": "DENY",
      "Content-Security-Policy": "frame-ancestors 'none'; object-src 'none'; base-uri 'none'"
    });
    res.end(req.method === "HEAD" ? undefined : html);
    return;
  }

  res.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
  res.end("Not found\n");
});

server.on("upgrade", (req, socket, head) => {
  const url = parseUrl(req);
  const key = req.headers["sec-websocket-key"];

  if (!url || url.pathname !== "/ws" || !key) {
    console.warn(`[ws] rejected upgrade: path=${url.pathname} key=${key ? "ok" : "missing"}`);
    socket.write("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
    socket.destroy();
    return;
  }

  if (MAX_CONNS && conns.size >= MAX_CONNS) {
    console.warn(`[ws] rejected upgrade: connection limit ${MAX_CONNS} reached`);
    socket.write("HTTP/1.1 503 Service Unavailable\r\nRetry-After: 5\r\nConnection: close\r\n\r\n");
    socket.destroy();
    return;
  }

  if (crossSiteUpgrade(req)) {
    console.warn(`[ws] rejected cross-site upgrade: sec-fetch-site=${req.headers["sec-fetch-site"]} origin=${req.headers.origin || "-"}`);
    socket.write("HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n");
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
  if (!appHtml) console.warn(`[app] ${APP_FILE} is not readable yet; requests answer 503`);
});

function shutdown(signal) {
  console.log(`[shutdown] ${signal}`);
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(0), 3000).unref();
}
process.on("SIGINT", () => shutdown("SIGINT"));
process.on("SIGTERM", () => shutdown("SIGTERM"));

// 兜底：房间状态全在内存里，进程崩一次所有会话就没了。正常路径已逐处
// try/catch，这里只负责记录并继续服务，避免单个请求/帧带走整个实例。
const EXIT_ON_UNCAUGHT = process.env.EXIT_ON_UNCAUGHT === "1";
process.on("uncaughtException", (err) => {
  console.error("[fatal] uncaught exception:", err && err.stack ? err.stack : err);
  if (EXIT_ON_UNCAUGHT) shutdown("uncaughtException");
});
process.on("unhandledRejection", (reason) => {
  console.error("[fatal] unhandled rejection:", reason && reason.stack ? reason.stack : reason);
  if (EXIT_ON_UNCAUGHT) shutdown("unhandledRejection");
});
