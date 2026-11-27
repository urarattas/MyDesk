//! WebRTC 传输层（webrtc-rs）。
//!
//! # ⚠ 全项目唯一没有编译验证过的文件
//!
//! 写这个文件时本机没有 cargo、也没有网络，签名来自 docs.rs 的 **webrtc 0.21.1**
//! （0.20 起 webrtc-rs 换成了 async trait + builder 架构，和 0.17 及更早的经典 API
//! 完全不兼容）。第一次 `cargo build` 如果报错，按下面这张表逐条改，**其它文件不用动**：
//!
//! | 位置 | 需要确认 | 常见替代写法 |
//! | --- | --- | --- |
//! | `build_pc` | `PeerConnectionBuilder::build()` 是否 async | `.build().await?` ↔ `.build()?` |
//! | `build_pc` | `with_udp_addrs` 是否存在 | 没有就删掉那一行（部分版本默认绑 socket） |
//! | `TrackLocalStaticSample::new` | 参数顺序 / 是否需要 `MediaStreamTrack` | 见 `make_video_track` |
//! | `write_sample` | 参数个数（0.12 只有一个 &Sample） | 见 `push_frame` |
//! | `DataChannelEvent` | 变体名（OnOpen/OnMessage/OnClose） | 有些版本叫 `on_open` 等 |
//! | `RTCDataChannelInit` | `ordered`/`max_retransmits` 的类型 | 有的版本是 `Option<bool>` |
//! | 文件头的 `use` | 0.21 把协议层拆到了 `rtc` crate，模块路径可能变成 `webrtc::rtc::…` | 按 `cargo doc` 的实际路径改 `use`，其余代码不动 |
//!
//! 与之相对，`mydesk-core` 和 `app.rs` 里的协议/状态机是纯逻辑，不依赖这些签名。

use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use mydesk_core::signal::IceCandidate;
use tokio::sync::mpsc;
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::media::Sample;
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceCandidateInit, RTCIceServer, RTCPeerConnectionIceEvent, RTCSdpType,
    RTCSessionDescription,
};
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;

use crate::capture::EncodedFrame;
use crate::Error;

/// 视频轨道的 ssrc / payload type。H.264 在浏览器里的动态 PT 由 SDP 协商，
/// 这里给一个常见的 96；对端以 SDP 里的为准。
const VIDEO_SSRC: u32 = 42;

