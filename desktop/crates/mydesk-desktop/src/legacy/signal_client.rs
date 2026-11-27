//! 信令客户端：连 `/ws`，收发 [`mydesk_core::signal`] 里的消息。
//!
//! 只做传输，不懂 WebRTC。上层拿到的是一串 [`SignalEvent`]。
//! 服务端每 30 秒发一次 ping，tungstenite 会自动回 pong，这里无需处理。

use futures_util::{SinkExt, StreamExt};
use mydesk_core::signal::{ClientMsg, ServerMsg};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug)]
pub enum SignalEvent {
    Open,
    Message(ServerMsg),
    /// 连接结束；`Some` 表示是错误导致的
    Closed(Option<String>),
}

#[derive(Debug, thiserror::Error)]
pub enum SignalError {
    #[error("WebSocket 连接失败: {0}")]
    Connect(String),
    #[error("信令消息收发失败: {0}")]
    Io(String),
    #[error("服务端消息无法解析: {0}")]
    Protocol(String),
}

/// 持有发送端；后台任务负责读写和重连（重连由上层决定，这里只报告 Closed）。
pub struct SignalClient {
    tx: mpsc::UnboundedSender<ClientMsg>,
    events: mpsc::UnboundedReceiver<SignalEvent>,
}

impl SignalClient {
    /// `ws_url` 形如 `wss://host/ws`（由 [`mydesk_core::signal::ws_url`] 生成）
    pub async fn connect(ws_url: &str) -> Result<Self, SignalError> {
        let (ws, _resp) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| SignalError::Connect(e.to_string()))?;
        let (mut sink, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ClientMsg>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<SignalEvent>();

        let _ = ev_tx.send(SignalEvent::Open);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    outgoing = out_rx.recv() => {
                        let Some(msg) = outgoing else { break };
                        let json = match serde_json::to_string(&msg) {
                            Ok(j) => j,
                            Err(e) => {
                                let _ = ev_tx.send(SignalEvent::Closed(Some(e.to_string())));
                                break;
                            }
                        };
                        if let Err(e) = sink.send(Message::Text(json.into())).await {
                            let _ = ev_tx.send(SignalEvent::Closed(Some(e.to_string())));
                            break;
                        }
                    }
                    incoming = stream.next() => {
                        match incoming {
                            Some(Ok(Message::Text(text))) => {
                                match serde_json::from_str::<ServerMsg>(text.as_str()) {
                                    Ok(msg) => { let _ = ev_tx.send(SignalEvent::Message(msg)); }
                                    Err(e) => {
                                        // 未知/畸形消息不该拖垮整条连接
                                        tracing::warn!(error = %e, raw = %text.as_str(), "忽略无法解析的信令消息");
                                    }
                                }
                            }
                            Some(Ok(Message::Close(_))) => {
                                let _ = ev_tx.send(SignalEvent::Closed(None));
                                break;
                            }
                            Some(Ok(_)) => {}
                            Some(Err(e)) => {
                                let _ = ev_tx.send(SignalEvent::Closed(Some(e.to_string())));
                                break;
                            }
                            None => {
                                let _ = ev_tx.send(SignalEvent::Closed(None));
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(Self { tx: out_tx, events: ev_rx })
    }

    pub fn send(&self, msg: ClientMsg) -> Result<(), SignalError> {
        self.tx.send(msg).map_err(|e| SignalError::Io(e.to_string()))
    }

    pub async fn next(&mut self) -> Option<SignalEvent> {
        self.events.recv().await
    }

    pub fn try_next(&mut self) -> Option<SignalEvent> {
        self.events.try_recv().ok()
    }
}
