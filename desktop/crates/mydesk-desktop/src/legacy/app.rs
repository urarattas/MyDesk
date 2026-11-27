//! 会话编排：信令 ↔ WebRTC ↔ 截屏编码 ↔ 键鼠注入。
//!
//! 线程模型（会阻塞的东西各占一个线程，tokio 里只做 IO）：
//!
//! ```text
//!  tokio          : 信令 / WebRTC / 事件循环
//!  capture (std)  : xcap 截屏 + openh264 编码（xcap 是阻塞调用）
//!  inject  (std)  : enigo 注入（不保证 Send，独立线程最省事）
//! ```

use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use mydesk_core::config::Config;
use mydesk_core::handshake::{self, Handshake, SdpKind as HandshakeKind};
use mydesk_core::input::InputEvent;
use mydesk_core::signal::{self, ClientMsg, ServerMsg};
use tokio::sync::mpsc;

use crate::capture::{EncodedFrame, ScreenCapturer, VideoEncoder};
use crate::inject::Injector;
use crate::signal_client::{SignalClient, SignalEvent};
use crate::webrtc_peer::{Peer, PeerEvent, PeerOptions, SdpKind};
use crate::Error;

pub enum Action {
    /// 创建房间（本端被控，发屏）
    Create,
    /// 用房间号加入（本端控制端）
    Join(String),
    /// 手动模式：粘贴对端的邀请码
    AcceptOffer(String),
    /// 手动模式：生成邀请码并等回执码
    CreateOffer,
}

enum InjectCmd {
    Event(InputEvent),
    Release,
}

fn spawn_injector(enabled: bool) -> Result<std_mpsc::Sender<InjectCmd>, Error> {
    let (tx, rx) = std_mpsc::channel::<InjectCmd>();
    let mut injector = Injector::new(enabled)?;
    std::thread::Builder::new()
        .name("mydesk-inject".into())
        .spawn(move || {
            for cmd in rx {
                match cmd {
                    InjectCmd::Event(ev) => {
                        if let Err(e) = injector.apply(ev) {
                            tracing::debug!(error = %e, "注入失败");
                        }
                    }
                    InjectCmd::Release => injector.release_all(),
                }
            }
        })
        .map_err(|e| Error::Other(format!("启动注入线程失败: {e}")))?;
    Ok(tx)
}

