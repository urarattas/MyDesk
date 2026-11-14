#!/usr/bin/env python3
# MyDesk bridge - 把浏览器收到的键鼠事件注入本机系统
#
# 后端（自动选择，可用 BRIDGE_BACKEND=pynput|ydotool|none 强制）：
#   pynput   X11 / Windows / macOS —— 走 XTEST / SendInput
#   ydotool  Linux Wayland —— 走 /dev/uinput，能注入原生 Wayland 窗口
#
# 仅监听 127.0.0.1:8765。
import glob
import json
import os
import shutil
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("BRIDGE_PORT", "8765"))
_REQ_BACKEND = os.environ.get("BRIDGE_BACKEND", "auto").strip().lower()


# --------------------------------------------------------------------------
# 屏幕尺寸
# --------------------------------------------------------------------------

def _drm_size():
    """Wayland 下 XWayland 报的是缩放后的逻辑分辨率，而 uinput 注入用的是
    原生像素。从 DRM 连接器读首选模式，才是真实分辨率。"""
    best = (0, 0)
    for status in glob.glob("/sys/class/drm/card*-*/status"):
        try:
            with open(status) as f:
                if f.read().strip() != "connected":
                    continue
            with open(os.path.join(os.path.dirname(status), "modes")) as f:
                first = f.readline().strip()
        except OSError:
            continue
        if "x" not in first:
            continue
        try:
            w, h = (int(x) for x in first.split("x")[:2])
        except ValueError:
            continue
        if w * h > best[0] * best[1]:
            best = (w, h)
    return best


def _x11_size():
    try:
        import tkinter as tk
        root = tk.Tk()
        root.withdraw()
        size = (root.winfo_screenwidth(), root.winfo_screenheight())
        root.destroy()
        return size
    except Exception:
        return (0, 0)


def screen_size():
    if sys.platform == "win32":
        try:
            import ctypes
            try:
                ctypes.windll.shcore.SetProcessDpiAwareness(2)
            except Exception:
                ctypes.windll.user32.SetProcessDPIAware()
            u = ctypes.windll.user32
            return u.GetSystemMetrics(0), u.GetSystemMetrics(1)
        except Exception:
            pass

    if sys.platform.startswith("linux") and os.environ.get("WAYLAND_DISPLAY"):
        size = _drm_size()
        if size[0] and size[1]:
            return size

    size = _x11_size()
    if size[0] and size[1]:
        return size
    return (1920, 1080)


SW, SH = screen_size()


# --------------------------------------------------------------------------
# 后端
# --------------------------------------------------------------------------

class Backend:
    name = "none"
    error = ""

    def move(self, x, y):
        pass

    def button(self, index, down):
        pass

    def scroll(self, dy):
        pass

    def key(self, down, code, ch, mods):
        pass

    def release_all(self):
        pass

    def close(self):
        pass


