# MyDesk input bridge

The browser page can see the remote screen and receive mouse/keyboard events, but
a web page is not allowed to inject input into the operating system. This small
agent closes that gap: it listens on `127.0.0.1:8765` and applies the events it
receives to the real desktop.

It is **optional**. Without it you can still share screens and watch them; with it
the controlled machine can actually be controlled.

## Run it

**Windows, no Python needed** — download `MyDeskBridge.exe` from the
[releases page](https://github.com/urarattas/MyDesk/releases) and double-click it.
A console window shows the listening address; keep it open while you work.

**Any platform, from source**

```bash
pip install pynput==1.8.2
python bridge.py
```

The bridge prints a **pairing token** at startup:

```text
MyDesk bridge  ->  http://127.0.0.1:8765   backend=pynput   clipboard=win32   screen 2560x1440
  配对令牌（粘贴到页面「系统级操控 → 配对令牌」）: 8f3c…
```

Then, in the MyDesk page on the controlled machine, paste that token into
**系统级操控 → 配对令牌** and press **检测**. The status turns to `已连接` and
clicks/keystrokes are applied.

Pin the token with `BRIDGE_TOKEN=…` if you do not want to re-paste it every time
the bridge restarts. An empty `BRIDGE_TOKEN` disables the check — debugging only.

## Wayland (Linux)

`pynput` talks to X11, so under Wayland it only reaches XWayland windows — native
Wayland apps never see the events. The bridge detects `WAYLAND_DISPLAY` and
switches to **ydotool**, which injects through `/dev/uinput` and therefore works in
any compositor.

```bash
# Debian / Ubuntu
sudo apt install ydotool
sudo systemctl enable --now ydotool        # 或: systemctl --user enable --now ydotoold

# Arch
sudo pacman -S ydotool && sudo systemctl enable --now ydotool

# Fedora
sudo dnf install ydotool && sudo systemctl enable --now ydotool
```

The daemon needs access to `/dev/uinput`. If it runs as your user, allow it once:

```bash
echo 'KERNEL=="uinput", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"' \
  | sudo tee /etc/udev/rules.d/80-uinput.rules
sudo udevadm control --reload && sudo udevadm trigger
sudo usermod -aG input "$USER"      # 重新登录生效
```

`/healthz` reports the active backend, and the page shows it next to the screen
size (`已连接 · 2560×1600 · ydotool`). Force one with `BRIDGE_BACKEND=pynput` or
`BRIDGE_BACKEND=ydotool`.

**Known limit:** ydotool 1.0.4 has no wheel command, so scrolling does not work on
that backend (clicks, movement and the keyboard do). On Wayland the bridge also
reads the real mode from `/sys/class/drm` instead of the scaled XWayland size, so
fractional scaling does not offset the cursor.

## Security

Binding `127.0.0.1` is **not** the protection it looks like: the browser runs on
the same machine, so every web page you open can reach `127.0.0.1:8765`. What
actually keeps other people out:

- **Pairing token.** `POST /input`, `POST /clipboard` and `GET /clipboard` all
  require `X-MyDesk-Token` matching the token printed at startup. A page that does
  not have it gets `401` and cannot type a single key.
- **Host allowlist.** The `Host` header must be a loopback name. After a DNS
  rebinding attack the browser sends the attacker's domain there, so the request
  is refused with `403` even though it really did arrive at 127.0.0.1.
- **CORS echoes instead of `*`.** The response mirrors the request `Origin`
  (including `null`, so a `file://` page still works) and only allows the
  `X-MyDesk-Token` header. There is no blanket wildcard any more, and
  `Access-Control-Allow-Private-Network` is only granted on those replies.
- **Bodies over 1 MiB are refused** with `413`.

Remaining trust boundaries, unchanged and on purpose:

- Any local process running as your user can read the token from its own console
  or from `BRIDGE_TOKEN` and then post events. That is the same boundary as a
  local keyboard API; do not run it on a machine you share with untrusted local
  accounts.
- The token is a bearer secret with no expiry: treat it like a password, and do
  not paste it into a page served from a machine you do not control.

## Windows notes

- **SmartScreen / antivirus.** The binary is unsigned, so the first run shows
  "Windows protected your PC" → *More info* → *Run anyway*. Some scanners flag
  one-file PyInstaller builds heuristically; the workflow publishes a SHA-256
  next to the exe so you can verify the download.
- **DPI.** The bridge declares per-monitor DPI awareness before reading the screen
  size. Without that, on a scaled display (125 %/150 %) the cursor would only
  reach the top-left part of the screen.
- **Multiple monitors.** Coordinates are normalised to the primary display, so
  the bridge maps them there. Sharing a secondary monitor will be offset.

## Build the exe yourself

```powershell
powershell -ExecutionPolicy Bypass -File bridge\build.ps1
```

or directly:

```powershell
pip install -r requirements.txt pyinstaller
pyinstaller --noconfirm --clean --onefile --name MyDeskBridge bridge.py
```

CI does the same on `windows-latest` (`.github/workflows/bridge.yml`) and attaches
the result to the release when a `bridge-v*` tag is pushed.

## HTTP surface

| Method | Path | Auth | Meaning |
| --- | --- | --- | --- |
| `GET` | `/healthz` | none / token | `{"ok":true,"auth":"token"}`; with a valid token it also carries `screen`, `backend`, `clipboard`, `error` |
| `POST` | `/input` | token | `{"events":[...]}` — mouse/keyboard events, `204` on success |
| `GET` | `/clipboard` | token | `{"ok":true,"text":"…"}` — system clipboard as plain text |
| `POST` | `/clipboard` | token | `{"text":"…"}` — overwrite the system clipboard, `204` on success |
| `OPTIONS` | any | — | CORS preflight (`X-MyDesk-Token` is an allowed request header) |

Auth column `token` means `X-MyDesk-Token: <token>` is required. Missing or wrong
token → `401`; non-loopback `Host` → `403`; body over 1 MiB → `413`.

Related environment variables: `BRIDGE_TOKEN` (pin the token), `BRIDGE_HOSTS`
(extra allowed `Host` names, comma separated — for people who proxy the bridge),
plus the existing `BRIDGE_PORT` and `BRIDGE_BACKEND`.

Event format (all coordinates normalised to `0..1`):

```json
{"t":"m","x":0.42,"y":0.51,"b":0}
{"t":"mb","d":1,"b":0,"x":0.42,"y":0.51}
{"t":"w","dy":-3}
{"t":"k","d":"down","key":"a","code":"KeyA","mods":1}
{"t":"t","s":"pasted text"}
{"t":"release"}
```
