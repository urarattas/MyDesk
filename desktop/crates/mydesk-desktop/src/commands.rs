//! Tauri 命令：前端唯一能碰到系统的地方。
//!
//! 网页版对应的是「HTTP 打到 127.0.0.1:8765」，这里换成了进程内调用 ——
//! 少一次 HTTP 往返、少一次 JSON 反序列化、没有 CORS、也没有能被任意网页打到的端口。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use mydesk_core::input::InputEvent;
use serde::Serialize;
use tauri::AppHandle;

use crate::config::{self, ConfigPatch, ConfigView};
#[cfg(windows)]
use crate::capture_win;
use crate::inject::{self, InputStatus};

/* ------------------------------ 自检 ------------------------------ */

#[tauri::command]
pub fn selftest_mode() -> bool {
    std::env::var("MYDESK_SELFTEST").map(|v| v == "1").unwrap_or(false)
}

/// 自检模式下把前端探测结果落盘（无 GUI 环境下也能验证 WebView2 的能力）
#[tauri::command]
pub fn selftest_report(payload: String) -> Result<String, String> {
    if let Ok(path) = std::env::var("MYDESK_SELFTEST_OUT") {
        std::fs::write(&path, payload.as_bytes()).map_err(|e| e.to_string())?;
        Ok(format!("written to {path}"))
    } else {
        Ok("no MYDESK_SELFTEST_OUT".into())
    }
}

/// 自动化端到端测试的参数（由外层测试脚本通过环境变量注入）
#[derive(Debug, Serialize)]
pub struct AutotestEnv {
    pub role: String,
    pub server: String,
    pub room: String,
    pub out: String,
    pub force_native: bool,
    pub share: bool,
    pub allow: bool,
}

#[tauri::command]
pub fn autotest_env() -> Option<AutotestEnv> {
    let role = std::env::var("MYDESK_AUTOTEST").ok()?;
    if role.trim().is_empty() {
        return None;
    }
    Some(AutotestEnv {
        role: role.trim().to_string(),
        server: std::env::var("MYDESK_AUTOTEST_SERVER").unwrap_or_default(),
        room: std::env::var("MYDESK_AUTOTEST_ROOM").unwrap_or_default(),
        out: std::env::var("MYDESK_AUTOTEST_OUT").unwrap_or_default(),
        force_native: std::env::var("MYDESK_AUTOTEST_NATIVE").map(|v| v == "1").unwrap_or(false),
        share: std::env::var("MYDESK_AUTOTEST_SHARE").map(|v| v == "1").unwrap_or(false),
        allow: std::env::var("MYDESK_AUTOTEST_ALLOW").map(|v| v == "1").unwrap_or(false),
    })
}

/// 自动化测试用的写文件通道（比在控制台抓输出可靠）。
///
/// 只在自动化/自检环境开放：否则 WebView 里任意脚本都能借此写任意路径，
/// 等于给页面一个「以当前用户身份落盘」的原语。
#[tauri::command]
pub fn autotest_write(path: String, content: String) -> Result<(), String> {
    if std::env::var("MYDESK_AUTOTEST").is_err() && std::env::var("MYDESK_SELFTEST").is_err() {
        return Err("autotest_write 仅在自动化测试模式下可用".into());
    }
    if let Some(dir) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&path, content).map_err(|e| e.to_string())
}

/// 自动化测试跑完后主动退出（避免测试脚本等超时）
#[tauri::command]
pub fn exit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
pub fn selftest_done(app: AppHandle) {
    tracing::info!("自检完成，退出");
    app.exit(0);
}

/* ------------------------------ 输入 ------------------------------ */

/// 一批键鼠事件（前端按帧聚合后一次送来，最多几百条）
#[tauri::command]
pub fn inject_events(events: Vec<serde_json::Value>) -> usize {
    let mut applied = 0usize;
    let mut guard = match inject::global().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    for value in events {
        match serde_json::from_value::<InputEvent>(value) {
            Ok(ev) => {
                guard.apply(ev);
                applied += 1;
            }
            Err(e) => tracing::debug!(error = %e, "无法解析输入事件"),
        }
    }
    applied
}

