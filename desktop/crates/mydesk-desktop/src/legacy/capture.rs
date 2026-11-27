//! 被控端：截屏 + H.264 编码，产出可以直接塞进 RTP 的 Annex-B 帧。
//!
//! 这里刻意**不依赖 image crate**：xcap 给的 `RgbaImage` 只用来 `as_raw()` 取像素，
//! RGBA→I420 自己转（20 行），再用已验证存在的 `YUVBuffer::from_vec`。
//! 这样就不会被 image / openh264 的版本耦合卡住。
//!
//! 编码器的两个硬约束（来自 openh264 文档）：
//! * `EncodedBitStream` 是 `!Send` 且借用了 `&mut Encoder`，**绝不能跨 .await 持有**；
//! * 宽高必须是偶数，否则 `YUVBuffer::new` 直接 panic。
//!
//! 输出是 Annex-B（`00 00 00 01` 起始码），打包 RTP 前要去掉起始码。

use std::time::{Duration, Instant};

use openh264::encoder::Encoder;
use openh264::formats::YUVBuffer;

use crate::Error;

/// 一帧编码结果
pub struct EncodedFrame {
    /// Annex-B 码流（每个 NAL 前有 4 字节起始码），关键帧自带 SPS/PPS
    pub annexb: Vec<u8>,
    pub keyframe: bool,
    pub duration: Duration,
}

pub struct ScreenCapturer {
    monitor_index: usize,
    fps: u32,
    pub width: u32,
    pub height: u32,
    next_deadline: Instant,
}

impl ScreenCapturer {
    pub fn open(monitor_index: usize, fps: u32) -> Result<Self, Error> {
        let fps = fps.clamp(1, 240);
        let (width, height) = {
            let monitors = xcap::Monitor::all().map_err(|e| Error::Capture(format!("枚举显示器失败: {e}")))?;
            let monitor = monitors
                .get(monitor_index)
                .or_else(|| monitors.first())
                .ok_or_else(|| Error::Capture("没有可用的显示器".into()))?;
            let w = monitor.width().map_err(|e| Error::Capture(e.to_string()))?;
            let h = monitor.height().map_err(|e| Error::Capture(e.to_string()))?;
            (w, h)
        };
        // 编码器要求偶数宽高
        let width = width & !1;
        let height = height & !1;
        if width < 16 || height < 16 {
            return Err(Error::Capture(format!("显示器尺寸异常: {width}x{height}")));
        }
        Ok(Self { monitor_index, fps, width, height, next_deadline: Instant::now() })
    }

    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// 取一帧原始 RGBA 像素。帧率控制在这里做：没到下一帧的时间点就睡一下。
    pub fn capture_rgba(&mut self) -> Result<Vec<u8>, Error> {
        let now = Instant::now();
        if self.next_deadline > now {
            std::thread::sleep(self.next_deadline - now);
        }
        let step = Duration::from_micros(1_000_000 / u64::from(self.fps.max(1)));
        self.next_deadline = Instant::now() + step;

        let monitors = xcap::Monitor::all().map_err(|e| Error::Capture(e.to_string()))?;
        let monitor = monitors
            .get(self.monitor_index)
            .or_else(|| monitors.first())
            .ok_or_else(|| Error::Capture("显示器已消失".into()))?;
        let image = monitor.capture_image().map_err(|e| Error::Capture(e.to_string()))?;
        Ok(image.as_raw().clone())
    }
}

pub struct VideoEncoder {
    encoder: Encoder,
    width: u32,
    height: u32,
    fps: u32,
}

impl VideoEncoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, Error> {
        // 注意：`Encoder::new()` 用默认参数（含默认码率控制）。
        // 想设码率/屏幕内容优化，要换 `Encoder::with_api_config(OpenH264API::from_source(), cfg)`，
        // 但 `EncoderConfig` 的构造器名在 0.9.10 文档里没有逐字确认，所以先不猜。
        let encoder = Encoder::new().map_err(|e| Error::Encode(e.to_string()))?;
        Ok(Self { encoder, width, height, fps })
    }

    /// RGBA → I420 → 编码。返回 None 表示这一帧没有任何输出（编码器还没攒够）。
    pub fn encode_rgba(&mut self, rgba: &[u8], stamp: Instant) -> Result<Option<EncodedFrame>, Error> {
        let expect = (self.width as usize) * (self.height as usize) * 4;
        if rgba.len() != expect {
            return Err(Error::Encode(format!("像素长度不符: {} != {expect}", rgba.len())));
        }
        let i420 = rgba_to_i420(rgba, self.width as usize, self.height as usize);
        let yuv = YUVBuffer::from_vec(i420, self.width as usize, self.height as usize);
        // EncodedBitStream 借了 &mut encoder 且 !Send：这段必须是一个同步块，
        // 出了作用域才允许 .await
        let (bytes, keyframe) = {
            let stream = self.encoder.encode(&yuv).map_err(|e| Error::Encode(e.to_string()))?;
            // 用 Debug 字符串判关键帧，避免写死可能改名/改大小写的枚举变体
            let key = matches!(
                format!("{:?}", stream.frame_type()).to_ascii_lowercase().as_str(),
                "i" | "idr" | "iframe"
            );
            (stream.to_vec(), key)
        };
        if bytes.is_empty() {
            return Ok(None);
        }
        let _ = stamp;
        Ok(Some(EncodedFrame {
            annexb: bytes,
            keyframe,
            duration: Duration::from_micros(1_000_000 / u64::from(self.fps.max(1))),
        }))
    }
}

