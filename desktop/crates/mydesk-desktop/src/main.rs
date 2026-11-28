//! MyDesk 桌面客户端（Tauri 2）。
//!
//! 分工：
//! * **前端（WebView2）** —— 信令、WebRTC、SDP/ICE、DataChannel、画面显示。
//!   这部分直接复用 `MyDesk.html` 里已经验证过的逻辑，所以任意一端是浏览器都能互通。
//! * **Rust 侧** —— 浏览器做不到的两件事：原生键鼠注入（enigo）、原生截屏兜底（xcap）。
//!   相当于把网页版依赖的 `bridge.py` 整个吞掉：没有 HTTP、没有 Python、没有批处理队列。
//!
//! 自检模式（无 GUI 也能验证）：
//! ```powershell
//! $env:MYDESK_SELFTEST=1; $env:MYDESK_SELFTEST_OUT="D:\\tmp\\selftest.json"; .\mydesk.exe
//! ```
//! 前端会把 WebView2 的能力探测结果写到该文件并退出。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture_win;
mod commands;
mod inject;
mod config;


/// WebView2 的数据目录。
///
/// 默认它放在 %LOCALAPPDATA%\\<identifier>，在受限环境（或被重定向的 profile）里
/// 会直接 access denied，而 Tauri 在 Windows 上会强制注入 data_directory ——
/// 所以这里自己算一个可写目录，建窗口时显式传进去。
fn webview_data_dir() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("MYDESK_WEBVIEW_DIR") {
        let p = std::path::PathBuf::from(p);
        let _ = std::fs::create_dir_all(&p);
        std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &p);
        return p;
    }
    let dir = mydesk_core::config::default_path()
        .parent()
        .map(|p| p.join("webview2"))
        .unwrap_or_else(|| std::path::PathBuf::from("webview2"));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(path = %dir.display(), error = %e, "创建 WebView2 数据目录失败");
    }
    std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &dir);
    dir
}

fn main() {
    let _ = webview_data_dir();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    tauri::Builder::default()
        .setup(|app| {
            let cfg = mydesk_core::config::Config::load_default().unwrap_or_default();
            tracing::info!(
                mode = cfg.mode.as_str(),
                server = cfg.effective_server().unwrap_or("-"),
                allow_remote = cfg.input.allow_remote,
                "MyDesk 启动"
            );
            let data_dir = webview_data_dir();
            tracing::info!(path = %data_dir.display(), "WebView2 数据目录");
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
                .title("MyDesk")
                .inner_size(1180.0, 800.0)
                .min_inner_size(900.0, 600.0)
                .data_directory(data_dir)
                .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::selftest_mode,
            commands::autotest_env,
            commands::autotest_write,
            commands::exit_app,
            commands::selftest_report,
            commands::selftest_done,
            commands::inject_events,
            commands::input_status,
            commands::set_allow_remote,
            commands::release_input,
            commands::cursor_pos,
            commands::config_get,
            commands::config_set,
            commands::server_history,
            commands::ws_url,
            commands::room_link,
            commands::capture_start,
            commands::capture_stop,
            commands::capture_frame,
            commands::capture_stats,
            commands::inject_probe,
            commands::capture_monitors,
        ])
        .run(tauri::generate_context!())
        .expect("启动 Tauri 应用失败");
}
