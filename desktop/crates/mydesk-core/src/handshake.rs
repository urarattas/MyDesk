//! 手动模式的握手码，与网页端 `packCode` / `unpackCode` 字节级兼容：
//!
//! * `RD1z` + base64url( raw-deflate( JSON ) )  —— 压缩
//! * `RD1p` + base64url( JSON )                —— 不压缩（浏览器没有 CompressionStream 时的退化）
//!
//! JSON 是 `{"v":1,"t":"offer"|"answer","s":"<sdp>"}`。base64url 不带 padding。

use std::io::{Read, Write};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "RD1";
/// 握手码本身（base64 之后）的长度上限，正常只有几十 KB
pub const MAX_INPUT: usize = 96 * 1024;
/// 解压输出上限：deflate 能到 1000:1，不设限就是内存炸弹
pub const MAX_OUTPUT: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SdpKind {
    Offer,
    Answer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handshake {
    pub v: u32,
    #[serde(rename = "t")]
    pub kind: SdpKind,
    #[serde(rename = "s")]
    pub sdp: String,
}

impl Handshake {
    pub fn new(kind: SdpKind, sdp: impl Into<String>) -> Self {
        Self { v: 1, kind, sdp: sdp.into() }
    }
}

/// 和网页端一样：能压就压，压不了（或压缩反而更大）就走明文。
pub fn pack(hs: &Handshake) -> Result<String, HandshakeError> {
    let json = serde_json::to_vec(hs).map_err(|e| HandshakeError::Json(e.to_string()))?;
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&json).map_err(|e| HandshakeError::Io(e.to_string()))?;
    let packed = enc.finish().map_err(|e| HandshakeError::Io(e.to_string()))?;
    if packed.len() < json.len() {
        Ok(format!("{PREFIX}z{}", URL_SAFE_NO_PAD.encode(packed)))
    } else {
        Ok(format!("{PREFIX}p{}", URL_SAFE_NO_PAD.encode(json)))
    }
}

/// 容忍聊天软件插入的空格和换行。
pub fn unpack(text: &str) -> Result<Handshake, HandshakeError> {
    let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.is_empty() {
        return Err(HandshakeError::Empty);
    }
    let rest = clean
        .strip_prefix(PREFIX)
        .ok_or(HandshakeError::BadPrefix)?;
    let mut chars = rest.chars();
    let flag = chars.next().ok_or(HandshakeError::BadPrefix)?;
    let body = chars.as_str();
    if body.is_empty() {
        return Err(HandshakeError::Empty);
    }
    if body.len() > MAX_INPUT {
        return Err(HandshakeError::TooLarge);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|e| HandshakeError::Base64(e.to_string()))?;

    let json = match flag {
        'z' => {
            let mut out = Vec::new();
            let mut dec = DeflateDecoder::new(&bytes[..]).take((MAX_OUTPUT + 1) as u64);
            dec.read_to_end(&mut out)
                .map_err(|e| HandshakeError::Inflate(e.to_string()))?;
            if out.len() > MAX_OUTPUT {
                return Err(HandshakeError::TooLarge);
            }
            out
        }
        'p' => bytes,
        other => return Err(HandshakeError::BadFlag(other)),
    };

    let hs: Handshake =
        serde_json::from_slice(&json).map_err(|e| HandshakeError::Json(e.to_string()))?;
    if hs.sdp.is_empty() {
        return Err(HandshakeError::Empty);
    }
    Ok(hs)
}

#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error("握手码为空")]
    Empty,
    #[error("不是 MyDesk 握手码（应以 RD1 开头）")]
    BadPrefix,
    #[error("握手码格式无法识别: {0}")]
    BadFlag(char),
    #[error("握手码过大，已拒绝")]
    TooLarge,
    #[error("base64 解码失败: {0}")]
    Base64(String),
    #[error("解压失败: {0}")]
    Inflate(String),
    #[error("握手码内容已损坏: {0}")]
    Json(String),
    #[error("压缩失败: {0}")]
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 网页端 MyDesk.html 用同一段 SDP 生成的码（deflate-raw + base64url）。
    const WEB_PACKED: &str = "RD1zq1YqU7Iy1FEqUbJSyk9LSy1S0lEqVrJSKrM1iCmKycu31VUwMTM0NDc2NDEwMDE2MDA1NDY2UzBS8PRT8AwwUTA0Mtcz0DPQMwSpLrbVBVEltgYKYN2JtulF-aUFVk6hfi4-rhAxpVoA";
    /// 同一个 SDP 的不压缩版本。
    const WEB_PLAIN: &str = "RD1peyJ2IjoxLCJ0Ijoib2ZmZXIiLCJzIjoidj0wXHJcbm89LSA0NjExNzMxNDAwNDMwMDUxMzM2IDIgSU4gSVA0IDEyNy4wLjAuMVxyXG5zPS1cclxudD0wIDBcclxuYT1ncm91cDpCVU5ETEUgMFxyXG4ifQ";
    const SDP: &str = "v=0\r\no=- 4611731400430051336 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0\r\n";

    #[test]
    fn decodes_a_code_produced_by_the_web_page() {
        let hs = unpack(WEB_PACKED).unwrap();
        assert_eq!(hs, Handshake::new(SdpKind::Offer, SDP));
        let hs = unpack(WEB_PLAIN).unwrap();
        assert_eq!(hs, Handshake::new(SdpKind::Offer, SDP));
        // 粘贴时常见的换行/空格
        let messy = format!("  {}\n {}  ", &WEB_PACKED[..20], &WEB_PACKED[20..]);
        assert_eq!(unpack(&messy).unwrap().sdp, SDP);
    }

    #[test]
    fn packs_something_it_can_read_back() {
        let hs = Handshake::new(SdpKind::Answer, SDP);
        let code = pack(&hs).unwrap();
        assert!(code.starts_with("RD1z"), "这个 SDP 应当能压小: {code}");
        assert_eq!(unpack(&code).unwrap(), hs);
        assert!(!code.contains('='), "base64url 不带 padding");
        assert!(!code.contains('+') && !code.contains('/'));
    }

    #[test]
    fn rejects_junk_and_bombs() {
        assert!(unpack("").is_err());
        assert!(unpack("hello").is_err());
        assert!(unpack("RD1xAAAA").is_err());
        assert!(unpack(&format!("RD1p{}", "A".repeat(MAX_INPUT + 8))).is_err());

        // 8 MiB 的零 > MAX_OUTPUT，必须被截断而不是把内存吃光
        let bomb = vec![0u8; 8 * 1024 * 1024];
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::best());
        enc.write_all(&bomb).unwrap();
        let packed = format!("RD1z{}", URL_SAFE_NO_PAD.encode(enc.finish().unwrap()));
        assert!(matches!(unpack(&packed), Err(HandshakeError::TooLarge)));
    }
}
