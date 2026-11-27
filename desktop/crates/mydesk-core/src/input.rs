//! 控制通道（`in`）的事件模型。字段名与网页端 `sendInput` 完全一致：
//!
//! ```text
//! {t:"m",  x, y, b}                      鼠标移动（x/y 已归一化到 0..1，b 是按键位图）
//! {t:"mb", d:0|1, b, x, y, bts}          鼠标键（d 是数字！）
//! {t:"w",  dy}                           滚轮（dy 是整数步长）
//! {t:"k",  d:"down"|"up", key, code, mods}  键盘（d 是字符串！mods: 1 ctrl 2 shift 4 alt 8 meta）
//! {t:"release"}                          释放所有按住的键
//! ```
//!
//! 注意两个 `d` 的类型不同：这是网页端的历史包袱，照抄才能互通。

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 键盘按下/抬起。网页端发的是字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    Down,
    Up,
}

/// mods 位
pub const MOD_CTRL: u8 = 1;
pub const MOD_SHIFT: u8 = 2;
pub const MOD_ALT: u8 = 4;
pub const MOD_META: u8 = 8;
pub const MOD_MASK: u8 = MOD_CTRL | MOD_SHIFT | MOD_ALT | MOD_META;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum InputEvent {
    #[serde(rename = "m")]
    Move {
        x: f64,
        y: f64,
        #[serde(default)]
        b: u8,
    },
    #[serde(rename = "mb")]
    Button {
        /// 1 = 按下，0 = 抬起（数字，不是字符串）
        d: u8,
        b: u8,
        #[serde(default)]
        x: Option<f64>,
        #[serde(default)]
        y: Option<f64>,
        /// 按下瞬间的按键位图，网页端会带，注入时可以忽略
        #[serde(default)]
        bts: Option<u8>,
    },
    #[serde(rename = "w")]
    Wheel {
        dy: i32,
    },
    #[serde(rename = "k")]
    Key {
        d: Dir,
        #[serde(default)]
        key: String,
        #[serde(default)]
        code: String,
        #[serde(default)]
        mods: u8,
    },
    #[serde(rename = "release")]
    Release,
}