#[derive(Debug)]
pub enum PeerEvent {
    /// 本端 SDP，需要经信令转发
    LocalDescription { kind: SdpKind, sdp: String },
    /// 本端 ICE 候选，需要经信令转发
    LocalCandidate(IceCandidate),
    /// data channel 打开（`ctl` / `in`）
    ChannelOpen { label: String },
    /// `ctl` 通道上的 JSON
    Ctl(serde_json::Value),
    /// `in` 通道上的 JSON（键鼠事件）
    Input(serde_json::Value),
    /// 连接失败/关闭
    Closed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdpKind {
    Offer,
    Answer,
}

impl SdpKind {
    fn from_web(t: &str) -> Option<Self> {
        match t {
            "offer" => Some(Self::Offer),
            "answer" => Some(Self::Answer),
            _ => None,
        }
    }
    fn to_rtc(self) -> RTCSdpType {
        match self {
            Self::Offer => RTCSdpType::Offer,
            Self::Answer => RTCSdpType::Answer,
        }
    }
}

#[derive(Default)]
struct Channels {
    ctl: Mutex<Option<Arc<dyn DataChannel>>>,
    input: Mutex<Option<Arc<dyn DataChannel>>>,
}

impl Channels {
    fn slot(&self, label: &str) -> Option<&Mutex<Option<Arc<dyn DataChannel>>>> {
        match label {
            "ctl" => Some(&self.ctl),
            "in" => Some(&self.input),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct Handler {
    events: mpsc::UnboundedSender<PeerEvent>,
    channels: Arc<Channels>,
}

impl Handler {
    fn store_and_poll(&self, dc: Arc<dyn DataChannel>) {
        let label = dc.label().unwrap_or_default();
        let Some(slot) = self.channels.slot(&label) else {
            tracing::warn!(%label, "忽略未知 label 的 data channel");
            return;
        };
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(dc.clone());
        }
        let events = self.events.clone();
        let label_owned = label.clone();
        tokio::spawn(async move {
            while let Some(ev) = dc.poll().await {
                match ev {
                    DataChannelEvent::OnOpen => {
                        let _ = events.send(PeerEvent::ChannelOpen { label: label_owned.clone() });
                    }
                    DataChannelEvent::OnMessage(m) => {
                        if !m.is_string {
                            continue;
                        }
                        let text = String::from_utf8_lossy(&m.data).to_string();
                        match serde_json::from_str::<serde_json::Value>(&text) {
                            Ok(v) if label_owned == "ctl" => {
                                let _ = events.send(PeerEvent::Ctl(v));
                            }
                            Ok(v) => {
                                let _ = events.send(PeerEvent::Input(v));
                            }
                            Err(e) => tracing::debug!(error = %e, "通道上收到非 JSON 数据，已忽略"),
                        }
                    }
                    DataChannelEvent::OnClose => break,
                    _ => {}
                }
            }
            tracing::debug!(%label_owned, "data channel 结束");
        });
    }
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, ev: RTCPeerConnectionIceEvent) {
        if let Ok(init) = ev.candidate.to_json() {
            // 注意：RTCIceCandidateInit 的字段是 snake_case，而浏览器要的是
            // sdpMid / sdpMLineIndex —— 统一走 mydesk-core 里的 camelCase 结构体，
            // 不直接把 webrtc 的 serde 结果发出去。
            let candidate = IceCandidate {
                candidate: init.candidate,
                sdp_mid: init.sdp_mid,
                sdp_mline_index: init.sdp_mline_index,
                username_fragment: init.username_fragment,
            };
            let _ = self.events.send(PeerEvent::LocalCandidate(candidate));
        }
    }

    async fn on_data_channel(&self, dc: Arc<dyn DataChannel>) {
        self.store_and_poll(dc);
    }
}

pub struct Peer {
    pc: Arc<dyn PeerConnection>,
    channels: Arc<Channels>,
    video: Arc<TrackLocalStaticSample>,
    pub events: mpsc::UnboundedReceiver<PeerEvent>,
}

pub struct PeerOptions {
    /// (url, 可选 (username, credential))
    pub ice_servers: Vec<(String, Option<(String, String)>)>,
    /// 作为被控端时是否要发屏
    pub share_screen: bool,
}

impl Peer {
    /// `as_offerer` = true 表示本端创建房间（网页端里的 host）。
    /// 网页端的约定：**只有 offer 方创建 data channel 和 transceiver**，
    /// answer 方等 `ondatachannel`。照这个来才不会出现"两边各建一套通道"。
    pub async fn new(as_offerer: bool, opts: PeerOptions) -> Result<Self, Error> {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let channels = Arc::new(Channels::default());
        let handler = Arc::new(Handler { events: events_tx.clone(), channels: channels.clone() });

        // 只设 ICE 服务器；bundle/rtcp-mux 的策略构造器在不同版本里改名过，
        // 用默认值也能和浏览器互通（单视频轨 + 两个 data channel 都在同一个 bundle 里）。
        let builder = RTCConfigurationBuilder::default().with_ice_servers(
            opts.ice_servers
                .iter()
                .map(|(url, cred)| RTCIceServer {
                    urls: vec![url.clone()],
                    username: cred.as_ref().map(|c| c.0.clone()).unwrap_or_default(),
                    credential: cred.as_ref().map(|c| c.1.clone()).unwrap_or_default(),
                    ..Default::default()
                })
                .collect(),
        );
        let builder = builder.build();

        let pc: Arc<dyn PeerConnection> = PeerConnectionBuilder::new()
            .with_configuration(builder)
            .with_handler(handler.clone())
            .with_udp_addrs(vec!["0.0.0.0:0"])
            .build()
            .await
            .map_err(|e| Error::WebRtc(e.to_string()))?;

        let video = Arc::new(make_video_track()?);

        if as_offerer {
            // 两个 data channel：ctl 可靠有序，in 不可靠无序（键鼠丢了就丢了，不能排队）
            let ctl = pc
                .create_data_channel(
                    "ctl",
                    Some(RTCDataChannelInit { ordered: true, ..Default::default() }),
                )
                .await
                .map_err(|e| Error::WebRtc(e.to_string()))?;
            handler.store_and_poll(ctl);

            let input = pc
                .create_data_channel(
                    "in",
                    Some(RTCDataChannelInit {
                        ordered: false,
                        max_retransmits: Some(0),
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| Error::WebRtc(e.to_string()))?;
            handler.store_and_poll(input);

            if opts.share_screen {
                pc.add_track(video.clone() as Arc<dyn TrackLocal>)
                    .await
                    .map_err(|e| Error::WebRtc(e.to_string()))?;
            }
        }

        let this = Self { pc, channels, video, events: events_rx };
        this.watch_connection(events_tx);
        Ok(this)
    }

    fn watch_connection(&self, events: mpsc::UnboundedSender<PeerEvent>) {
        let pc = self.pc.clone();
        tokio::spawn(async move {
            let mut prev = String::new();
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                let state = format!("{:?}", pc.connection_state());
                if state != prev {
                    tracing::info!(state, "连接状态变化");
                    prev = state.clone();
                    if state.contains("Failed") || state.contains("Closed") {
                        let _ = events.send(PeerEvent::Closed(state));
                        break;
                    }
                }
            }
        });
    }

    /// 被控端：生成 offer
    pub async fn create_offer(&self) -> Result<String, Error> {
        let offer = self.pc.create_offer(None).await.map_err(|e| Error::WebRtc(e.to_string()))?;
        self.pc
            .set_local_description(offer.clone())
            .await
            .map_err(|e| Error::WebRtc(e.to_string()))?;
        Ok(offer.sdp)
    }

    /// 控制端：收到 offer 后回 answer
    pub async fn accept_offer(&self, sdp: &str) -> Result<String, Error> {
        self.pc
            .set_remote_description(RTCSessionDescription {
                sdp_type: RTCSdpType::Offer,
                sdp: sdp.to_string(),
            })
            .await
            .map_err(|e| Error::WebRtc(e.to_string()))?;
        let answer = self.pc.create_answer(None).await.map_err(|e| Error::WebRtc(e.to_string()))?;
        self.pc
            .set_local_description(answer.clone())
            .await
            .map_err(|e| Error::WebRtc(e.to_string()))?;
        Ok(answer.sdp)
    }

    /// 被控端：收到 answer
    pub async fn accept_answer(&self, sdp: &str) -> Result<(), Error> {
        self.pc
            .set_remote_description(RTCSessionDescription {
                sdp_type: RTCSdpType::Answer,
                sdp: sdp.to_string(),
            })
            .await
            .map_err(|e| Error::WebRtc(e.to_string()))
    }

    pub async fn add_remote_candidate(&self, c: &IceCandidate) -> Result<(), Error> {
        if c.candidate.is_empty() {
            return Ok(());
        }
        let init = RTCIceCandidateInit {
            candidate: c.candidate.clone(),
            sdp_mid: c.sdp_mid.clone(),
            sdp_mline_index: c.sdp_mline_index,
            username_fragment: c.username_fragment.clone(),
            ..Default::default()
        };
        self.pc.add_ice_candidate(init).await.map_err(|e| Error::WebRtc(e.to_string()))
    }

    pub async fn send_ctl(&self, value: &serde_json::Value) -> Result<(), Error> {
        send_on(&self.channels.ctl, value).await
    }

    pub async fn send_input(&self, value: &serde_json::Value) -> Result<(), Error> {
        send_on(&self.channels.input, value).await
    }

    /// 把一帧编码数据写进视频轨。Annex-B 起始码在这里去掉（RTP 不要它）。
    pub async fn push_frame(&self, frame: EncodedFrame) -> Result<(), Error> {
        let payload = strip_annexb(&frame.annexb);
        if payload.is_empty() {
            return Ok(());
        }
        let sample = Sample {
            data: bytes::Bytes::from(payload),
            duration: frame.duration,
            ..Sample::new(Instant::now())
        };
        // 0.21.1 是 write_sample(ssrc, payload_type, &sample, &[])；
        // 若你的版本只有 (&sample)，把这一行改成 self.video.write_sample(&sample).await
        self.video
            .write_sample(VIDEO_SSRC.into(), 96.into(), &sample, &[])
            .await
            .map_err(|e| Error::WebRtc(e.to_string()))
    }

    /// 把事件流取出来（`session` 里要同时用 `&Peer` 发消息和读事件）
    pub fn take_events(&mut self) -> mpsc::UnboundedReceiver<PeerEvent> {
        let (_tx, rx) = mpsc::unbounded_channel();
        std::mem::replace(&mut self.events, rx)
    }

    /// 当前本地 SDP。手动模式必须等 ICE 收集完再取，否则对端拿不到候选。
    pub fn local_sdp(&self) -> String {
        self.pc.local_description().map(|d| d.sdp).unwrap_or_default()
    }

    /// 等 ICE 收集完成（手动模式没有 trickle，必须等）
    pub async fn wait_gathering(&self, timeout: std::time::Duration) -> Result<(), Error> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.pc.ice_gathering_state() == webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState::Complete {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!("ICE 收集超时，按当前 SDP 继续");
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    pub fn has_channel(&self, label: &str) -> bool {
        self.channels
            .slot(label)
            .and_then(|m| m.lock().ok().map(|g| g.is_some()))
            .unwrap_or(false)
    }
}

async fn send_on(
    slot: &Mutex<Option<Arc<dyn DataChannel>>>,
    value: &serde_json::Value,
) -> Result<(), Error> {
    let Some(dc) = slot.lock().ok().and_then(|g| g.clone()) else {
        return Err(Error::WebRtc("通道尚未打开".into()));
    };
    let text = serde_json::to_string(value).map_err(|e| Error::WebRtc(e.to_string()))?;
    dc.send_text(&text).await.map_err(|e| Error::WebRtc(e.to_string()))
}

fn make_video_track() -> Result<TrackLocalStaticSample, Error> {
    // H.264 必须带 fmtp，否则浏览器会直接丢掉这些包（黑屏的经典原因）
    let capability = RTCRtpCodecCapability {
        mime_type: "video/H264".to_string(),
        clock_rate: 90000,
        channels: 0,
        sdp_fmtp_line:
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_string(),
        rtcp_feedback: vec![],
    };
    let track = TrackLocalStaticSample::new(
        capability,
        "video".to_string(),
        "mydesk".to_string(),
    );
    Ok(track)
}

/// 去掉 Annex-B 起始码：RTP 里的 H.264 负载不带 4 字节起始码
pub fn strip_annexb(data: &[u8]) -> Vec<u8> {
    let nals = crate::capture::split_annexb(data);
    if nals.is_empty() {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    for nal in nals {
        out.extend_from_slice(nal);
    }
    out
}

#[allow(dead_code)]
fn kind_of(sdp_kind: SdpKind) -> RTCSdpType {
    sdp_kind.to_rtc()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdp_kind_parses_web_strings() {
        assert_eq!(SdpKind::from_web("offer"), Some(SdpKind::Offer));
        assert_eq!(SdpKind::from_web("answer"), Some(SdpKind::Answer));
        assert_eq!(SdpKind::from_web("pranswer"), None);
    }

    #[test]
    fn strip_drops_start_codes() {
        let data = vec![0u8, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0xce];
        assert_eq!(strip_annexb(&data), vec![0x67, 0x42, 0x68, 0xce]);
    }
}