#[tauri::command]
pub fn input_status() -> InputStatus {
    let guard = match inject::global().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.status()
}

/// 与网页端的「允许对方控制我」完全同义：默认关闭，必须显式打开
#[tauri::command]
pub fn set_allow_remote(allow: bool) -> Result<bool, String> {
    let mut guard = match inject::global().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.allow_remote = allow;
    if !allow {
        guard.release_all();
    }
    let mut cfg = config::load()?;
    cfg.input.allow_remote = allow;
    let _ = config::save(&cfg);
    Ok(guard.allow_remote)
}

/// 当前系统光标坐标（端到端测试用它证明"真的注入了"）
#[tauri::command]
pub fn cursor_pos() -> Option<(i32, i32)> {
    let mut guard = match inject::global().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.cursor()
}

#[tauri::command]
pub fn release_input() {
    let mut guard = match inject::global().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.release_all();
}

/* ------------------------------ 配置 ------------------------------ */

#[tauri::command]
pub fn config_get() -> Result<ConfigView, String> {
    let cfg = config::load()?;
    // 启动时把持久化的授权状态同步给注入层
    let allow = cfg.input.allow_remote;
    if let Ok(mut guard) = inject::global().lock() {
        guard.allow_remote = allow;
    }
    Ok(ConfigView::new(&cfg))
}

#[tauri::command]
pub fn config_set(patch: serde_json::Value) -> Result<ConfigView, String> {
    let patch: ConfigPatch = serde_json::from_value(patch).map_err(|e| e.to_string())?;
    let mut cfg = config::load()?;
    config::apply(&mut cfg, &patch)?;
    config::save(&cfg)?;
    if let Ok(mut guard) = inject::global().lock() {
        guard.allow_remote = cfg.input.allow_remote;
    }
    Ok(ConfigView::new(&cfg))
}

#[tauri::command]
pub fn server_history() -> Vec<String> {
    config::load().map(|c| c.server.recent).unwrap_or_default()
}

#[tauri::command]
pub fn ws_url(server: String) -> Result<String, String> {
    mydesk_core::signal::ws_url(&server).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn room_link(server: String, code: String) -> Result<String, String> {
    mydesk_core::signal::room_link(&server, &code).map_err(|e| e.to_string())
}

/* ------------------------------ 截屏兜底 ------------------------------ */

/// WebView2 里 `getDisplayMedia` 不可用时（或用户强制原生）的退路：
/// Rust 截屏 → JPEG → 前端画到 canvas → `canvas.captureStream()` 变成真正的视频轨。
///
/// 关键设计：截屏线程常驻，只保留**最新一帧**。
/// 同步在 IPC 调用里截屏会慢到 1.9 s/帧（枚举显示器 + GDI 抓屏 + 缩放 + JPEG 全串行），
/// 前端一帧一帧 `invoke` 就是在等这个 1.9 s。
struct Capturer {
    monitor: usize,
    fps: u32,
    max_width: u32,
    /// 主线程取好的屏幕尺寸（工作线程里绝不能再碰 xcap：实测会卡住）
    width: u32,
    height: u32,
}

struct FrameSlot {
    jpeg: Vec<u8>,
    width: u32,
    height: u32,
    seq: u64,
}

struct CaptureState {
    slot: Arc<Mutex<Option<FrameSlot>>>,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<CaptureStatsInner>>,
}

#[derive(Default, Clone, Copy)]
struct CaptureStatsInner {
    frames: u64,
    last_capture_ms: f64,
    last_encode_ms: f64,
    avg_interval_ms: f64,
}

static CAPTURE: Mutex<Option<CaptureState>> = Mutex::new(None);

#[derive(Debug, Serialize)]
pub struct MonitorInfo {
    pub index: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Serialize)]
pub struct CaptureInfo {
    pub monitor: usize,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Debug, Serialize)]
pub struct CaptureFrame {
    pub jpeg: String,
    pub width: u32,
    pub height: u32,
    /// 帧序号：前端可以据此判断有没有新帧，避免重复 base64 编码同一帧
    pub seq: u64,
}

#[derive(Debug, Serialize)]
pub struct CaptureStats {
    pub running: bool,
    pub frames: u64,
    pub last_capture_ms: f64,
    pub last_encode_ms: f64,
    pub avg_interval_ms: f64,
}

