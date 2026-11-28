//! Linux 原生注入：直接写 `/dev/uinput`。
//!
//! 为什么不继续用 enigo 的默认后端：enigo 0.6 默认只开 `x11rb`，也就是只能往
//! XWayland 里注入；Wayland 原生窗口（尤其是 niri 这类不实现
//! `zwlr_virtual_pointer_v1` 的合成器）收不到任何事件。uinput 在**内核**层造一个
//! 真实的虚拟键鼠设备，任何合成器都会把它当成硬件输入，因此 niri / sway /
//! GNOME / KDE 一视同仁。
//!
//! 代价是需要 `/dev/uinput` 的写权限（udev 规则或 input 组），这一点和 ydotool
//! 完全一样。
//!
//! 设备一次创建，同时提供绝对坐标指针（ABS_X/ABS_Y）、三个按键、滚轮和键盘。

#![cfg(target_os = "linux")]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::thread::sleep;
use std::time::Duration;

use libc::{c_int, c_ulong};

const UI_DEV_CREATE: c_ulong = 0x5501;
const UI_DEV_DESTROY: c_ulong = 0x5502;
const UI_SET_EVBIT: c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: c_ulong = 0x4004_5565;
const UI_SET_RELBIT: c_ulong = 0x4004_5566;
const UI_SET_ABSBIT: c_ulong = 0x4004_5567;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;

const REL_WHEEL: u16 = 8;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_MAX: i32 = 65_535;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;

const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_LEFTALT: u16 = 56;
const KEY_LEFTMETA: u16 = 125;

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputUserDev {
    name: [u8; 80],
    id: InputId,
    ff_effects_max: u32,
    absmax: [i32; 64],
    absmin: [i32; 64],
    absfuzz: [i32; 64],
    absflat: [i32; 64],
}

/// 浏览器 `KeyboardEvent.code` → Linux evdev keycode。
/// 和 `bridge.py` 里的表同一份数据，改一处记得改另一处。
fn evdev_code(code: &str) -> Option<u16> {
    Some(match code {
        "Escape" => 1, "Digit1" => 2, "Digit2" => 3, "Digit3" => 4, "Digit4" => 5,
        "Digit5" => 6, "Digit6" => 7, "Digit7" => 8, "Digit8" => 9, "Digit9" => 10,
        "Digit0" => 11, "Minus" => 12, "Equal" => 13, "Backspace" => 14, "Tab" => 15,
        "KeyQ" => 16, "KeyW" => 17, "KeyE" => 18, "KeyR" => 19, "KeyT" => 20,
        "KeyY" => 21, "KeyU" => 22, "KeyI" => 23, "KeyO" => 24, "KeyP" => 25,
        "BracketLeft" => 26, "BracketRight" => 27, "Enter" => 28, "ControlLeft" => 29,
        "KeyA" => 30, "KeyS" => 31, "KeyD" => 32, "KeyF" => 33, "KeyG" => 34,
        "KeyH" => 35, "KeyJ" => 36, "KeyK" => 37, "KeyL" => 38, "Semicolon" => 39,
        "Quote" => 40, "Backquote" => 41, "ShiftLeft" => 42, "Backslash" => 43,
        "KeyZ" => 44, "KeyX" => 45, "KeyC" => 46, "KeyV" => 47, "KeyB" => 48,
        "KeyN" => 49, "KeyM" => 50, "Comma" => 51, "Period" => 52, "Slash" => 53,
        "ShiftRight" => 54, "NumpadMultiply" => 55, "AltLeft" => 56, "Space" => 57,
        "CapsLock" => 58, "F1" => 59, "F2" => 60, "F3" => 61, "F4" => 62, "F5" => 63,
        "F6" => 64, "F7" => 65, "F8" => 66, "F9" => 67, "F10" => 68, "NumLock" => 69,
        "ScrollLock" => 70, "Numpad7" => 71, "Numpad8" => 72, "Numpad9" => 73,
        "NumpadSubtract" => 74, "Numpad4" => 75, "Numpad5" => 76, "Numpad6" => 77,
        "NumpadAdd" => 78, "Numpad1" => 79, "Numpad2" => 80, "Numpad3" => 81,
        "Numpad0" => 82, "NumpadDecimal" => 83, "F11" => 87, "F12" => 88,
        "NumpadEnter" => 96, "ControlRight" => 97, "NumpadDivide" => 98,
        "PrintScreen" => 99, "AltRight" => 100, "Home" => 102, "ArrowUp" => 103,
        "PageUp" => 104, "ArrowLeft" => 105, "ArrowRight" => 106, "End" => 107,
        "ArrowDown" => 108, "PageDown" => 109, "Insert" => 110, "Delete" => 111,
        "AudioVolumeMute" => 113, "AudioVolumeDown" => 114, "AudioVolumeUp" => 115,
        "Pause" => 119, "MetaLeft" => 125, "MetaRight" => 126, "ContextMenu" => 127,
        _ => return None,
    })
}