class PynputBackend(Backend):
    """X11 / Windows / macOS。pynput 在 Wayland 下只能注入 XWayland 窗口。"""

    name = "pynput"

    def __init__(self):
        from pynput.mouse import Controller as MouseCtl, Button
        from pynput.keyboard import Controller as KeyCtl, Key, KeyCode

        self._Key = Key
        self._KeyCode = KeyCode
        self.mouse = MouseCtl()
        self.kb = KeyCtl()
        self.btn = {0: Button.left, 1: Button.middle, 2: Button.right}
        self.mods = [(1, Key.ctrl), (2, Key.shift), (4, Key.alt), (8, Key.cmd)]
        self.special = {
            "Escape": Key.esc, "Enter": Key.enter, "NumpadEnter": Key.enter,
            "Backspace": Key.backspace, "Tab": Key.tab, "Delete": Key.delete,
            "ArrowUp": Key.up, "ArrowDown": Key.down, "ArrowLeft": Key.left,
            "ArrowRight": Key.right, "Home": Key.home, "End": Key.end,
            "PageUp": Key.page_up, "PageDown": Key.page_down, "Insert": Key.insert,
            "CapsLock": Key.caps_lock, "Space": Key.space,
        }
        for i in range(1, 13):
            self.special["F" + str(i)] = getattr(Key, "f" + str(i))
        extra = {
            "PrintScreen": "print_screen", "ScrollLock": "scroll_lock", "Pause": "pause",
            "ContextMenu": "menu",
            "AudioVolumeMute": "media_volume_mute", "AudioVolumeDown": "media_volume_down",
            "AudioVolumeUp": "media_volume_up", "MediaPlayPause": "media_play_pause",
            "MediaStop": "media_stop", "MediaTrackNext": "media_next",
            "MediaTrackPrevious": "media_previous",
            "BrowserBack": "browser_back", "BrowserForward": "browser_forward",
            "BrowserRefresh": "browser_refresh", "BrowserHome": "browser_home",
            "BrowserSearch": "browser_search", "BrowserFavorites": "browser_favorites",
        }
        for code, attr in extra.items():
            key = getattr(Key, attr, None)
            if key is not None:
                self.special[code] = key

        self.held_mods = set()
        self.held_by_code = {}

    def move(self, x, y):
        try:
            self.mouse.position = (int(x * SW), int(y * SH))
        except Exception:
            pass

    def button(self, index, down):
        try:
            b = self.btn.get(index)
            if b is None:
                return
            (self.mouse.press if down else self.mouse.release)(b)
        except Exception:
            pass

    def scroll(self, dy):
        try:
            self.mouse.scroll(0, -int(dy))
        except Exception:
            pass

    def _resolve(self, code, ch):
        target = self.special.get(code)
        if target is not None:
            return target, False
        if len(ch) == 1 and ch != "Dead":
            try:
                if ch.isalpha():
                    return self._KeyCode.from_char(ch.lower()), False
                return self._KeyCode.from_char(ch), True
            except Exception:
                return None, False
        return None, False

    def _sync_mods(self, mods, skip_shift):
        for bit, mk in self.mods:
            want = bool(mods & bit) and not (bit == 2 and skip_shift)
            if want and mk not in self.held_mods:
                try:
                    self.kb.press(mk)
                    self.held_mods.add(mk)
                except Exception:
                    pass
            elif not want and mk in self.held_mods:
                try:
                    self.kb.release(mk)
                    self.held_mods.discard(mk)
                except Exception:
                    pass

    def key(self, down, code, ch, mods):
        target, skip_shift = self._resolve(code, ch)
        self._sync_mods(mods, skip_shift)
        if down:
            if target is None or code in self.held_by_code:
                return
            try:
                self.kb.press(target)
                self.held_by_code[code] = target
            except Exception:
                pass
        else:
            target = self.held_by_code.pop(code, target)
            if target is None:
                return
            try:
                self.kb.release(target)
            except Exception:
                pass

    def release_all(self):
        for target in list(self.held_by_code.values()):
            try:
                self.kb.release(target)
            except Exception:
                pass
        self.held_by_code.clear()
        for mk in list(self.held_mods):
            try:
                self.kb.release(mk)
            except Exception:
                pass
        self.held_mods.clear()