fn spawn_capture(cfg: &Config) -> Result<mpsc::Receiver<EncodedFrame>, Error> {
    if cfg.media.codec != "h264" {
        tracing::warn!(codec = %cfg.media.codec, "目前只实现 H.264 编码，忽略该配置项");
    }
    let mut capturer = ScreenCapturer::open(cfg.media.monitor, cfg.media.max_fps)?;
    let mut encoder = VideoEncoder::new(capturer.width, capturer.height, capturer.fps())?;
    tracing::info!(
        width = capturer.width,
        height = capturer.height,
        fps = capturer.fps(),
        bitrate_kbps = cfg.media.bitrate_kbps,
        "开始共享屏幕"
    );
    // 有界通道：编码跟不上时宁可丢帧（低延迟优先），不能把内存堆爆
    let (tx, rx) = mpsc::channel::<EncodedFrame>(2);
    std::thread::Builder::new()
        .name("mydesk-capture".into())
        .spawn(move || loop {
            let rgba = match capturer.capture_rgba() {
                Ok(px) => px,
                Err(e) => {
                    tracing::warn!(error = %e, "截屏失败");
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
            };
            match encoder.encode_rgba(&rgba, std::time::Instant::now()) {
                Ok(Some(frame)) => {
                    if tx.blocking_send(frame).is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "编码失败");
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        })
        .map_err(|e| Error::Other(format!("启动采集线程失败: {e}")))?;
    Ok(rx)
}

fn ice_servers(cfg: &Config) -> Vec<(String, Option<(String, String)>)> {
    let mut out: Vec<(String, Option<(String, String)>)> = cfg
        .ice
        .urls
        .iter()
        .filter(|u| !u.trim().is_empty())
        .map(|u| (u.trim().to_string(), None))
        .collect();
    if let Some(turn) = &cfg.ice.turn {
        if !turn.url.trim().is_empty() {
            out.push((
                turn.url.trim().to_string(),
                Some((turn.username.clone(), turn.password.clone())),
            ));
        }
    }
    out
}

pub async fn run(cfg: Config, action: Action) -> Result<(), Error> {
    match action {
        Action::Create => run_server(cfg, None).await,
        Action::Join(room) => run_server(cfg, Some(room)).await,
        Action::AcceptOffer(code) => run_manual_answer(cfg, code).await,
        Action::CreateOffer => run_manual_offer(cfg).await,
    }
}

/* ------------------------------ 服务器模式 ------------------------------ */

async fn run_server(cfg: Config, join_room: Option<String>) -> Result<(), Error> {
    let server = cfg
        .effective_server()
        .ok_or_else(|| {
            Error::Other("还没有配置信令服务器：用 --server <URL> 指定一次，之后会记住".into())
        })?
        .to_string();
    let ws_url = signal::ws_url(&server).map_err(Error::RoomCode)?;
    tracing::info!(%ws_url, "连接信令服务器");
    let mut signal = SignalClient::connect(&ws_url).await?;

    match &join_room {
        Some(room) => {
            let code = signal::normalize_room_code(room)
                .ok_or_else(|| Error::Other("房间号应为 6 位（A-Z / 2-9，不含 0 O 1 I）".into()))?;
            signal.send(ClientMsg::Join { code: code.clone() })?;
            tracing::info!(%code, "已请求加入房间");
        }
        None => signal.send(ClientMsg::Create)?,
    }

    // 等服务端给出"可以开始协商"的信号：host 收 Peer，guest 收 Offer。
    // 注意 host 可能在 offer 之前就开始 trickle ICE，这些候选必须排队，
    // 否则对端拿不到本端候选（网页端里就是 SRV.pendingIce）。
    let mut pending_offer: Option<String> = None;
    let mut early_ice: Vec<mydesk_core::signal::IceCandidate> = Vec::new();
    loop {
        let Some(ev) = signal.next().await else {
            return Err(Error::Other("信令连接已关闭".into()));
        };
        match ev {
            SignalEvent::Open => {}
            SignalEvent::Closed(reason) => {
                return Err(Error::Other(format!(
                    "信令断开: {}",
                    reason.unwrap_or_else(|| "对端关闭".into())
                )))
            }
            SignalEvent::Message(msg) => match msg {
                ServerMsg::Room { code } => {
                    let link = signal::room_link(&server, &code).unwrap_or_default();
                    println!("\n房间已创建：{code}");
                    println!("把链接发给对方：{link}");
                    if cfg.mode == mydesk_core::config::ConnectMode::Manual {
                        println!("（当前是手动模式配置；本次仍然走信令服务器）");
                    }
                    println!("等待对方加入…\n");
                }
                ServerMsg::Joined { code } => {
                    tracing::info!(%code, "已进入房间，等待对方发起");
                }
                ServerMsg::Peer => {
                    tracing::info!("对方已加入，开始协商（本端发屏）");
                    break;
                }
                ServerMsg::Offer { sdp } => {
                    pending_offer = Some(sdp);
                    break;
                }
                ServerMsg::Ice { candidate } => {
                    if early_ice.len() < 64 {
                        early_ice.push(candidate);
                    }
                }
                ServerMsg::Error { message } => return Err(Error::Other(message)),
                ServerMsg::PeerLeft => tracing::warn!("对方离开了房间"),
                ServerMsg::Bye => tracing::warn!("对方断开了连接"),
                other => tracing::debug!(?other, "忽略的信令消息"),
            },
        }
    }

    let offerer = pending_offer.is_none();
    let peer = Peer::new(
        offerer,
        PeerOptions { ice_servers: ice_servers(&cfg), share_screen: offerer },
    )
    .await?;

    if offerer {
        let sdp = peer.create_offer().await?;
        signal.send(ClientMsg::Offer { sdp })?;
        session(cfg, Some(signal), peer, true).await
    } else {
        let answer = peer.accept_offer(&pending_offer.unwrap_or_default()).await?;
        signal.send(ClientMsg::Answer { sdp: answer })?;
        // 补投 offer 之前就收到的候选
        for candidate in early_ice {
            if let Err(e) = peer.add_remote_candidate(&candidate).await {
                tracing::debug!(error = %e, "补投早期候选失败");
            }
        }
        session(cfg, Some(signal), peer, false).await
    }
}

/* ------------------------------- 手动模式 ------------------------------- */

async fn read_line() -> Result<String, Error> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        Ok::<String, std::io::Error>(line)
    })
    .await
    .map_err(|e| Error::Other(format!("读取输入失败: {e}")))?
    .map_err(|e| Error::Other(format!("读取输入失败: {e}")))
}

