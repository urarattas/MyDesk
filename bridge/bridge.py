#!/usr/bin/env python3
# MyDesk bridge - 把浏览器收到的键鼠事件注入本机系统
# 依赖: pip install pynput   |   仅监听 127.0.0.1:8765
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

try:
    from pynput.mouse import Controller as MouseCtl, Button
    from pynput.keyboard import Controller as KeyCtl, Key, KeyCode
except ImportError:
    sys.exit("缺少依赖: pip install pynput")

PORT = 8765
mouse, kb = MouseCtl(), KeyCtl()

def screen_size():
    """当前屏幕分辨率。Windows 走 GetSystemMetrics，并先声明 DPI 感知：
    否则高缩放比下拿到的是逻辑像素，鼠标只会落到左上角一块区域。"""
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
    try:
        import tkinter as tk
        _r = tk.Tk(); _r.withdraw()
        size = (_r.winfo_screenwidth(), _r.winfo_screenheight())
        _r.destroy()
        return size
    except Exception:
        return 1920, 1080

SW, SH = screen_size()

BTN = {0: Button.left, 1: Button.middle, 2: Button.right}
MODS = [(1, Key.ctrl), (2, Key.shift), (4, Key.alt), (8, Key.cmd)]
SPECIAL = {
    "Escape": Key.esc, "Enter": Key.enter, "NumpadEnter": Key.enter,
    "Backspace": Key.backspace, "Tab": Key.tab, "Delete": Key.delete,
    "ArrowUp": Key.up, "ArrowDown": Key.down, "ArrowLeft": Key.left,
    "ArrowRight": Key.right, "Home": Key.home, "End": Key.end,
    "PageUp": Key.page_up, "PageDown": Key.page_down, "Insert": Key.insert,
    "CapsLock": Key.caps_lock, "Space": Key.space,
}
for _i in range(1, 13):
    SPECIAL["F" + str(_i)] = getattr(Key, "f" + str(_i))

held_mods = set()
held_by_code = {}          # code -> 实际按下的键对象，避免大小写变化导致按键卡住

def where(ev):
    return (int(float(ev.get("x", 0)) * SW), int(float(ev.get("y", 0)) * SH))

def sync_mods(mods, skip_shift):
    """每个事件都同步修饰键状态：按下缺失的，松开多余的"""
    for bit, mk in MODS:
        want = bool(mods & bit) and not (bit == 2 and skip_shift)
        if want and mk not in held_mods:
            try: kb.press(mk); held_mods.add(mk)
            except Exception: pass
        elif not want and mk in held_mods:
            try: kb.release(mk); held_mods.discard(mk)
            except Exception: pass

def resolve(code, ch):
    """返回 (按键对象, 是否跳过浏览器带来的 shift)。
    字母统一用小写 + 显式 shift；符号本身已含 shift 语义，直接按字符送。"""
    t = SPECIAL.get(code)
    if t is not None:
        return t, False
    if len(ch) == 1 and ch != "Dead":
        try:
            if ch.isalpha():
                return KeyCode.from_char(ch.lower()), False
            return KeyCode.from_char(ch), True
        except Exception:
            return None, False
    return None, False

def do_key(ev):
    down = ev.get("d") == "down"
    code = ev.get("code", "")
    ch = ev.get("key", "")
    mods = int(ev.get("mods", 0) or 0)
    target, skip_shift = resolve(code, ch)
    sync_mods(mods, skip_shift)

    if down:
        if target is None or code in held_by_code:
            return
        try:
            kb.press(target); held_by_code[code] = target
        except Exception:
            pass
    else:
        target = held_by_code.pop(code, target)
        if target is None:
            return
        try:
            kb.release(target)
        except Exception:
            pass

def release_all():
    for target in list(held_by_code.values()):
        try: kb.release(target)
        except Exception: pass
    held_by_code.clear()
    for mk in list(held_mods):
        try: kb.release(mk)
        except Exception: pass
    held_mods.clear()

def handle(ev):
    t = ev.get("t")
    if t == "release":
        release_all()
    elif t == "m":
        mouse.position = where(ev)
    elif t == "mb":
        if "x" in ev and "y" in ev:
            mouse.position = where(ev)
        b = BTN.get(int(ev.get("b", 0)))
        if b is not None:
            (mouse.press if ev.get("d") == 1 else mouse.release)(b)
    elif t == "w":
        mouse.scroll(0, -int(ev.get("dy", 0)))
    elif t == "k":
        do_key(ev)
    elif t == "t":
        kb.type(ev.get("s", ""))

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _cors(self):
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Headers", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Private-Network", "true")

    def do_OPTIONS(self):
        self.send_response(204); self._cors(); self.end_headers()

    def do_GET(self):
        body = json.dumps({"ok": True, "screen": [SW, SH]}).encode()
        self.send_response(200); self._cors()
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers(); self.wfile.write(body)

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        try:
            data = json.loads(self.rfile.read(n) or b"{}")
            for ev in data.get("events", []):
                try: handle(ev)
                except Exception: pass
        except Exception:
            pass
        self.send_response(204); self._cors(); self.end_headers()

if __name__ == "__main__":
    print("MyDesk bridge  ->  http://127.0.0.1:%d   screen %dx%d" % (PORT, SW, SH))
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