fn modifier_of(bit: u8) -> Option<u16> {
    match bit {
        1 => Some(KEY_LEFTCTRL),
        2 => Some(KEY_LEFTSHIFT),
        4 => Some(KEY_LEFTALT),
        8 => Some(KEY_LEFTMETA),
        _ => None,
    }
}

fn is_modifier_key(code: u16) -> bool {
    matches!(code, KEY_LEFTCTRL | KEY_LEFTSHIFT | KEY_LEFTALT | KEY_LEFTMETA)
}

fn button_of(index: u8) -> Option<u16> {
    match index {
        0 => Some(BTN_LEFT),
        1 => Some(BTN_MIDDLE),
        2 => Some(BTN_RIGHT),
        3 => Some(BTN_SIDE),
        4 => Some(BTN_EXTRA),
        _ => None,
    }
}

pub struct Uinput {
    file: File,
    screen: (i32, i32),
    held_mods: u8,
    held_keys: Vec<u16>,
}

impl Uinput {
    /// 造一个同时具备指针 + 键盘的虚拟设备。失败时返回可读原因（多半是权限）。
    pub fn new(screen: (i32, i32)) -> Result<Self, String> {
        let file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .map_err(|e| format!("打开 /dev/uinput 失败: {e}（需要 udev 规则或加入 input 组）"))?;
        let fd = file.as_raw_fd();

        unsafe {
            for ev in [EV_KEY, EV_REL, EV_ABS, EV_SYN] {
                if ioctl(fd, UI_SET_EVBIT, ev as c_int) < 0 {
                    return Err(format!("UI_SET_EVBIT({ev}) 失败: {}", last_error()));
                }
            }
            // 指针三个键 + 侧键
            for key in [BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE, BTN_EXTRA] {
                if ioctl(fd, UI_SET_KEYBIT, key as c_int) < 0 {
                    return Err(format!("UI_SET_KEYBIT({key}) 失败: {}", last_error()));
                }
            }
            // 键盘：把 1..=127 里合法的 keycode 全开，简单且覆盖常用键
            for code in 1..=127u16 {
                if !is_modifier_key(code) && evdev_code_by_number(code).is_none() {
                    continue;
                }
                ioctl(fd, UI_SET_KEYBIT, code as c_int);
            }
            ioctl(fd, UI_SET_RELBIT, REL_WHEEL as c_int);
            ioctl(fd, UI_SET_ABSBIT, ABS_X as c_int);
            ioctl(fd, UI_SET_ABSBIT, ABS_Y as c_int);
        }

        let mut dev: UinputUserDev = unsafe { std::mem::zeroed() };
        let name = b"MyDesk Virtual Input";
        dev.name[..name.len()].copy_from_slice(name);
        dev.id = InputId { bustype: 0x03, vendor: 0x4d44, product: 0x0001, version: 1 };
        dev.absmin[ABS_X as usize] = 0;
        dev.absmax[ABS_X as usize] = ABS_MAX;
        dev.absmin[ABS_Y as usize] = 0;
        dev.absmax[ABS_Y as usize] = ABS_MAX;

        let bytes = unsafe {
            std::slice::from_raw_parts(
                &dev as *const UinputUserDev as *const u8,
                std::mem::size_of::<UinputUserDev>(),
            )
        };
        let mut writer = &file;
        writer.write_all(bytes).map_err(|e| format!("写入设备描述失败: {e}"))?;

        if unsafe { ioctl(fd, UI_DEV_CREATE, 0) } < 0 {
            return Err(format!("UI_DEV_CREATE 失败: {}", last_error()));
        }
        // 内核需要一点时间把 event 节点注册出来
        sleep(Duration::from_millis(120));

        Ok(Self { file, screen, held_mods: 0, held_keys: Vec::new() })
    }