/// 本端生成邀请码，然后把对方发回的回执码粘进来。
async fn run_manual_offer(cfg: Config) -> Result<(), Error> {
    let peer = Peer::new(true, PeerOptions { ice_servers: ice_servers(&cfg), share_screen: true }).await?;
    let _ = peer.create_offer().await?;
    peer.wait_gathering(Duration::from_secs(10)).await?;
    let code = handshake::pack(&Handshake::new(HandshakeKind::Offer, peer.local_sdp()))?;
    println!("\n=== 邀请码（整行发给对方）===\n\n{code}\n");
    println!("把对方回给你的回执码粘到下面，回车继续：");
    let answer = read_line().await?;
    let hs = handshake::unpack(&answer)?;
    if hs.kind != HandshakeKind::Answer {
        return Err(Error::Other("这不是回执码（answer）".into()));
    }
    peer.accept_answer(&hs.sdp).await?;
    session(cfg, None, peer, true).await
}

/// 本端粘贴对方的邀请码，生成回执码让对端粘回去。
async fn run_manual_answer(cfg: Config, code: String) -> Result<(), Error> {
    let hs = handshake::unpack(&code)?;
    if hs.kind != HandshakeKind::Offer {
        return Err(Error::Other("这是回执码（answer），本端需要的是对方的邀请码".into()));
    }
    let peer = Peer::new(false, PeerOptions { ice_servers: ice_servers(&cfg), share_screen: false }).await?;
    let _ = peer.accept_offer(&hs.sdp).await?;
    peer.wait_gathering(Duration::from_secs(10)).await?;
    let reply = handshake::pack(&Handshake::new(HandshakeKind::Answer, peer.local_sdp()))?;
    println!("\n=== 回执码（整行发给对方）===\n\n{reply}\n");
    println!("等对方粘贴后连接会自动建立…\n");
    session(cfg, None, peer, false).await
}

/* ------------------------------- 主循环 ------------------------------- */

async fn next_signal(signal: &mut Option<SignalClient>) -> Option<SignalEvent> {
    match signal {
        Some(s) => s.next().await,
        None => std::future::pending().await,
    }
}

async fn next_frame(frames: &mut Option<mpsc::Receiver<EncodedFrame>>) -> Option<EncodedFrame> {
    match frames {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn session(
    cfg: Config,
    signal: Option<SignalClient>,
    mut peer: Peer,
    share_screen: bool,
) -> Result<(), Error> {
    let mut signal = signal;
    let inject_tx = spawn_injector(cfg.input.allow_remote)?;
    let mut frames = if share_screen { Some(spawn_capture(&cfg)?) } else { None };
    let mut events = peer.take_events();
    let mut hello_sent = false;

    loop {
        tokio::select! {
            sig = next_signal(&mut signal) => {
                match sig {
                    None => { tracing::info!("信令结束"); break; }
                    Some(SignalEvent::Open) => {}
                    Some(SignalEvent::Closed(reason)) => {
                        tracing::warn!(?reason, "信令关闭");
                        break;
                    }
                    Some(SignalEvent::Message(msg)) => match msg {
                        ServerMsg::Answer { sdp } => {
                            if let Err(e) = peer.accept_answer(&sdp).await {
                                tracing::warn!(error = %e, "设置远端 answer 失败");
                            }
                        }
                        ServerMsg::Ice { candidate } => {
                            if let Err(e) = peer.add_remote_candidate(&candidate).await {
                                tracing::debug!(error = %e, "添加远端候选失败");
                            }
                        }
                        ServerMsg::Bye | ServerMsg::PeerLeft => {
                            tracing::warn!("对方断开，结束会话");
                            break;
                        }
                        ServerMsg::Error { message } => {
                            tracing::error!(%message, "服务端报错");
                            break;
                        }
                        _ => {}
                    },
                }
            }
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    PeerEvent::LocalDescription { kind, sdp } => {
                        if let Some(sig) = signal.as_ref() {
                            let msg = match kind {
                                SdpKind::Offer => ClientMsg::Offer { sdp },
                                SdpKind::Answer => ClientMsg::Answer { sdp },
                            };
                            let _ = sig.send(msg);
                        }
                    }
                    PeerEvent::LocalCandidate(candidate) => {
                        if let Some(sig) = signal.as_ref() {
                            let _ = sig.send(ClientMsg::Ice { candidate });
                        }
                    }
                    PeerEvent::ChannelOpen { label } => {
                        tracing::info!(%label, "数据通道已打开");
                        if label == "ctl" && !hello_sent {
                            hello_sent = true;
                            send_hello(&peer, &cfg).await;
                        }
                    }
                    PeerEvent::Ctl(value) => {
                        if let Err(e) = on_ctl(&peer, &inject_tx, &cfg, value).await {
                            tracing::debug!(error = %e, "处理控制消息失败");
                        }
                    }
                    PeerEvent::Input(value) => {
                        match serde_json::from_value::<InputEvent>(value) {
                            Ok(ev) => { let _ = inject_tx.send(InjectCmd::Event(ev)); }
                            Err(e) => tracing::debug!(error = %e, "无法解析输入事件"),
                        }
                    }
                    PeerEvent::Closed(reason) => {
                        tracing::warn!(%reason, "WebRTC 连接结束");
                        break;
                    }
                }
            }
            frame = next_frame(&mut frames) => {
                if let Some(frame) = frame {
                    if let Err(e) = peer.push_frame(frame).await {
                        tracing::debug!(error = %e, "发送视频帧失败");
                    }
                }
            }
        }
    }

    let _ = inject_tx.send(InjectCmd::Release);
    Ok(())
}

