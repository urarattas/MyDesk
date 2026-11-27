//! `config.toml`：决定走信令服务器还是手动握手，并记住用过的服务器地址。
//!
//! 默认位置（可用 `MYDESK_CONFIG` 或 `--config` 覆盖）：
//!
//! * Windows: `%APPDATA%\MyDesk\config.toml`
//! * 其它:    `$XDG_CONFIG_HOME/mydesk/config.toml`，退化为 `~/.config/mydesk/config.toml`

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `server.recent` 最多保留多少个历史地址
pub const RECENT_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectMode {
    /// 走信令服务器：创建房间 / 用房间号加入
    #[default]
    Server,
    /// 双方交换一次握手码，零基础设施
    Manual,
}

impl ConnectMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "server" | "srv" => Some(Self::Server),
            "manual" | "p2p" | "offline" => Some(Self::Manual),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// 最近一次使用的信令服务器地址，例如 `https://desk.example.com`
    pub url: String,
    /// 历史地址，最新的在前（`remember_server` 自动维护）
    pub recent: Vec<String>,
    /// 加入房间用的 6 位房间号；留空表示本端创建房间
    pub room: String,
    /// 连不上服务器时自动退回手动模式（需要手动交换握手码）
    pub fallback_to_manual: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ManualConfig {
    /// 手动模式：粘贴对方的邀请码（`RD1…`），或在创建后把自己生成的码填进去
    pub code: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IceServer {
    pub url: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IceConfig {
    /// STUN 列表。默认只给一个公共节点：STUN 会看到你的公网 IP，
    /// 想换/加自建节点直接改这里。
    pub urls: Vec<String>,
    /// 可选的 TURN（对称 NAT 的兜底）。密码只保存在本文件里。
    pub turn: Option<IceServer>,
}

impl Default for IceConfig {
    fn default() -> Self {
        Self {
            urls: vec!["stun:stun.cloudflare.com:3478".to_string()],
            turn: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaConfig {
    /// 目前 Rust 端只实现 H.264（对端是网页时请把「发送编码」也设成 H.264）
    pub codec: String,
    pub max_fps: u32,
    pub bitrate_kbps: u32,
    /// 共享第几块屏幕（0 = 主屏）
    pub monitor: usize,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            codec: "h264".to_string(),
            max_fps: 60,
            bitrate_kbps: 6000,
            monitor: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    /// 是否允许对方控制本机。默认关闭：和网页端一样，必须显式授权。
    pub allow_remote: bool,
    /// 本机鼠标采集上限（控制端），单位 Hz。原生实现不再受浏览器的
    /// requestAnimationFrame（约 60–120 Hz）与 HTTP 桥接批处理的限制。
    pub max_hz: u32,
    /// 是否同步剪贴板（默认关闭）
    pub clipboard: bool,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            allow_remote: false,
            max_hz: 1000,
            clipboard: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mode: ConnectMode,
    pub server: ServerConfig,
    pub manual: ManualConfig,
    pub ice: IceConfig,
    pub media: MediaConfig,
    pub input: InputConfig,
}

impl Config {
    /// 读配置；文件不存在时返回默认值（不报错，首次运行就是这种情况）。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(ConfigError::Read { path: path.to_path_buf(), source }),
        };
        toml::from_str(&text).map_err(|source| ConfigError::Parse { path: path.to_path_buf(), source })
    }

    pub fn load_default() -> Result<Self, ConfigError> {
        Self::load(&default_path())
    }

    /// 原子写：先写临时文件再 rename，避免掉电留下半截配置。
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir)
                    .map_err(|source| ConfigError::Write { path: dir.to_path_buf(), source })?;
            }
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::Serialize(e.to_string()))?;
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text).map_err(|source| ConfigError::Write { path: tmp.clone(), source })?;
        fs::rename(&tmp, path).map_err(|source| ConfigError::Write { path: path.to_path_buf(), source })
    }

    /// 记录一次使用的服务器地址：设为当前 + 进历史（去重、最新在前、封顶）。
    pub fn remember_server(&mut self, url: &str) -> Result<String, ConfigError> {
        let normalized = normalize_server_url(url)?;
        self.server.url = normalized.clone();
        self.server.recent.retain(|u| u != &normalized);
        self.server.recent.insert(0, normalized.clone());
        self.server.recent.truncate(RECENT_LIMIT);
        Ok(normalized)
    }

    /// 当前模式下的服务器地址（手动模式可能为空）
    pub fn effective_server(&self) -> Option<&str> {
        let url = self.server.url.trim();
        if url.is_empty() {
            None
        } else {
            Some(url)
        }
    }
}

/// `https://host`、`host:8080`、`ssh://…` 一律规整成带 scheme 的 http(s) 地址。
pub fn normalize_server_url(input: &str) -> Result<String, ConfigError> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(ConfigError::EmptyServerUrl);
    }
    if raw.contains(char::is_whitespace) {
        return Err(ConfigError::BadServerUrl(raw.to_string()));
    }
    let with_scheme = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else if raw.starts_with("ws://") {
        format!("http://{}", &raw[5..])
    } else if raw.starts_with("wss://") {
        format!("https://{}", &raw[6..])
    } else {
        format!("https://{raw}")
    };
    let (scheme, rest) = with_scheme
        .split_once("://")
        .ok_or_else(|| ConfigError::BadServerUrl(raw.to_string()))?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || scheme.is_empty() {
        return Err(ConfigError::BadServerUrl(raw.to_string()));
    }
    Ok(format!("{scheme}://{host}"))
}