    fn emit(&mut self, type_: u16, code: u16, value: i32) {
        let ev = libc::input_event {
            time: libc::timeval { tv_sec: 0, tv_usec: 0 },
            type_,
            code,
            value,
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(&ev as *const _ as *const u8, std::mem::size_of::<libc::input_event>())
        };
        let _ = (&self.file).write_all(bytes);
    }

    fn sync(&mut self) {
        self.emit(EV_SYN, SYN_REPORT, 0);
    }

    pub fn move_abs(&mut self, x: f64, y: f64) {
        let (w, h) = self.screen;
        let px = (x.clamp(0.0, 1.0) * f64::from(ABS_MAX)).round() as i32;
        let py = (y.clamp(0.0, 1.0) * f64::from(ABS_MAX)).round() as i32;
        let _ = (w, h);
        self.emit(EV_ABS, ABS_X, px);
        self.emit(EV_ABS, ABS_Y, py);
        self.sync();
    }

    pub fn button(&mut self, index: u8, down: bool) {
        if let Some(btn) = button_of(index) {
            self.emit(EV_KEY, btn, if down { 1 } else { 0 });
            self.sync();
        }
    }

    pub fn wheel(&mut self, dy: i32) {
        // evdev: 正值 = 向上；页面里的 dy 正值 = 向下
        self.emit(EV_REL, REL_WHEEL, -dy);
        self.sync();
    }

    pub fn sync_mods(&mut self, want: u8) {
        let want = want & 0x0f;
        if want == self.held_mods {
            return;
        }
        for bit in [1u8, 2, 4, 8] {
            if let Some(code) = modifier_of(bit) {
                let had = self.held_mods & bit != 0;
                let wants = want & bit != 0;
                if wants && !had {
                    self.emit(EV_KEY, code, 1);
                } else if !wants && had {
                    self.emit(EV_KEY, code, 0);
                }
            }
        }
        self.held_mods = want;
        self.sync();
    }

    pub fn key(&mut self, down: bool, code: &str, mods: u8) {
        let Some(kc) = evdev_code(code) else { return };
        if is_modifier_key(kc) {
            // 修饰键完全由 mods 位管理
            self.sync_mods(mods);
            return;
        }
        self.sync_mods(mods);
        if down {
            if !self.held_keys.contains(&kc) {
                self.held_keys.push(kc);
                self.emit(EV_KEY, kc, 1);
            }
        } else if let Some(pos) = self.held_keys.iter().position(|k| *k == kc) {
            self.held_keys.remove(pos);
            self.emit(EV_KEY, kc, 0);
        }
        self.sync();
    }

    pub fn release_all(&mut self) {
        let keys: Vec<u16> = self.held_keys.drain(..).collect();
        for kc in keys {
            self.emit(EV_KEY, kc, 0);
        }
        for bit in [1u8, 2, 4, 8] {
            if let Some(code) = modifier_of(bit) {
                if self.held_mods & bit != 0 {
                    self.emit(EV_KEY, code, 0);
                }
            }
        }
        self.held_mods = 0;
        self.sync();
    }
}

