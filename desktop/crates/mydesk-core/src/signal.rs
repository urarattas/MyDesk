//! `/ws` 信令协议。字段名和 server.js 里的 JSON 完全一致，改这里等于改协议。

use serde::{Deserialize, Serialize};

/// 房间码字母表，与服务端保持一致（去掉容易看错的 0/O/1/I）
pub const ROOM_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub const ROOM_LEN: usize = 8;

/// 客户端 -> 服务端
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClientMsg {
    Create,
    Join { code: String },
    Offer { sdp: String },
    Answer { sdp: String },
    Ice { candidate: IceCandidate },
    Bye,
    #[serde(other)]
    Unknown,
}

/// 服务端 -> 客户端（其中 offer/answer/ice/bye 是原样转发对端的消息）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ServerMsg {
    Room { code: String },
    Joined { code: String },
    Peer,
    Offer { sdp: String },
    Answer { sdp: String },
    Ice { candidate: IceCandidate },
    Bye,
    #[serde(rename = "peer-left")]
    PeerLeft,
    Error { message: String },
    #[serde(other)]
    Unknown,
}

/// `RTCIceCandidateInit.toJSON()` 的形状（网页端直接把它塞进 `{type:"ice",candidate}`）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IceCandidate {
    #[serde(default)]
    pub candidate: String,
    #[serde(default, rename = "sdpMid")]
    pub sdp_mid: Option<String>,
    #[serde(default, rename = "sdpMLineIndex")]
    pub sdp_mline_index: Option<u16>,
    #[serde(default, rename = "usernameFragment")]
    pub username_fragment: Option<String>,
}

impl IceCandidate {
    pub fn new(candidate: impl Into<String>, sdp_mid: Option<String>, index: Option<u16>) -> Self {
        Self {
            candidate: candidate.into(),
            sdp_mid,
            sdp_mline_index: index,
            username_fragment: None,
        }
    }
}

/// `wss://host/ws`。和网页端一样只取 origin：页面里的 wsUrl() 就是
/// `location.host + "/ws"`，所以这里也丢掉路径，保证行为一致。
pub fn ws_url(server: &str) -> Result<String, SignalError> {
    let raw = server.trim();
    if raw.is_empty() {
        return Err(SignalError::EmptyServer);
    }
    let (scheme, rest) = if let Some(r) = raw.strip_prefix("https://") {
        ("wss", r)
    } else if let Some(r) = raw.strip_prefix("http://") {
        ("ws", r)
    } else if let Some(r) = raw.strip_prefix("wss://") {
        ("wss", r)
    } else if let Some(r) = raw.strip_prefix("ws://") {
        ("ws", r)
    } else {
        return Err(SignalError::BadServer(raw.to_string()));
    };
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(SignalError::BadServer(raw.to_string()));
    }
    Ok(format!("{scheme}://{host}/ws"))
}

/// 房间链接：`<server>/#room=ABC123`（fragment 不会进服务端日志）
pub fn room_link(server: &str, code: &str) -> Result<String, SignalError> {
    let code = normalize_room_code(code).ok_or(SignalError::BadRoomCode)?;
    let base = server.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(SignalError::EmptyServer);
    }
    Ok(format!("{base}/#room={code}"))
}

pub fn valid_room_code(code: &str) -> bool {
    code.len() == ROOM_LEN && code.bytes().all(|b| ROOM_ALPHABET.contains(&b))
}

