# MyDesk

A **single-file, serverless P2P remote desktop** that runs in the browser.

`MyDesk.html` is the whole app — about 100 KB, no build step, no dependencies, no
signaling server. Open the same file on two machines, exchange one handshake string,
and you get screen sharing plus remote mouse/keyboard in both directions.

![MyDesk](screenshot.png)

## Features

- **Bidirectional** — either side can share its screen and control the other at the same time.
- **No server** — manual SDP exchange; nothing to deploy, nothing in the middle.
- **End-to-end encrypted** — DTLS-SRTP, peer to peer only.
- **Runtime codec switching** — VP8 / H.264 / VP9 / AV1 are negotiated up front (four
  video m-lines), switching only moves the track between transceivers.
- **Works offline** — LAN-only mode skips STUN entirely.
- **Chinese UI.**

## Quick start

1. Copy `MyDesk.html` to both machines and open it.
2. A: **① 创建邀请码** → copy the code (about 2.9k characters) → send it to B.
3. B: **② 接受邀请** → paste → generate the reply code → send it back.
4. A: paste the reply → **完成连接**.
5. Either side clicks **共享屏幕**. Remote control requires the other side to allow it.

## Branches

| Branch | Contents |
| --- | --- |
| `main` | The single-file app (same tree as `single-file`). |
| `single-file` | The zero-infrastructure variant. |
| `server` | Same app plus an optional signaling server, a Docker image and CI. |

## How it works

- WebRTC with one `sendrecv` audio m-line and four video m-lines, each pinned to a codec
  through `setCodecPreferences()`.
- Switching codec calls `replaceTrack()` between transceivers; no renegotiation, the
  signaling state stays `stable`.
- Two data channels: reliable `ctl` (permissions, ping, state) and unreliable `in`
  (mouse/keyboard, unordered, no retransmits).
- The handshake string is the local SDP compressed with `deflate-raw` and base64url encoded.

## Known limitations

- A browser cannot inject OS-level input. An optional local bridge — `bridge/bridge.py`
  or the packaged `MyDeskBridge.exe` — applies remote input to the real desktop. Both
  are handed out from the **系统级操控** panel and listen on `127.0.0.1:8765` only.
- Symmetric NAT on both ends cannot be traversed without a relay. Add TURN under
  **高级：自定义 STUN / TURN**, or put both machines on a virtual LAN (e.g. EasyTier —
  the UI ships a small STUN server for that case).
- The default STUN list targets networks in China; run **检测可用性** to see what your
  network can actually reach.
- Screen capture needs a secure context: `file://`, `localhost` or HTTPS.

## Requirements

Chrome / Edge / Firefox, current versions. No runtime dependencies.
