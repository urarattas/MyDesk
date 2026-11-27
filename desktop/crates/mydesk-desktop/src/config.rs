//! 配置文件的薄封装：路径解析 + 局部更新（patch）。
//!
//! 真正的结构体与默认值在 `mydesk-core` 里，这里只负责「读 → 改 → 落盘」。

use std::path::PathBuf;

use mydesk_core::config::{Config, ConnectMode};
use serde::{Deserialize, Serialize};

/// 前端一次性提交的改动（全部可选）
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ConfigPatch {
    pub mode: Option<String>,
    pub server: Option<String>,
    pub room: Option<String>,
    pub manual_code: Option<String>,
    pub allow_remote: Option<bool>,
    pub max_hz: Option<u32>,
    pub monitor: Option<usize>,
    pub max_fps: Option<u32>,
    pub bitrate_kbps: Option<u32>,
    pub codec: Option<String>,
    /// 直接写死的 STUN 列表（逗号或换行分隔）
    pub ice_urls: Option<String>,
}

pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("MYDESK_CONFIG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    mydesk_core::config::default_path()
}

pub fn load() -> Result<Config, String> {
    Config::load(&path()).map_err(|e| e.to_string())
}

pub fn save(cfg: &Config) -> Result<(), String> {
    cfg.save(&path()).map_err(|e| e.to_string())
}

/// 把 patch 应用到配置上（不落盘）
pub fn apply(cfg: &mut Config, patch: &ConfigPatch) -> Result<(), String> {
    if let Some(mode) = &patch.mode {
        cfg.mode = ConnectMode::parse(mode).ok_or_else(|| format!("未知模式: {mode}"))?;
    }
    if let Some(server) = &patch.server {
        if !server.trim().is_empty() {
            cfg.remember_server(server).map_err(|e| e.to_string())?;
        }
    }
    if let Some(room) = &patch.room {
        cfg.server.room = room.trim().to_ascii_uppercase();
    }
    if let Some(code) = &patch.manual_code {
        cfg.manual.code = code.trim().to_string();
    }
    if let Some(allowed) = patch.allow_remote {
        cfg.input.allow_remote = allowed;
    }
    if let Some(hz) = patch.max_hz {
        cfg.input.max_hz = hz;
    }
    if let Some(monitor) = patch.monitor {
        cfg.media.monitor = monitor;
    }
    if let Some(fps) = patch.max_fps {
        cfg.media.max_fps = fps;
    }
    if let Some(kbps) = patch.bitrate_kbps {
        cfg.media.bitrate_kbps = kbps;
    }
    if let Some(codec) = &patch.codec {
        cfg.media.codec = codec.trim().to_string();
    }
    if let Some(list) = &patch.ice_urls {
        let urls: Vec<String> = list
            .split(|c| c == ',' || c == '\n' || c == '\r')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        if !urls.is_empty() {
            cfg.ice.urls = urls;
        }
    }
    Ok(())
}

/// 给前端的配置视图（比 TOML 更适合直接塞进表单）
#[derive(Debug, Serialize)]
pub struct ConfigView {
    pub path: String,
    pub mode: String,
    pub server: String,
    pub recent: Vec<String>,
    pub room: String,
    pub manual_code: String,
    pub allow_remote: bool,
    pub max_hz: u32,
    pub codec: String,
    pub max_fps: u32,
    pub bitrate_kbps: u32,
    pub monitor: usize,
    pub ice_urls: String,
}

impl ConfigView {
    pub fn new(cfg: &Config) -> Self {
        Self {
            path: path().display().to_string(),
            mode: cfg.mode.as_str().to_string(),
            server: cfg.server.url.clone(),
            recent: cfg.server.recent.clone(),
            room: cfg.server.room.clone(),
            manual_code: cfg.manual.code.clone(),
            allow_remote: cfg.input.allow_remote,
            max_hz: cfg.input.max_hz,
            codec: cfg.media.codec.clone(),
            max_fps: cfg.media.max_fps,
            bitrate_kbps: cfg.media.bitrate_kbps,
            monitor: cfg.media.monitor,
            ice_urls: cfg.ice.urls.join("\n"),
        }
    }
}
