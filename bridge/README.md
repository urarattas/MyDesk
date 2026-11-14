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
pip install pynput
python bridge.py
```

Then, in the MyDesk page on the controlled machine, open **系统级操控** and press
**检测**. The status turns to `已连接` and clicks/keystrokes are applied.

## Security

- Binds `127.0.0.1` only. Nothing outside the machine can reach it, and it is not
  a general-purpose remote-control daemon: it only consumes the event format the
  page sends.
- CORS is `*` on purpose — the page may be opened from `file://` (opaque origin).
  Combined with the loopback binding this keeps the attack surface local.
- Any process running as your user can also post events to it. That is the same
  trust boundary as a local keyboard API; do not run it on a machine you share
  with untrusted local accounts.

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

| Method | Path | Meaning |
| --- | --- | --- |
| `GET` | `/healthz` | `{"ok":true,"screen":[width,height]}` |
| `POST` | `/input` | `{"events":[...]}` — mouse/keyboard events, `204` on success |
| `OPTIONS` | any | CORS preflight (including `Access-Control-Allow-Private-Network`) |

Event format (all coordinates normalised to `0..1`):

```json
{"t":"m","x":0.42,"y":0.51,"b":0}
{"t":"mb","d":1,"b":0,"x":0.42,"y":0.51}
{"t":"w","dy":-3}
{"t":"k","d":"down","key":"a","code":"KeyA","mods":1}
{"t":"t","s":"pasted text"}
{"t":"release"}
```