#[tauri::command]
pub fn capture_monitors() -> Result<Vec<MonitorInfo>, String> {
    let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (index, m) in monitors.iter().enumerate() {
        out.push(MonitorInfo {
            index,
            name: m.name().unwrap_or_else(|_| format!("显示器 {index}")),
            width: m.width().unwrap_or(0),
            height: m.height().unwrap_or(0),
        });
    }
    Ok(out)
}

#[tauri::command]
pub fn capture_start(monitor: Option<usize>, fps: Option<u32>) -> Result<CaptureInfo, String> {
    let cfg = config::load()?;
    let monitor = monitor.unwrap_or(cfg.media.monitor);
    let fps = fps.unwrap_or(cfg.media.max_fps).clamp(1, 60);
    let (width, height) = {
        let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
        let m = monitors.get(monitor).or_else(|| monitors.first()).ok_or("没有可用显示器")?;
        (
            m.width().map_err(|e| e.to_string())?,
            m.height().map_err(|e| e.to_string())?,
        )
    };

    // 关掉上一次
    capture_stop();

    let slot = Arc::new(Mutex::new(None::<FrameSlot>));
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Mutex::new(CaptureStatsInner::default()));

    let cap = Capturer { monitor, fps, max_width: 1920, width, height };
    let (slot_t, stop_t, stats_t) = (slot.clone(), stop.clone(), stats.clone());
    std::thread::Builder::new()
        .name("mydesk-capture".into())
        .spawn(move || capture_loop(cap, slot_t, stop_t, stats_t))
        .map_err(|e| format!("启动截屏线程失败: {e}"))?;

    *CAPTURE.lock().map_err(|e| e.to_string())? =
        Some(CaptureState { slot, stop, stats });
    Ok(CaptureInfo { monitor, width, height, fps })
}

/// 采集线程：自己持有 Monitor（可能不是 Send），只把编码后的字节送出去
fn capture_loop(cap: Capturer, slot: Arc<Mutex<Option<FrameSlot>>>, stop: Arc<AtomicBool>, stats: Arc<Mutex<CaptureStatsInner>>) {
    let interval = std::time::Duration::from_micros(1_000_000 / u64::from(cap.fps.max(1)));
    let mut seq = 0u64;
    let (mw, mh) = (cap.width as i32, cap.height as i32);
    // 非 Windows 才需要 xcap；Windows 走纯 GDI，工作线程里完全不碰 xcap
    #[cfg(not(windows))]
    let mut cached: Option<xcap::Monitor> = None;
    tracing::info!(width = mw, height = mh, fps = cap.fps, "截屏线程启动");
    while !stop.load(Ordering::Relaxed) {
        let t0 = std::time::Instant::now();

        // 目标尺寸：超过 max_width 就直接交给 StretchBlt 缩放（比 CPU 重采样快得多）
        #[allow(unused_variables)]
        let (dw, dh) = if mw as u32 > cap.max_width {
            let ratio = cap.max_width as f64 / mw as f64;
            (cap.max_width as i32, ((mh as f64 * ratio) as i32).max(2))
        } else {
            (mw, mh)
        };
        #[cfg(windows)]
        let raw: Option<(Vec<u8>, u32, u32)> = capture_win::capture_scaled_bgra(0, 0, mw, mh, dw, dh)
            .map(|buf| (capture_win::bgra_to_rgba(buf), dw as u32, dh as u32));

        #[cfg(not(windows))]
        let raw: Option<(Vec<u8>, u32, u32)> = {
            if cached.is_none() {
                cached = xcap::Monitor::all().ok().and_then(|l| l.into_iter().nth(cap.monitor));
            }
            cached
                .as_ref()
                .and_then(|m| m.capture_image().ok())
                .map(|img| (img.as_raw().clone(), img.width(), img.height()))
        };

        let Some((pixels, pw, ph)) = raw else {
            tracing::warn!("抓屏失败，稍后重试");
            #[cfg(not(windows))]
            {
                cached = None;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
            continue;
        };
        let capture_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = std::time::Instant::now();
        let (width, height) = (pw, ph);
        let mut buf: Vec<u8> = Vec::new();
        // jpeg-encoder 比 image 自带编码器快一个数量级（2560×1440 实测 ~150 ms → ~25 ms）
        let encoder = jpeg_encoder::Encoder::new(&mut buf, 70);
        if encoder
            .encode(&pixels, width as u16, height as u16, jpeg_encoder::ColorType::Rgba)
            .is_err()
        {
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        }
        let encode_ms = t1.elapsed().as_secs_f64() * 1000.0;

        seq += 1;
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(FrameSlot { jpeg: buf, width, height, seq });
        }
        if let Ok(mut s) = stats.lock() {
            s.frames += 1;
            s.last_capture_ms = capture_ms;
            s.last_encode_ms = encode_ms;
            let spent = t0.elapsed().as_secs_f64() * 1000.0;
            s.avg_interval_ms = if s.frames <= 1 { spent } else { s.avg_interval_ms * 0.8 + spent * 0.2 };
        }
        let spent = t0.elapsed();
        if spent < interval {
            std::thread::sleep(interval - spent);
        }
    }
    tracing::info!("截屏线程退出");
}

