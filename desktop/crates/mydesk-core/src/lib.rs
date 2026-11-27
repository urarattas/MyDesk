//! 与网页端（MyDesk.html）共用的那部分逻辑：
//!
//! * [`config`] —— 配置文件：服务器模式 / 手动模式的切换，以及服务器地址历史
//! * [`signal`] —— `/ws` 信令协议（和 server.js 逐字段一致）
//! * [`handshake`] —— 手动模式的 `RD1…` 握手码（和网页端字节级兼容）
//! * [`input`] —— 控制通道的事件模型、限速合并与按键映射
//!
//! 这里刻意不依赖 tokio / webrtc / 任何 GUI 或平台库，保证能在任何机器上
//! `cargo test -p mydesk-core` 跑通，也方便单独复用。

pub mod config;
pub mod handshake;
pub mod input;
pub mod signal;