class YdotoolBackend(Backend):
    """Linux Wayland（以及任何有 /dev/uinput 的会话）。

    ydotool 1.x 只提供 click / key / mousemove / type —— 没有滚轮，
    所以 scroll() 是空实现，启动时会提示。
    """

    name = "ydotool"

    # 浏览器 KeyboardEvent.code -> Linux evdev keycode
    KEYS = {
        "Escape": 1, "Digit1": 2, "Digit2": 3, "Digit3": 4, "Digit4": 5,
        "Digit5": 6, "Digit6": 7, "Digit7": 8, "Digit8": 9, "Digit9": 10,
        "Digit0": 11, "Minus": 12, "Equal": 13, "Backspace": 14, "Tab": 15,
        "KeyQ": 16, "KeyW": 17, "KeyE": 18, "KeyR": 19, "KeyT": 20, "KeyY": 21,
        "KeyU": 22, "KeyI": 23, "KeyO": 24, "KeyP": 25,
        "BracketLeft": 26, "BracketRight": 27, "Enter": 28, "ControlLeft": 29,
        "KeyA": 30, "KeyS": 31, "KeyD": 32, "KeyF": 33, "KeyG": 34, "KeyH": 35,
        "KeyJ": 36, "KeyK": 37, "KeyL": 38, "Semicolon": 39, "Quote": 40,
        "Backquote": 41, "ShiftLeft": 42, "Backslash": 43,
        "KeyZ": 44, "KeyX": 45, "KeyC": 46, "KeyV": 47, "KeyB": 48, "KeyN": 49,
        "KeyM": 50, "Comma": 51, "Period": 52, "Slash": 53, "ShiftRight": 54,
        "NumpadMultiply": 55, "AltLeft": 56, "Space": 57, "CapsLock": 58,
        "F1": 59, "F2": 60, "F3": 61, "F4": 62, "F5": 63, "F6": 64, "F7": 65,
        "F8": 66, "F9": 67, "F10": 68, "NumLock": 69, "ScrollLock": 70,
        "Numpad7": 71, "Numpad8": 72, "Numpad9": 73, "NumpadSubtract": 74,
        "Numpad4": 75, "Numpad5": 76, "Numpad6": 77, "NumpadAdd": 78,
        "Numpad1": 79, "Numpad2": 80, "Numpad3": 81, "Numpad0": 82,
        "NumpadDecimal": 83, "F11": 87, "F12": 88, "NumpadEnter": 96,
        "ControlRight": 97, "NumpadDivide": 98, "PrintScreen": 99,
        "AltRight": 100, "Home": 102, "ArrowUp": 103, "PageUp": 104,
        "ArrowLeft": 105, "ArrowRight": 106, "End": 107, "ArrowDown": 108,
        "PageDown": 109, "Insert": 110, "Delete": 111,
        "AudioVolumeMute": 113, "AudioVolumeDown": 114, "AudioVolumeUp": 115,
        "Pause": 119, "MetaLeft": 125, "MetaRight": 126, "ContextMenu": 127,
    }
    MODS = [(1, 29), (2, 42), (4, 56), (8, 125)]      # ctrl shift alt meta

    def __init__(self):
        self.bin = shutil.which("ydotool")
        if not self.bin:
            raise RuntimeError("ydotool not found")
        self.held_mods = set()
        self.held_keys = set()
        self._warned = set()

    def _run(self, *args, quiet=False):
        try:
            res = subprocess.run(
                [self.bin, *args],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                timeout=3,
            )
            if res.returncode != 0 and not quiet:
                msg = res.stderr.decode("utf-8", "replace").strip() or ("exit %d" % res.returncode)
                self.error = msg
                if msg not in self._warned:
                    self._warned.add(msg)
                    print("ydotool failed: " + msg, file=sys.stderr)
        except Exception as exc:
            self.error = str(exc)
            if str(exc) not in self._warned:
                self._warned.add(str(exc))
                print("ydotool failed: %s" % exc, file=sys.stderr)

    def move(self, x, y):
        self._run("mousemove", "--absolute",
                  "-x", str(int(x * SW)), "-y", str(int(y * SH)), quiet=True)

    def button(self, index, down):
        # 0xC0 = press|release|left；按下=0x40|idx，抬起=0x80|idx
        code = (0x40 if down else 0x80) | (index & 0x03)
        self._run("click", str(code))

    def scroll(self, dy):
        # ydotool 1.x 没有滚轮命令
        if "scroll" not in self._warned:
            self._warned.add("scroll")
            print("ydotool 1.x has no wheel command; scrolling is ignored", file=sys.stderr)

    def key(self, down, code, ch, mods):
        kc = self.KEYS.get(code)
        is_modifier_key = kc in (29, 42, 56, 125)
        specs = []
        # 修饰键完全由 mods 位管理：按下 Ctrl 时浏览器会给 ctrlKey=True，
        # 松开时给 False，不需要再单独跟踪 ControlLeft 的 keyup。
        for bit, mk in self.MODS:
            want = bool(mods & bit)
            if want and mk not in self.held_mods:
                specs.append("%d:1" % mk)
                self.held_mods.add(mk)
            elif not want and mk in self.held_mods:
                specs.append("%d:0" % mk)
                self.held_mods.discard(mk)
        if kc is not None and not is_modifier_key:
            if down and kc not in self.held_keys:
                specs.append("%d:1" % kc)
                self.held_keys.add(kc)
            elif not down and kc in self.held_keys:
                specs.append("%d:0" % kc)
                self.held_keys.discard(kc)
        if specs:
            self._run("key", *specs)

    def release_all(self):
        specs = ["%d:0" % k for k in self.held_keys]
        specs += ["%d:0" % k for k in self.held_mods]
        self.held_keys.clear()
        self.held_mods.clear()
        if specs:
            self._run("key", *specs)


