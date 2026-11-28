//! 原生键鼠注入。网页版靠 `bridge.py` 做这件事，这里直接落到系统。
//!
//! 两个后端，运行时自动选：
//!
//! * **uinput**（Linux）：直接写 `/dev/uinput`，在内核层造一个虚拟键鼠。
//!   Wayland 原生窗口（niri / sway / GNOME / KDE）都能收到，因为对合成器来说
//!   它就是一个真实设备。需要 `/dev/uinput` 写权限。
//! * **enigo**：X11（x11rb）、Windows（SendInput）、macOS。
//!
//! 与 bridge.py 的差别：没有 HTTP 往返、没有分批，修饰键位图状态机一致，
//! 断开时统一 `release_all()`，不会留下卡住的 Ctrl。

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use mydesk_core::input::{
    self, Dir, InputEvent, KeyTarget, MOD_ALT, MOD_CTRL, MOD_META, MOD_SHIFT,
};
use serde::Serialize;

#[cfg(target_os = "linux")]
use crate::inject_uinput::Uinput;

/// enigo 的 Enigo 不保证 Send/Sync 的稳定性，统一用一把锁包起来
static INJECTOR: OnceLock<Mutex<Injector>> = OnceLock::new();

pub fn global() -> &'static Mutex<Injector> {
    INJECTOR.get_or_init(|| Mutex::new(Injector::new()))
}

#[derive(Debug, Serialize)]
pub struct InputStatus {
    pub allow_remote: bool,
    pub screen: (i32, i32),
    pub held_keys: usize,
    pub held_mods: u8,
    /// 真正落到系统的事件条数（端到端测试用来确认注入链路走通）
    pub applied: u64,
    /// 最近一次注入错误（正常为 None）
    pub last_error: Option<String>,
    /// 实际使用的后端：uinput / enigo / none
    pub backend: &'static str,
}

enum Inner {
    Enigo(Enigo),
    #[cfg(target_os = "linux")]
    Uinput(Uinput),
}

fn mod_key(bit: u8) -> Option<Key> {
    match bit {
        MOD_CTRL => Some(Key::Control),
        MOD_SHIFT => Some(Key::Shift),
        MOD_ALT => Some(Key::Alt),
        MOD_META => Some(Key::Meta),
        _ => None,
    }
}

fn button_of(index: u8) -> Option<Button> {
    match index {
        0 => Some(Button::Left),
        1 => Some(Button::Middle),
        2 => Some(Button::Right),
        3 => Some(Button::Back),
        4 => Some(Button::Forward),
        _ => None,
    }
}

/// 中立键名 → enigo 键。名字不符时只需要改这一张表。
fn named_to_key(name: &str) -> Option<Key> {
    Some(match name {
        "Escape" => Key::Escape,
        "Enter" => Key::Return,
        "Tab" => Key::Tab,
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "Insert" => Key::Insert,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "ArrowUp" => Key::UpArrow,
        "ArrowDown" => Key::DownArrow,
        "ArrowLeft" => Key::LeftArrow,
        "ArrowRight" => Key::RightArrow,
        "Space" => Key::Space,
        "CapsLock" => Key::CapsLock,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        _ => return None,
    })
}

fn direction(down: bool) -> Direction {
    if down {
        Direction::Press
    } else {
        Direction::Release
    }
}

pub struct Injector {
    inner: Option<Inner>,
    screen: (i32, i32),
    held_mods: u8,
    held_keys: HashSet<String>,
    pub allow_remote: bool,
    pub applied: u64,
    last_error: Option<String>,
    backend: &'static str,
}

impl Injector {
    fn new() -> Self {
        let mut me = Self {
            inner: None,
            screen: (1920, 1080),
            held_mods: 0,
            held_keys: HashSet::new(),
            allow_remote: false,
            applied: 0,
            last_error: None,
            backend: "none",
        };
        me.init();
        me
    }