fn clamp01(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

impl InputEvent {
    /// 不信任对端：把坐标夹到屏幕内、mods 只留 4 位、滚轮步长封顶。
    /// 返回 `None` 表示这条消息应当整条丢弃。
    pub fn sanitize(self) -> Option<Self> {
        match self {
            InputEvent::Move { x, y, b } => Some(InputEvent::Move { x: clamp01(x), y: clamp01(y), b: b & 0x1f }),
            InputEvent::Button { d, b, x, y, bts } => {
                if b > 4 {
                    return None;
                }
                Some(InputEvent::Button {
                    d: u8::from(d != 0),
                    b,
                    x: x.map(clamp01),
                    y: y.map(clamp01),
                    bts: bts.map(|v| v & 0x1f),
                })
            }
            InputEvent::Wheel { dy } => Some(InputEvent::Wheel { dy: dy.clamp(-60, 60) }),
            InputEvent::Key { d, key, code, mods } => {
                if code.len() > 32 || key.len() > 32 {
                    return None;
                }
                Some(InputEvent::Key { d, key, code, mods: mods & MOD_MASK })
            }
            InputEvent::Release => Some(InputEvent::Release),
        }
    }

    pub fn is_move(&self) -> bool {
        matches!(self, InputEvent::Move { .. })
    }
}

/// 本地鼠标的发送节流。
///
/// 网页端用 `requestAnimationFrame` 合并指针事件，实际只有 60–120 Hz；
/// 原生端直接吃操作系统事件，可以按 `max_hz`（默认 1000）上报。
/// 这里只做「合并最后一次位置 + 到点再发」，不做排队，保证延迟不随速率增长。
#[derive(Debug, Clone)]
pub struct MouseCoalescer {
    min_gap: Duration,
    since_last: Duration,
    pending: Option<InputEvent>,
}

impl MouseCoalescer {
    /// `max_hz == 0` 表示不限速（来一条发一条）
    pub fn new(max_hz: u32) -> Self {
        let min_gap = if max_hz == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(1.0 / f64::from(max_hz.min(10_000)))
        };
        Self { min_gap, since_last: min_gap, pending: None }
    }

    pub fn max_hz(&self) -> f64 {
        if self.min_gap.is_zero() {
            f64::INFINITY
        } else {
            1.0 / self.min_gap.as_secs_f64()
        }
    }

    /// 送进来一条事件：能立刻发就返回它，否则（只有鼠标移动会这样）暂存并返回 `None`。
    /// 非移动事件不参与节流，直接放行。
    pub fn push(&mut self, ev: InputEvent) -> Option<InputEvent> {
        if !ev.is_move() {
            return Some(ev);
        }
        if self.since_last >= self.min_gap {
            self.since_last = Duration::ZERO;
            self.pending = None;
            Some(ev)
        } else {
            // 同一帧内只保留最后一个位置：中间位置没有意义，攒着反而增加延迟
            self.pending = Some(ev);
            None
        }
    }

    /// 时间前进 `dt`，到点就把攒下的那个位置吐出来。
    pub fn advance(&mut self, dt: Duration) -> Option<InputEvent> {
        self.since_last = self.since_last.saturating_add(dt);
        if self.since_last < self.min_gap {
            return None;
        }
        let ev = self.pending.take();
        if ev.is_some() {
            self.since_last = Duration::ZERO;
        }
        ev
    }

    /// 断开连接 / 释放控制时调用，丢掉未发出的位置
    pub fn clear(&mut self) {
        self.pending = None;
        self.since_last = self.min_gap;
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}

/// 注入层的按键目标：具名键交给平台层翻译，单字符直接按字符打。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTarget {
    Named(&'static str),
    Char(char),
}

/// 修饰键由 `mods` 位统一维护，这些 code 的按下/抬起事件本身要忽略，
/// 否则会出现「Ctrl 被按了两次」或者松不开的问题（bridge.py 里同样的处理）。
pub fn is_modifier(code: &str) -> bool {
    matches!(
        code,
        "ControlLeft" | "ControlRight" | "ShiftLeft" | "ShiftRight" | "AltLeft" | "AltRight" | "MetaLeft" | "MetaRight" | "OSLeft" | "OSRight"
    )
}

/// DOM `KeyboardEvent.code` → 中立键名。表来自 bridge.py 的 ydotool/pynput 映射。
pub fn named_key(code: &str) -> Option<&'static str> {
    Some(match code {
        "Escape" => "Escape",
        "Enter" | "NumpadEnter" => "Enter",
        "Backspace" => "Backspace",
        "Tab" => "Tab",
        "Delete" => "Delete",
        "ArrowUp" => "ArrowUp",
        "ArrowDown" => "ArrowDown",
        "ArrowLeft" => "ArrowLeft",
        "ArrowRight" => "ArrowRight",
        "Home" => "Home",
        "End" => "End",
        "PageUp" => "PageUp",
        "PageDown" => "PageDown",
        "Insert" => "Insert",
        "CapsLock" => "CapsLock",
        "NumLock" => "NumLock",
        "ScrollLock" => "ScrollLock",
        "Pause" => "Pause",
        "PrintScreen" => "PrintScreen",
        "ContextMenu" => "ContextMenu",
        "Space" => "Space",
        "Minus" => "Minus",
        "Equal" => "Equal",
        "BracketLeft" => "BracketLeft",
        "BracketRight" => "BracketRight",
        "Semicolon" => "Semicolon",
        "Quote" => "Quote",
        "Backquote" => "Backquote",
        "Backslash" => "Backslash",
        "Comma" => "Comma",
        "Period" => "Period",
        "Slash" => "Slash",
        "Digit0" => "Digit0",
        "Digit1" => "Digit1",
        "Digit2" => "Digit2",
        "Digit3" => "Digit3",
        "Digit4" => "Digit4",
        "Digit5" => "Digit5",
        "Digit6" => "Digit6",
        "Digit7" => "Digit7",
        "Digit8" => "Digit8",
        "Digit9" => "Digit9",
        "Numpad0" => "Numpad0",
        "Numpad1" => "Numpad1",
        "Numpad2" => "Numpad2",
        "Numpad3" => "Numpad3",
        "Numpad4" => "Numpad4",
        "Numpad5" => "Numpad5",
        "Numpad6" => "Numpad6",
        "Numpad7" => "Numpad7",
        "Numpad8" => "Numpad8",
        "Numpad9" => "Numpad9",
        "NumpadAdd" => "NumpadAdd",
        "NumpadSubtract" => "NumpadSubtract",
        "NumpadMultiply" => "NumpadMultiply",
        "NumpadDivide" => "NumpadDivide",
        "NumpadDecimal" => "NumpadDecimal",
        "F1" => "F1",
        "F2" => "F2",
        "F3" => "F3",
        "F4" => "F4",
        "F5" => "F5",
        "F6" => "F6",
        "F7" => "F7",
        "F8" => "F8",
        "F9" => "F9",
        "F10" => "F10",
        "F11" => "F11",
        "F12" => "F12",
        "AudioVolumeMute" => "AudioVolumeMute",
        "AudioVolumeDown" => "AudioVolumeDown",
        "AudioVolumeUp" => "AudioVolumeUp",
        "MediaPlayPause" => "MediaPlayPause",
        "MediaStop" => "MediaStop",
        "MediaTrackNext" => "MediaTrackNext",
        "MediaTrackPrevious" => "MediaTrackPrevious",
        "BrowserBack" => "BrowserBack",
        "BrowserForward" => "BrowserForward",
        "BrowserRefresh" => "BrowserRefresh",
        "BrowserHome" => "BrowserHome",
        "BrowserSearch" => "BrowserSearch",
        "BrowserFavorites" => "BrowserFavorites",
        _ => return None,
    })
}