/// 容忍小写、空格和连字符，输出服务端认的大写形式
pub fn normalize_room_code(input: &str) -> Option<String> {
    let cleaned: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if valid_room_code(&cleaned) {
        Some(cleaned)
    } else {
        None
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SignalError {
    #[error("服务器地址为空")]
    EmptyServer,
    #[error("服务器地址无法识别: {0}")]
    BadServer(String),
    #[error("房间号应为 8 位（字母 A-Z / 数字 2-9，不含 0 O 1 I）")]
    BadRoomCode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_serializes_exactly_like_the_web_page() {
        let json = serde_json::to_string(&ClientMsg::Join { code: "ABC123".into() }).unwrap();
        assert_eq!(json, r#"{"type":"join","code":"ABC123"}"#);
        let json = serde_json::to_string(&ClientMsg::Create).unwrap();
        assert_eq!(json, r#"{"type":"create"}"#);
        let json = serde_json::to_string(&ClientMsg::Bye).unwrap();
        assert_eq!(json, r#"{"type":"bye"}"#);
    }

    #[test]
    fn parses_every_server_message() {
        let cases = [
            (r#"{"type":"room","code":"ABC123"}"#, ServerMsg::Room { code: "ABC123".into() }),
            (r#"{"type":"joined","code":"ABC123"}"#, ServerMsg::Joined { code: "ABC123".into() }),
            (r#"{"type":"peer"}"#, ServerMsg::Peer),
            (r#"{"type":"peer-left"}"#, ServerMsg::PeerLeft),
            (r#"{"type":"bye"}"#, ServerMsg::Bye),
            (r#"{"type":"error","message":"房间已满"}"#, ServerMsg::Error { message: "房间已满".into() }),
        ];
        for (raw, want) in cases {
            let got: ServerMsg = serde_json::from_str(raw).unwrap();
            assert_eq!(got, want, "parsing {raw}");
        }
        // 未知类型不能炸
        let got: ServerMsg = serde_json::from_str(r#"{"type":"whatever"}"#).unwrap();
        assert_eq!(got, ServerMsg::Unknown);
    }

    #[test]
    fn ice_candidate_matches_browser_shape() {
        let raw = r#"{"type":"ice","candidate":{"candidate":"candidate:1 1 udp 1 127.0.0.1 5000 typ host","sdpMid":"0","sdpMLineIndex":0,"usernameFragment":"abc"}}"#;
        let msg: ServerMsg = serde_json::from_str(raw).unwrap();
        let ServerMsg::Ice { candidate } = msg else { panic!("not ice: {msg:?}") };
        assert_eq!(candidate.sdp_mid.as_deref(), Some("0"));
        assert_eq!(candidate.sdp_mline_index, Some(0));
        assert!(candidate.candidate.contains("typ host"));
        // 回写形状也必须能被浏览器吃下
        let out = serde_json::to_string(&ClientMsg::Ice { candidate }).unwrap();
        assert!(out.contains(r#""sdpMid":"0""#), "{out}");
        assert!(out.contains(r#""sdpMLineIndex":0"#), "{out}");
    }

    #[test]
    fn room_codes_are_validated_and_normalized() {
        assert!(valid_room_code("ABCD2345"));
        assert!(!valid_room_code("abcd2345"), "小写不是服务端格式");
        assert!(!valid_room_code("ABCD234"), "少一位不行");
        assert!(!valid_room_code("ABCD23456"), "多一位也不行");
        assert!(!valid_room_code("ABCD23O5"), "0/O/1/I 不在字母表里");
        assert_eq!(normalize_room_code(" abcd-2345 ").as_deref(), Some("ABCD2345"));
        assert_eq!(normalize_room_code("ab cd 2 3 4 5").as_deref(), Some("ABCD2345"));
        assert!(normalize_room_code("hello!").is_none());
    }

    #[test]
    fn ws_url_follows_the_page() {
        assert_eq!(ws_url("https://desk.example.com").unwrap(), "wss://desk.example.com/ws");
        assert_eq!(ws_url("http://192.168.1.8:8080").unwrap(), "ws://192.168.1.8:8080/ws");
        assert_eq!(ws_url("http://nas:6688/").unwrap(), "ws://nas:6688/ws");
        // 和页面一致：路径被丢掉，永远连根的 /ws
        assert_eq!(ws_url("https://desk.example.com/app").unwrap(), "wss://desk.example.com/ws");
        assert_eq!(ws_url("wss://desk.example.com/ws").unwrap(), "wss://desk.example.com/ws");
        assert!(ws_url("desk.example.com").is_err(), "缺少 scheme 应当报错，先走 normalize");
    }

    #[test]
    fn room_link_uses_fragment() {
        assert_eq!(
            room_link("https://desk.example.com/", "ABCD2345").unwrap(),
            "https://desk.example.com/#room=ABCD2345"
        );
        assert!(room_link("https://desk.example.com", "nope").is_err());
    }
}