def pick_backend():
    forced = _REQ_BACKEND
    if forced == "none":
        return Backend()

    want = []
    if forced in ("ydotool", "pynput"):
        want = [forced]
    elif sys.platform.startswith("linux") and os.environ.get("WAYLAND_DISPLAY"):
        want = ["ydotool", "pynput"]          # Wayland 优先 uinput
    else:
        want = ["pynput", "ydotool"]

    errors = []
    for name in want:
        try:
            if name == "ydotool":
                return YdotoolBackend()
            return PynputBackend()
        except Exception as exc:
            errors.append("%s: %s" % (name, exc))

    b = Backend()
    b.error = "; ".join(errors)
    return b


BACKEND = pick_backend()


# --------------------------------------------------------------------------
# 事件分发
# --------------------------------------------------------------------------

def handle(ev):
    t = ev.get("t")
    if t == "m":
        BACKEND.move(float(ev.get("x", 0)), float(ev.get("y", 0)))
    elif t == "mb":
        if "x" in ev and "y" in ev:
            BACKEND.move(float(ev.get("x", 0)), float(ev.get("y", 0)))
        BACKEND.button(int(ev.get("b", 0) or 0), ev.get("d") == 1)
    elif t == "w":
        BACKEND.scroll(int(ev.get("dy", 0) or 0))
    elif t == "k":
        BACKEND.key(
            ev.get("d") == "down",
            ev.get("code", ""),
            ev.get("key", ""),
            int(ev.get("mods", 0) or 0),
        )
    elif t == "release":
        BACKEND.release_all()


def apply_events(events):
    """一次请求里的连续鼠标移动只落最后一次：中间位置没有意义，
    还能把每帧几十次指针移动压成一次（ydotool 每次都要拉起一个进程）。"""
    pending_move = None
    for ev in events:
        try:
            if ev.get("t") == "m":
                pending_move = ev
                continue
            if pending_move is not None:
                handle(pending_move)
                pending_move = None
            handle(ev)
        except Exception:
            pass
    if pending_move is not None:
        try:
            handle(pending_move)
        except Exception:
            pass


# --------------------------------------------------------------------------
# HTTP
# --------------------------------------------------------------------------

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _cors(self):
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Headers", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Private-Network", "true")

    def _reply(self, code, body=b"", ctype=None):
        self.send_response(code)
        self._cors()
        if ctype:
            self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_OPTIONS(self):
        self._reply(204)

    def do_GET(self):
        body = json.dumps({
            "ok": True,
            "screen": [SW, SH],
            "backend": BACKEND.name,
            "error": BACKEND.error or None,
        }).encode()
        self._reply(200, body, "application/json")

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b"{}"
        try:
            apply_events(json.loads(raw).get("events", []))
        except Exception:
            pass
        self._reply(204)


if __name__ == "__main__":
    if BACKEND.name == "none":
        print("MyDesk bridge: no usable input backend", file=sys.stderr)
        if BACKEND.error:
            print("  " + BACKEND.error, file=sys.stderr)
        print("  On Wayland install ydotool (uinput); on X11 install pynput.", file=sys.stderr)
    print("MyDesk bridge  ->  http://127.0.0.1:%d   backend=%s   screen %dx%d"
          % (PORT, BACKEND.name, SW, SH))
    httpd = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    httpd.daemon_threads = True
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        BACKEND.close()