/// RGBA8 → I420（BT.601 有限范围）。纯算术，方便单测。
pub fn rgba_to_i420(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    debug_assert_eq!(rgba.len(), width * height * 4);
    let y_size = width * height;
    let c_size = (width / 2) * (height / 2);
    let mut out = vec![0u8; y_size + c_size * 2];
    let (y_plane, rest) = out.split_at_mut(y_size);
    let (u_plane, v_plane) = rest.split_at_mut(c_size);

    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) * 4;
            let r = i32::from(rgba[i]);
            let g = i32::from(rgba[i + 1]);
            let b = i32::from(rgba[i + 2]);
            // 有限范围：Y 16..235，Cb/Cr 16..240
            let yy = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            y_plane[y * width + x] = yy.clamp(0, 255) as u8;
        }
    }
    for cy in 0..height / 2 {
        for cx in 0..width / 2 {
            let mut r = 0i32;
            let mut g = 0i32;
            let mut b = 0i32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = ((cy * 2 + dy) * width + (cx * 2 + dx)) * 4;
                    r += i32::from(rgba[i]);
                    g += i32::from(rgba[i + 1]);
                    b += i32::from(rgba[i + 2]);
                }
            }
            r /= 4;
            g /= 4;
            b /= 4;
            let u = ((112 * b - 38 * r - 74 * g + 128) >> 8) + 128;
            let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
            let idx = cy * (width / 2) + cx;
            u_plane[idx] = u.clamp(0, 255) as u8;
            v_plane[idx] = v.clamp(0, 255) as u8;
        }
    }
    out
}

/// 把 Annex-B 拆成 NAL 单元（去掉起始码）。
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = None;
    let mut i = 0;
    while i + 3 <= data.len() {
        let is4 = data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && i + 4 <= data.len() && data[i + 3] == 1;
        let is3 = data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1;
        if is4 || is3 {
            if let Some(s) = start {
                out.push(&data[s..i]);
            }
            i += if is4 { 4 } else { 3 };
            start = Some(i);
            continue;
        }
        i += 1;
    }
    if let Some(s) = start {
        if s < data.len() {
            out.push(&data[s..]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i420_has_the_right_size_and_planes() {
        let w = 4;
        let h = 2;
        let rgba = vec![255u8; w * h * 4];
        let out = rgba_to_i420(&rgba, w, h);
        assert_eq!(out.len(), w * h + (w / 2) * (h / 2) * 2);
        // 纯白：Y 应当接近 235，U/V 接近 128
        assert!(out[0] >= 230, "Y={}", out[0]);
        let chroma = &out[w * h..];
        assert!(chroma.iter().all(|v| (120..=136).contains(v)), "{chroma:?}");
    }

    #[test]
    fn black_is_16() {
        let rgba = vec![0u8; 2 * 2 * 4];
        let out = rgba_to_i420(&rgba, 2, 2);
        assert_eq!(out[0], 16);
    }

    #[test]
    fn annexb_splitter_handles_3_and_4_byte_start_codes() {
        let data = [
            0u8, 0, 0, 1, 0x67, 0x42, // SPS
            0, 0, 1, 0x68, 0xce, // PPS（3 字节起始码）
            0, 0, 0, 1, 0x65, 0x88, // IDR
        ];
        let nals = split_annexb(&data);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0], &[0x67, 0x42]);
        assert_eq!(nals[1], &[0x68, 0xce]);
        assert_eq!(nals[2], &[0x65, 0x88]);
    }

    #[test]
    fn annexb_splitter_tolerates_report() {
        assert!(split_annexb(&[]).is_empty());
        assert!(split_annexb(&[1, 2, 3]).is_empty());
    }
}