async fn send_hello(peer: &Peer, cfg: &Config) {
    let hello = serde_json::json!({
        "t": "hello",
        "name": "MyDesk Rust",
        "ua": format!("mydesk-desktop/{}", env!("CARGO_PKG_VERSION")),
        "allow": cfg.input.allow_remote,
    });
    let _ = peer.send_ctl(&hello).await;
    // 告知对端：本机不需要 bridge（原生注入），免得页面提示"对方桥接未连接"
    let _ = peer.send_ctl(&serde_json::json!({"t": "bridge", "ok": true})).await;
    let _ = peer.send_ctl(&serde_json::json!({"t": "perm", "allow": cfg.input.allow_remote})).await;
    // 编码槽位：网页端据此显示 rxSlots[0]
    let _ = peer.send_ctl(&serde_json::json!({"t": "codec", "i": 0})).await;
}

async fn on_ctl(
    peer: &Peer,
    inject_tx: &std_mpsc::Sender<InjectCmd>,
    cfg: &Config,
    value: serde_json::Value,
) -> Result<(), Error> {
    let kind = value.get("t").and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        "hello" => {
            tracing::info!(
                peer = %value.get("name").and_then(|v| v.as_str()).unwrap_or("未知设备"),
                "对方已就绪"
            );
            let _ = peer
                .send_ctl(&serde_json::json!({"t": "perm", "allow": cfg.input.allow_remote}))
                .await;
        }
        "perm" => {
            let allow = value.get("allow").and_then(|v| v.as_bool()).unwrap_or(false);
            tracing::info!(allow, "对方的控制权限状态");
        }
        "ask" => {
            tracing::warn!(
                "对方请求控制本机；当前 --allow-remote = {}",
                if cfg.input.allow_remote { "on" } else { "off（重启并带 --allow-remote on）" }
            );
            let _ = peer
                .send_ctl(&serde_json::json!({"t": "perm", "allow": cfg.input.allow_remote}))
                .await;
        }
        "ping" => {
            let id = value.get("id").cloned().unwrap_or(serde_json::Value::Null);
            let _ = peer.send_ctl(&serde_json::json!({"t": "pong", "id": id})).await;
        }
        "codec" => tracing::info!(slot = ?value.get("i"), "对端切换了编码槽位"),
        "clip-req" | "clip-set" | "clip-data" => {
            let _ = peer
                .send_ctl(&serde_json::json!({"t": "clip-fail", "error": "Rust 端暂未实现剪贴板"}))
                .await;
        }
        "bye" => {
            tracing::info!("对方结束会话");
            let _ = inject_tx.send(InjectCmd::Release);
        }
        other => tracing::debug!(other, "未知控制消息"),
    }
    Ok(())
}
