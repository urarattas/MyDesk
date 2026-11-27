//! 原生键鼠注入。网页版靠 `bridge.py` 做这件事，这里直接调 enigo。
//!
//! 与 bridge.py 的差别（也是「不需要桥接」的实质）：
//! * 没有 HTTP 往返、没有 240 条一批的分片、没有 400 条队列截断；
//! * 修饰键用位图状态机同步，断开时统一 `release_all()`，不会留下卡住的 Ctrl。

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use mydesk_core::input::{
    self, Dir, InputEvent, KeyTarget, MOD_ALT, MOD_CTRL, MOD_META, MOD_SHIFT,
};
use serde::Serialize;

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
    enigo: Option<Enigo>,
    screen: (i32, i32),
    held_mods: u8,
    held_keys: HashSet<String>,
    pub allow_remote: bool,
    pub applied: u64,
    last_error: Option<String>,
}

impl Injector {
    fn new() -> Self {
        let mut me = Self {
            enigo: None,
            screen: (1920, 1080),
            held_mods: 0,
            held_keys: HashSet::new(),
            allow_remote: false,
            applied: 0,
            last_error: None,
        };
        me.init();
        me
    }

    fn init(&mut self) {
        match Enigo::new(&Settings::default()) {
            Ok(e) => {
                self.enigo = Some(e);
                self.refresh_screen();
            }
            Err(e) => {
                self.last_error = Some(format!("初始化输入后端失败: {e}"));
                tracing::error!(error = %e, "enigo 初始化失败");
            }
        }
    }

    pub fn refresh_screen(&mut self) {
        if let Some(e) = self.enigo.as_ref() {
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
        }
    }

    /// 当前系统光标位置（Windows 上由 enigo 的 location() 读取）
    pub fn cursor(&mut self) -> Option<(i32, i32)> {
        self.enigo.as_ref().and_then(|e| e.location().ok())
    }

    pub fn apply(&mut self, ev: InputEvent) {
        if !self.allow_remote {
            return;
        }
        let Some(ev) = ev.sanitize() else { return };
        if self.enigo.is_none() {
            return;
        }
        let result = match ev {
            InputEvent::Move { x, y, .. } => self.move_to(x, y),
            InputEvent::Button { d, b, x, y, .. } => {
                if let (Some(x), Some(y)) = (x, y) {
                    let _ = self.move_to(x, y);
                }
                match button_of(b) {
                    Some(button) => self.raw(|e| e.button(button, direction(d == 1))),
                    None => Ok(()),
                }
            }
            InputEvent::Wheel { dy } => self.raw(|e| e.scroll(dy, Axis::Vertical)),
            InputEvent::Key { d, key, code, mods } => {
                self.sync_mods(mods);
                if input::is_modifier(&code) {
                    Ok(())
                } else {
                    self.key(d == Dir::Down, &code, &key)
                }
            }
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

    fn raw<F>(&mut self, f: F) -> Result<(), String>
    where
        F: FnOnce(&mut Enigo) -> enigo::InputResult<()>,
    {
        let Some(e) = self.enigo.as_mut() else { return Err("输入后端不可用".into()) };
        f(e).map_err(|err| err.to_string())
    }

    fn move_to(&mut self, x: f64, y: f64) -> Result<(), String> {
        let (w, h) = self.screen;
        let px = (x * f64::from((w - 1).max(1))).round() as i32;
        let py = (y * f64::from((h - 1).max(1))).round() as i32;
        self.raw(|e| e.move_mouse(px, py, Coordinate::Abs))
    }

    fn sync_mods(&mut self, want: u8) {
        let want = want & input::MOD_MASK;
        if want == self.held_mods {
            return;
        }
        for bit in [MOD_CTRL, MOD_SHIFT, MOD_ALT, MOD_META] {
            let Some(k) = mod_key(bit) else { continue };
            let had = self.held_mods & bit != 0;
            let wants = want & bit != 0;
            if wants && !had {
                let _ = self.raw(|e| e.key(k, Direction::Press));
            } else if !wants && had {
                let _ = self.raw(|e| e.key(k, Direction::Release));
            }
        }
        self.held_mods = want;
    }

    fn key(&mut self, down: bool, code: &str, key: &str) -> Result<(), String> {
        if down {
            self.held_keys.insert(code.to_string());
        } else {
            self.held_keys.remove(code);
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
        self.raw(|e| e.key(enigo_key, direction(down)))
    }

    pub fn release_all(&mut self) {
        let keys: Vec<String> = self.held_keys.drain().collect();
        for code in keys {
            let _ = self.key(false, &code, "");
        }
        for bit in [MOD_CTRL, MOD_SHIFT, MOD_ALT, MOD_META] {
            if self.held_mods & bit != 0 {
                if let Some(k) = mod_key(bit) {
                    let _ = self.raw(|e| e.key(k, Direction::Release));
                }
            }
        }
        self.held_mods = 0;
    }
}
