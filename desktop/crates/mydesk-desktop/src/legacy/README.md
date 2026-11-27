# legacy：Rust 侧直连 WebRTC 的实现（暂不参与编译）

这里原本是「用 webrtc-rs 在 Rust 里做 PeerConnection」的完整实现：

- `webrtc_peer.rs` —— PC 构建、data channel、H.264 轨道
- `app.rs` —— 信令 ↔ WebRTC ↔ 采集 ↔ 注入的会话编排
- `signal_client.rs` —— /ws 客户端

改用 Tauri 2 之后，WebRTC（含视频/音频采集）跑在 WebView2 里 —— 也就是直接复用了
`MyDesk.html` 那套已经在真机上验证过的 WebRTC + UI 逻辑，Rust 侧只保留
「原生键鼠注入」和「原生截屏兜底」这些浏览器做不到的部分。

保留这些文件是因为它们对「headless 服务端 / 无 WebView 场景」仍然有价值：
真要恢复，把三个文件移回 `src/`、在 `main.rs` 里加回 `mod` 声明即可，
但需要按 webrtc 0.21 的实际签名修 9 处编译错误（当时编译器报的 E0308/E0432/E0433/E0599）。