    fn init(&mut self) {
        // Wayland 会话优先 uinput：enigo 的默认后端只能进 XWayland。
        #[cfg(target_os = "linux")]
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            match Uinput::new(self.screen) {
                Ok(dev) => {
                    tracing::info!("注入后端: uinput（Wayland 原生）");
                    self.inner = Some(Inner::Uinput(dev));
                    self.backend = "uinput";
                    return;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "uinput 不可用，回退 enigo/X11");
                    self.last_error = Some(e);
                }
            }
        }

        match Enigo::new(&Settings::default()) {
            Ok(e) => {
                self.inner = Some(Inner::Enigo(e));
                self.backend = "enigo";
                self.refresh_screen();
            }
            Err(e) => {
                self.last_error = Some(format!("初始化输入后端失败: {e}"));
                tracing::error!(error = %e, "enigo 初始化失败");
            }
        }
    }

    pub fn refresh_screen(&mut self) {
        if let Some(Inner::Enigo(e)) = self.inner.as_ref() {
            if let Ok(size) = e.main_display() {
                if size.0 > 0 && size.1 > 0 {
                    self.screen = size;
                }
            }
        }
    }

    pub fn status(&self) -> InputStatus {
        InputStatus {
            allow_remote: self.allow_remote,
            screen: self.screen,
            held_keys: self.held_keys.len(),
            held_mods: self.held_mods,
            applied: self.applied,
            last_error: self.last_error.clone(),
            backend: self.backend,
        }
    }

    /// 当前系统光标位置。uinput 后端读不到，只在 enigo 下可用。
    pub fn cursor(&mut self) -> Option<(i32, i32)> {
        match self.inner.as_ref() {
            Some(Inner::Enigo(e)) => e.location().ok(),
            _ => None,
        }
    }

    pub fn apply(&mut self, ev: InputEvent) {
        if !self.allow_remote {
            return;
        }
        let Some(ev) = ev.sanitize() else { return };
        if self.inner.is_none() {
            return;
        }
        let result = match ev {
            InputEvent::Move { x, y, .. } => self.move_to(x, y),
            InputEvent::Button { d, b, x, y, .. } => {
                if let (Some(x), Some(y)) = (x, y) {
                    let _ = self.move_to(x, y);
                }
                self.press(b, d == 1)
            }
            InputEvent::Wheel { dy } => self.wheel(dy),
            InputEvent::Key { d, key, code, mods } => self.key(d == Dir::Down, &code, &key, mods),
            InputEvent::Release => {
                self.release_all();
                Ok(())
            }
        };
        match result {
            Ok(()) => self.applied += 1,
            Err(e) => {
                self.last_error = Some(e);
            }
        }
    }

    fn move_to(&mut self, x: f64, y: f64) -> Result<(), String> {
        match self.inner.as_mut() {
            #[cfg(target_os = "linux")]
            Some(Inner::Uinput(dev)) => {
                dev.move_abs(x, y);
                Ok(())
            }
            Some(Inner::Enigo(e)) => {
                let (w, h) = self.screen;
                let px = (x * f64::from((w - 1).max(1))).round() as i32;
                let py = (y * f64::from((h - 1).max(1))).round() as i32;
                e.move_mouse(px, py, Coordinate::Abs).map_err(|err| err.to_string())
            }
            None => Err("输入后端不可用".into()),
        }
    }

    fn press(&mut self, index: u8, down: bool) -> Result<(), String> {
        match self.inner.as_mut() {
            #[cfg(target_os = "linux")]
            Some(Inner::Uinput(dev)) => {
                dev.button(index, down);
                Ok(())
            }
            Some(Inner::Enigo(e)) => match button_of(index) {
                Some(button) => e.button(button, direction(down)).map_err(|err| err.to_string()),
                None => Ok(()),
            },
            None => Err("输入后端不可用".into()),
        }
    }

    fn wheel(&mut self, dy: i32) -> Result<(), String> {
        match self.inner.as_mut() {
            #[cfg(target_os = "linux")]
            Some(Inner::Uinput(dev)) => {
                dev.wheel(dy);
                Ok(())
            }
            // enigo 的正方向是「向上」，页面里的 dy 正值是「向下」
            Some(Inner::Enigo(e)) => e
                .scroll(-dy, Axis::Vertical)
                .map_err(|err| err.to_string()),
            None => Err("输入后端不可用".into()),
        }
    }

    fn key(&mut self, down: bool, code: &str, key: &str, mods: u8) -> Result<(), String> {
        if down {
            self.held_keys.insert(code.to_string());
        } else {
            self.held_keys.remove(code);
        }

        // uinput 后端自己维护修饰键位图，不需要先 sync
        #[cfg(target_os = "linux")]
        if matches!(self.inner, Some(Inner::Uinput(_))) {
            let want = mods & input::MOD_MASK;
            if let Some(Inner::Uinput(dev)) = self.inner.as_mut() {
                dev.key(down, code, want);
            }
            self.held_mods = want;
            return Ok(());
        }

        self.sync_mods(mods);
        if input::is_modifier(code) {
            return Ok(());
        }
        let Some(target) = input::resolve(code, key) else {
            return Ok(());
        };
        let enigo_key = match target {
            KeyTarget::Char(c) => Key::Unicode(c),
            KeyTarget::Named(name) => match named_to_key(name) {
                Some(k) => k,
                None => return Ok(()),
            },
        };
        match self.inner.as_mut() {
            Some(Inner::Enigo(e)) => e.key(enigo_key, direction(down)).map_err(|err| err.to_string()),
            #[cfg(target_os = "linux")]
            Some(Inner::Uinput(_)) => Ok(()),
            None => Err("输入后端不可用".into()),
        }
    }

    fn sync_mods(&mut self, want: u8) {
        let want = want & input::MOD_MASK;
        if want == self.held_mods {
            return;
        }
        if let Some(Inner::Enigo(e)) = self.inner.as_mut() {
            for bit in [MOD_CTRL, MOD_SHIFT, MOD_ALT, MOD_META] {
                let Some(k) = mod_key(bit) else { continue };
                let had = self.held_mods & bit != 0;
                let wants = want & bit != 0;
                if wants && !had {
                    let _ = e.key(k, Direction::Press);
                } else if !wants && had {
                    let _ = e.key(k, Direction::Release);
                }
            }
        }
        self.held_mods = want;
    }

    pub fn release_all(&mut self) {
        // 先取出来，避免在借用 inner 的同时再借 self
        let held: Vec<String> = self.held_keys.drain().collect();
        match self.inner.as_mut() {
            #[cfg(target_os = "linux")]
            Some(Inner::Uinput(dev)) => dev.release_all(),
            Some(Inner::Enigo(e)) => {
                for code in held {
                    if let Some(target) = input::resolve(&code, "") {
                        let k = match target {
                            KeyTarget::Char(c) => Key::Unicode(c),
                            KeyTarget::Named(name) => match named_to_key(name) {
                                Some(k) => k,
                                None => continue,
                            },
                        };
                        let _ = e.key(k, Direction::Release);
                    }
                }
                for bit in [MOD_CTRL, MOD_SHIFT, MOD_ALT, MOD_META] {
                    if self.held_mods & bit != 0 {
                        if let Some(k) = mod_key(bit) {
                            let _ = e.key(k, Direction::Release);
                        }
                    }
                }
            }
            None => {}
        }
        self.held_mods = 0;
    }
}