/// 解析一条键盘事件：优先用 `code`，退化为 `key` 的单个字符。
pub fn resolve(code: &str, key: &str) -> Option<KeyTarget> {
    if let Some(named) = named_key(code) {
        return Some(KeyTarget::Named(named));
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => Some(KeyTarget::Char(c)),
        _ => named_key(key).map(KeyTarget::Named),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_exact_web_payloads() {
        let cases: &[(&str, InputEvent)] = &[
            (r#"{"t":"m","x":0.42,"y":0.51,"b":0}"#, InputEvent::Move { x: 0.42, y: 0.51, b: 0 }),
            (
                r#"{"t":"m","x":0.1,"y":0.2}"#,
                InputEvent::Move { x: 0.1, y: 0.2, b: 0 },
            ),
            (
                r#"{"t":"mb","d":1,"b":0,"x":0.42,"y":0.51,"bts":1}"#,
                InputEvent::Button { d: 1, b: 0, x: Some(0.42), y: Some(0.51), bts: Some(1) },
            ),
            (r#"{"t":"mb","d":0,"b":2,"x":0.4,"y":0.5}"#, InputEvent::Button { d: 0, b: 2, x: Some(0.4), y: Some(0.5), bts: None }),
            (r#"{"t":"w","dy":-3}"#, InputEvent::Wheel { dy: -3 }),
            (
                r#"{"t":"k","d":"down","key":"a","code":"KeyA","mods":1}"#,
                InputEvent::Key { d: Dir::Down, key: "a".into(), code: "KeyA".into(), mods: 1 },
            ),
            (
                r#"{"t":"k","d":"up","key":"","code":"KeyA","mods":0}"#,
                InputEvent::Key { d: Dir::Up, key: "".into(), code: "KeyA".into(), mods: 0 },
            ),
            (r#"{"t":"release"}"#, InputEvent::Release),
        ];
        for (raw, want) in cases {
            let got: InputEvent = serde_json::from_str(raw).unwrap();
            assert_eq!(&got, want, "parsing {raw}");
        }
    }

    #[test]
    fn hostile_values_are_clamped() {
        let ev: InputEvent = serde_json::from_str(r#"{"t":"m","x":-9e9,"y":1e9}"#).unwrap();
        assert_eq!(ev.sanitize().unwrap(), InputEvent::Move { x: 0.0, y: 1.0, b: 0 });

        let ev: InputEvent = serde_json::from_str(r#"{"t":"w","dy":99999}"#).unwrap();
        assert_eq!(ev.sanitize().unwrap(), InputEvent::Wheel { dy: 60 });

        let ev: InputEvent = serde_json::from_str(r#"{"t":"k","d":"down","key":"a","code":"KeyA","mods":255}"#).unwrap();
        let Some(InputEvent::Key { mods, .. }) = ev.sanitize() else { panic!() };
        assert_eq!(mods, MOD_MASK);

        // 未知鼠标键直接丢弃
        let ev: InputEvent = serde_json::from_str(r#"{"t":"mb","d":1,"b":9}"#).unwrap();
        assert!(ev.sanitize().is_none());
    }

    #[test]
    fn key_resolution_prefers_code() {
        assert_eq!(resolve("KeyA", "a"), Some(KeyTarget::Char('a')));
        assert_eq!(resolve("KeyA", "A"), Some(KeyTarget::Char('A')));
        assert_eq!(resolve("Escape", "Escape"), Some(KeyTarget::Named("Escape")));
        assert_eq!(resolve("Numpad7", "7"), Some(KeyTarget::Named("Numpad7")));
        // code 未知时退化为字符
        assert_eq!(resolve("", "字"), Some(KeyTarget::Char('字')));
        assert_eq!(resolve("", ""), None);
        assert!(is_modifier("ControlLeft"));
        assert!(!is_modifier("KeyA"));
    }

    #[test]
    fn coalescer_keeps_the_last_position_and_never_queues() {
        let mut c = MouseCoalescer::new(1000); // 1 ms 一次
        let mv = |x: f64| InputEvent::Move { x, y: 0.0, b: 0 };

        // 第一条立刻发
        assert_eq!(c.push(mv(0.1)), Some(mv(0.1)));
        // 同一毫秒内的三条只留最后一条
        assert_eq!(c.push(mv(0.2)), None);
        assert_eq!(c.push(mv(0.3)), None);
        assert_eq!(c.push(mv(0.4)), None);
        assert!(c.has_pending());
        // 时间没到不发
        assert_eq!(c.advance(Duration::from_micros(500)), None);
        // 到点发最后那个位置
        assert_eq!(c.advance(Duration::from_micros(600)), Some(mv(0.4)));
        assert!(!c.has_pending());
        // 非移动事件不受节流影响
        assert_eq!(c.push(InputEvent::Wheel { dy: 1 }), Some(InputEvent::Wheel { dy: 1 }));
    }

    #[test]
    fn coalescer_zero_means_unlimited() {
        let mut c = MouseCoalescer::new(0);
        for i in 0..100 {
            let x = f64::from(i) / 100.0;
            assert_eq!(c.push(InputEvent::Move { x, y: 0.0, b: 0 }), Some(InputEvent::Move { x, y: 0.0, b: 0 }));
        }
        assert!(c.max_hz().is_infinite());
    }

    #[test]
    fn coalescer_reports_configured_rate() {
        let c = MouseCoalescer::new(1000);
        assert!((c.max_hz() - 1000.0).abs() < 0.001);
    }
}