#[tauri::command]
pub fn capture_stats() -> CaptureStats {
    let guard = match CAPTURE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    match guard.as_ref() {
        Some(state) => {
            let s = state.stats.lock().map(|v| *v).unwrap_or_default();
            CaptureStats {
                running: true,
                frames: s.frames,
                last_capture_ms: s.last_capture_ms,
                last_encode_ms: s.last_encode_ms,
                avg_interval_ms: s.avg_interval_ms,
            }
        }
        None => CaptureStats { running: false, frames: 0, last_capture_ms: 0.0, last_encode_ms: 0.0, avg_interval_ms: 0.0 },
    }
}

#[tauri::command]
pub fn capture_stop() {
    let mut guard = match CAPTURE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if let Some(state) = guard.take() {
        state.stop.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
pub fn capture_frame(last_seq: Option<u64>) -> Result<Option<CaptureFrame>, String> {
    let (slot, want) = {
        let guard = CAPTURE.lock().map_err(|e| e.to_string())?;
        let state = guard.as_ref().ok_or("截屏尚未开始")?;
        (state.slot.clone(), last_seq.unwrap_or(0))
    };
    let guard = slot.lock().map_err(|e| e.to_string())?;
    let Some(frame) = guard.as_ref() else {
        return Ok(None);
    };
    if frame.seq <= want {
        // 没有新帧：省掉一次 base64
        return Ok(Some(CaptureFrame {
            jpeg: String::new(),
            width: frame.width,
            height: frame.height,
            seq: frame.seq,
        }));
    }
    Ok(Some(CaptureFrame {
        jpeg: B64.encode(&frame.jpeg),
        width: frame.width,
        height: frame.height,
        seq: frame.seq,
    }))
}

/* ------------------------------ 注入探针 ------------------------------ */

/// 端到端/自检用：读光标 → 移动 → 再读，用来确认 enigo 真的在动系统光标
#[derive(Debug, Serialize)]
pub struct InjectProbe {
    pub before: Option<(i32, i32)>,
    pub after: Option<(i32, i32)>,
    pub screen: (i32, i32),
    pub ok: bool,
}

/// 只在自检/自动化里可用：它会临时给自己授权再移动光标，
/// 不该成为页面随时可调的「绕过 allow_remote」入口。
#[tauri::command]
pub fn inject_probe() -> Result<InjectProbe, String> {
    if std::env::var("MYDESK_SELFTEST").is_err() && std::env::var("MYDESK_AUTOTEST").is_err() {
        return Err("inject_probe 仅在自检模式下可用".into());
    }
    let mut guard = match inject::global().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let was = guard.allow_remote;
    guard.allow_remote = true; // 探针自己给自己授权
    let before = guard.cursor();
    guard.apply(InputEvent::Move { x: 0.5, y: 0.5, b: 0 });
    std::thread::sleep(std::time::Duration::from_millis(80));
    let after = guard.cursor();
    guard.allow_remote = was;
    let screen = guard.status().screen;
    Ok(InjectProbe { before, after, screen, ok: before != after })
}