pub fn default_path() -> PathBuf {
    if let Some(p) = env::var_os("MYDESK_CONFIG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    platform_config_path()
}

#[cfg(windows)]
fn platform_config_path() -> PathBuf {
    if let Some(appdata) = env::var_os("APPDATA") {
        if !appdata.is_empty() {
            return PathBuf::from(appdata).join("MyDesk").join("config.toml");
        }
    }
    PathBuf::from("config.toml")
}

#[cfg(not(windows))]
fn platform_config_path() -> PathBuf {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("mydesk").join("config.toml");
        }
    }
    if let Some(home) = env::var_os("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".config").join("mydesk").join("config.toml");
        }
    }
    PathBuf::from("config.toml")
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("读取配置失败 {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("解析配置失败 {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("写入配置失败 {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("序列化配置失败: {0}")]
    Serialize(String),
    #[error("服务器地址为空")]
    EmptyServerUrl,
    #[error("服务器地址无法识别: {0}")]
    BadServerUrl(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
mode = "manual"

[server]
url = "https://desk.example.com"
room = "ABC123"
recent = ["https://desk.example.com"]

[ice]
urls = ["stun:stun.cloudflare.com:3478"]

[ice.turn]
url = "turn:turn.example.com:3478"
username = "u"
password = "p"

[media]
max_fps = 30

[input]
allow_remote = true
"#;

    #[test]
    fn parses_modes_and_sections() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.mode, ConnectMode::Manual);
        assert_eq!(cfg.server.url, "https://desk.example.com");
        assert_eq!(cfg.server.room, "ABC123");
        let turn = cfg.ice.turn.as_ref().unwrap();
        assert_eq!(turn.url, "turn:turn.example.com:3478");
        assert_eq!(cfg.media.max_fps, 30);
        // 没写的字段落默认值
        assert_eq!(cfg.media.bitrate_kbps, 6000);
        assert_eq!(cfg.media.codec, "h264");
        assert!(cfg.input.allow_remote);
        assert_eq!(cfg.input.max_hz, 1000);
        // 剪贴板默认关闭
        assert!(!cfg.input.clipboard);
    }

    #[test]
    fn defaults_are_safe() {
        let cfg = Config::default();
        assert_eq!(cfg.mode, ConnectMode::Server);
        assert!(!cfg.input.allow_remote, "远程控制必须显式开启");
        assert!(cfg.effective_server().is_none());
        assert_eq!(cfg.ice.urls.len(), 1);
    }

    #[test]
    fn remembers_servers_most_recent_first_without_duplicates() {
        let mut cfg = Config::default();
        cfg.remember_server("https://a.example").unwrap();
        cfg.remember_server("https://b.example").unwrap();
        cfg.remember_server("https://a.example").unwrap();
        assert_eq!(cfg.server.url, "https://a.example");
        assert_eq!(cfg.server.recent, vec!["https://a.example", "https://b.example"]);

        for i in 0..RECENT_LIMIT + 4 {
            cfg.remember_server(&format!("https://h{i}.example")).unwrap();
        }
        assert_eq!(cfg.server.recent.len(), RECENT_LIMIT);
        assert_eq!(cfg.server.recent[0], format!("https://h{}.example", RECENT_LIMIT + 3));
    }

    #[test]
    fn normalizes_server_urls() {
        assert_eq!(normalize_server_url("desk.example.com").unwrap(), "https://desk.example.com");
        assert_eq!(normalize_server_url("  http://192.168.1.8:8080/  ").unwrap(), "http://192.168.1.8:8080");
        assert_eq!(normalize_server_url("wss://desk.example.com/ws").unwrap(), "https://desk.example.com");
        assert_eq!(normalize_server_url("https://desk.example.com/app/").unwrap(), "https://desk.example.com");
        assert!(normalize_server_url("   ").is_err());
        assert!(normalize_server_url("desk example com").is_err());
    }

    #[test]
    fn round_trips_through_disk() {
        // 写 crate 自己的 target/ 而不是系统临时目录：容器/受限环境下 temp 可能不可写
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!("cfg-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut cfg = Config::default();
        cfg.mode = ConnectMode::Manual;
        cfg.remember_server("https://desk.example.com").unwrap();
        cfg.server.room = "ZZZZZZ".into();
        cfg.save(&path).unwrap();

        let back = Config::load(&path).unwrap();
        assert_eq!(back.mode, ConnectMode::Manual);
        assert_eq!(back.server.url, "https://desk.example.com");
        assert_eq!(back.server.recent, vec!["https://desk.example.com"]);
        assert_eq!(back.server.room, "ZZZZZZ");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("mydesk-does-not-exist-xyz.toml");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.mode, ConnectMode::Server);
    }
}
