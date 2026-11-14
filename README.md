# MyDesk — server mode

A **P2P remote desktop that runs in the browser**: screen sharing plus remote
mouse/keyboard in both directions, with no relay in the media path.

This branch adds an **optional signaling server** so two people no longer have to
copy handshake strings by hand: create a room, send the link, done. The server
only relays SDP and ICE — the video, audio and input still go peer to peer,
encrypted with DTLS-SRTP.

![MyDesk](screenshot.png)

## Two ways to connect

| Mode | When | How |
| --- | --- | --- |
| **Server** | The page is served over `http(s)` | Create a room → share the 6-character code or the link. |
| **Manual** | `file://` or no server | Exchange one handshake string both ways. Works with zero infrastructure. |

Both modes use exactly the same media/control stack; the mode selector in the UI
switches between them.

## Quick start

### Docker (recommended)

```bash
docker build -t mydesk:local .
docker run -d --name mydesk -p 8080:8080 --restart unless-stopped mydesk:local
```

or

```bash
docker compose up -d
```

Then open `http://<host>:8080/` on both machines.

> Screen capture requires a secure context. `localhost` and HTTPS qualify;
> plain `http://192.168.x.x:8080` does **not** — browsers will block
> `getDisplayMedia` there. Put the container behind a TLS reverse proxy
> (Caddy, nginx, Traefik) when using it across the network.

### Node (no dependencies)

```bash
npm start            # or: node server.js
PORT=9000 node server.js
```

Node 20+ is required. There is nothing to `npm install`.

## Environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `PORT` | `8080` | Listen port |
| `HOST` | `0.0.0.0` | Bind address |
| `ROOM_TTL_MS` | `1800000` | How long an unjoined room lives (30 min) |
| `ALLOW_ANY_ORIGIN` | `0` | Set to `1` to skip the WebSocket `Origin` check (only behind a proxy that rewrites `Host`) |

## HTTP surface

| Path | Purpose |
| --- | --- |
| `GET /` | The app (`MyDesk.html`) |
| `GET /healthz` | `{"ok":true,"rooms":N}` — used by the Docker healthcheck |
| `WS /ws` | Signaling socket |

## Signaling protocol

```text
client -> server  {"type":"create"}                        -> {"type":"room","code":"ABC123"}
client -> server  {"type":"join","code":"ABC123"}          -> {"type":"joined"}
                                                            and {"type":"peer"} to the host
client -> server  {"type":"offer"|"answer","sdp":"..."}    relayed verbatim to the other peer
client -> server  {"type":"ice","candidate":{...}}         relayed verbatim
client -> server  {"type":"bye"}                           relayed verbatim
server -> client  {"type":"peer-left"} | {"type":"error","message":"..."}
```

A room holds exactly two peers. The first one creates it and becomes the host
(offerer); the second joins and answers. Rooms are deleted as soon as one side
disconnects, and garbage-collected if nobody ever joins.

## Security notes

- The room code is the only secret: anyone who has it can join a waiting room.
  Codes are 6 characters from a 32-symbol alphabet (`~1e9` combinations) and are
  randomly generated per room, never reused while the process lives.
- The server sees SDP and ICE candidates (which contain IP addresses) but never
  media, input or clipboard content.
- Put it behind TLS in any real deployment; `ws://` signaling is readable on the
  path.
- WebSocket upgrades are rejected when the `Origin` header does not match `Host`
  (disable with `ALLOW_ANY_ORIGIN=1` if a proxy rewrites the host header).
- The process runs as the unprivileged `node` user in the image and exposes one
  port. It holds no state on disk.

## Optional input bridge

Browsers cannot inject OS-level input. Two forms of the same agent are available
from the **系统级操控** panel:

- `bridge/bridge.py` — `pip install pynput` and run it.
- `MyDeskBridge.exe` — packaged with PyInstaller, no Python needed. Download it
  from the [releases page](https://github.com/urarattas/MyDesk/releases); CI builds
  and smoke-tests it on `windows-latest` for every change under `bridge/`.

Releases come from CI: every push to `server` refreshes the rolling
`bridge-latest` release, and a `bridge-v*` tag produces a versioned one. Both ship
`MyDeskBridge.exe` plus its SHA-256, so
`https://github.com/urarattas/MyDesk/releases/latest/download/MyDeskBridge.exe`
always resolves once the workflow has run.

Both listen on `127.0.0.1:8765` only. See [bridge/README.md](bridge/README.md).

Control is engaged by clicking the video: **Esc releases it, clicking again resumes**.
While engaged the page requests a [keyboard lock](https://developer.chrome.com/docs/web-platform/keyboard-lock),
so shortcuts that normally belong to the browser (Ctrl+W, Ctrl+T, F11) are delivered to the
remote machine instead. Keyboard lock needs Chrome/Edge on a secure context; without it those
combinations still act locally. `Ctrl+W`-style tab shortcuts are the main ones a page can never
intercept on its own.

## Development

```bash
npm test        # starts the server, drives two WebSocket clients end to end
```

The test uses only Node built-ins (global `WebSocket` since Node 22), so CI does
not need an install step either.

## CI

`.github/workflows/ci.yml` runs on pushes to `server` and on pull requests:

1. **test** — `npm test` on Node 22.
2. **docker** — builds the image with Buildx, tags it with the branch, the short
   SHA and semver tags, and pushes to `ghcr.io/<owner>/<repo>`
   (`GITHUB_TOKEN`, no extra secret needed).

Images are published for pushes only; pull requests just build.

## Branches

| Branch | Contents |
| --- | --- |
| `main` | Single-file app, no server. |
| `single-file` | Same as `main`. |
| `server` | This branch: signaling server, Docker image, CI. |

## License

No license has been chosen for this repository yet.