impl Drop for Uinput {
    fn drop(&mut self) {
        unsafe { ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY, 0) };
    }
}

unsafe fn ioctl(fd: c_int, request: c_ulong, arg: c_int) -> c_int {
    libc::ioctl(fd, request, arg)
}

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

/// 只用于建表时判断某个 keycode 是否在我们的映射里（避免把全部 1..127 都打开）
fn evdev_code_by_number(code: u16) -> Option<&'static str> {
    let all = [
        "Escape", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7",
        "Digit8", "Digit9", "Digit0", "Minus", "Equal", "Backspace", "Tab", "KeyQ", "KeyW",
        "KeyE", "KeyR", "KeyT", "KeyY", "KeyU", "KeyI", "KeyO", "KeyP", "BracketLeft",
        "BracketRight", "Enter", "ControlLeft", "KeyA", "KeyS", "KeyD", "KeyF", "KeyG",
        "KeyH", "KeyJ", "KeyK", "KeyL", "Semicolon", "Quote", "Backquote", "ShiftLeft",
        "Backslash", "KeyZ", "KeyX", "KeyC", "KeyV", "KeyB", "KeyN", "KeyM", "Comma",
        "Period", "Slash", "ShiftRight", "NumpadMultiply", "AltLeft", "Space", "CapsLock",
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "NumLock",
        "ScrollLock", "Numpad7", "Numpad8", "Numpad9", "NumpadSubtract", "Numpad4",
        "Numpad5", "Numpad6", "NumpadAdd", "Numpad1", "Numpad2", "Numpad3", "Numpad0",
        "NumpadDecimal", "F11", "F12", "NumpadEnter", "ControlRight", "NumpadDivide",
        "PrintScreen", "AltRight", "Home", "ArrowUp", "PageUp", "ArrowLeft", "ArrowRight",
        "End", "ArrowDown", "PageDown", "Insert", "Delete", "AudioVolumeMute",
        "AudioVolumeDown", "AudioVolumeUp", "Pause", "MetaLeft", "MetaRight", "ContextMenu",
    ];
    all.into_iter().find(|name| evdev_code(name) == Some(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_table_is_consistent() {
        for code in [1u16, 29, 30, 57, 87] {
            assert!(evdev_code_by_number(code).is_some(), "missing {code}");
        }
        assert_eq!(evdev_code("KeyA"), Some(30));
        assert_eq!(evdev_code("Escape"), Some(1));
        assert_eq!(evdev_code("Nope"), None);
    }

    #[test]
    fn user_dev_layout_matches_the_kernel() {
        // 内核头文件里这个是 1116 字节：80 + 8 + 4 + 4 * 64 * 4
        assert_eq!(std::mem::size_of::<UinputUserDev>(), 1116);
    }

    /// 只有真的能写 /dev/uinput 时才跑（CI 上通常没有，自动跳过）
    #[test]
    fn creates_a_device_when_uinput_is_writable() {
        if std::fs::OpenOptions::new().write(true).open("/dev/uinput").is_err() {
            eprintln!("skipping: /dev/uinput not writable");
            return;
        }
        let mut dev = Uinput::new((1920, 1080)).expect("create uinput device");
        dev.move_abs(0.5, 0.5);
        dev.button(0, true);
        dev.button(0, false);
        dev.key(true, "KeyA", 1);
        dev.key(false, "KeyA", 1);
        dev.release_all();
        // 设备出现在 sysfs 里，说明 UI_DEV_CREATE 生效
        let found = std::fs::read_dir("/sys/class/input")
            .map(|entries| {
                entries.flatten().any(|e| {
                    std::fs::read_to_string(e.path().join("device/name"))
                        .map(|n| n.trim() == "MyDesk Virtual Input")
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        assert!(found, "virtual device not present in sysfs");
    }
}
