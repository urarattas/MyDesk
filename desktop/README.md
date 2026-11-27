# MyDesk 桌面客户端（Rust + Tauri 2）

网页版的被控端需要「浏览器 + `bridge.py`」两件东西。这里把它们合成一个原生应用：
**前端仍然是那套网页 UI（Tauri 的 WebView2 里跑），Rust 侧只补上浏览器做不到的两件事
——原生键鼠注入、原生截屏兜底。**

```text
网页版：  对端键鼠 -> DataChannel -> 浏览器 -> HTTP POST 127.0.0.1:8765 -> bridge.py -> 系统
桌面版：  对端键鼠 -> DataChannel -> WebView2 -> Tauri 命令              -> enigo    -> 系统
```

## 为什么是这个架构

WebView2 就是一个完整的 Chromium：`RTCPeerConnection`、`getDisplayMedia`、
`canvas.captureStream()`、DataChannel 全都在（自检实测，见下）。所以

* **信令 / WebRTC / SDP / ICE / 数据通道 / 界面**：直接复用 `MyDesk.html`，协议天然与网页版一致，
  任意一端是浏览器都能互通；
* **Rust 侧**只做 WebView 做不到的：系统级键鼠注入（enigo）、原生截屏（GDI）、配置文件。

因此也不需要把 3900 行 UI 维护两份：`build.rs` 会把仓库根的 `MyDesk.html` 复制成
`ui/index.html`，只额外注入一个适配层 `ui/tauri-shim.js`（单一事实来源，不会漂移）。

## 目录

```text
desktop/
├── config.example.toml
└── crates/
    ├── mydesk-core/            # 纯逻辑：配置、信令协议、握手码、输入模型（21 个单测）
    └── mydesk-desktop/         # Tauri 2 应用
        ├── build.rs            # MyDesk.html -> ui/index.html（注入 shim）
        ├── tauri.conf.json     # 窗口在 Rust 里建（要指定 WebView2 数据目录）
        ├── capabilities/       # core:default
        ├── ui/tauri-shim.js    # 全部原生适配都在这里
        └── src/
            ├── main.rs         # 入口、窗口创建、命令注册
            ├── commands.rs     # Tauri 命令：注入/截屏/配置/自检
            ├── inject.rs       # enigo 注入（修饰键状态机、release_all）
            ├── capture_win.rs  # GDI BitBlt / StretchBlt 抓屏
            ├── config.rs       # config.toml 读写 + patch
            └── legacy/         # 早期「Rust 直连 WebRTC」实现，未参与编译
```

## 构建 / 运行

需要 **MSVC 工具链 + WebView2 运行时**（Win10/11 自带），Rust 1.78+。

```powershell
cd desktop
cargo build --release        # target/release/mydesk.exe
cargo test -p mydesk-core    # 纯逻辑单测
.	argeteleasemydesk.exe
```

> 如果在没有开发者命令提示符的环境里构建失败 `link.exe not found`，先 `call vcvars64.bat`；
> 若还报错，把 `TMP`/`TEMP` 指向可写目录（受限环境下 cl.exe 写不了系统临时目录）。

## 用起来

1. 打开应用 → 「服务器」面板填信令服务器地址 → 保存（写进 `config.toml`，下次自动带出）。
   默认是**服务器模式**；只想要零基础设施就切「手动」，双方交换一次 `RD1…` 握手码。
2. **当被控端**：点「创建房间」→ 把链接/房间号发给对方 → 有人在请求连接时点「允许连接」→
   点「共享屏幕」。
3. **当控制端**：填对方的房间号 → 加入 → 允许后点画面接管键鼠。
4. 要允许对方操作本机：勾「允许对方控制我」（等价于 `[input].allow_remote = true`）。

## 实测数据（release，2560×1440 屏幕）

| 项目 | 结果 |
| --- | --- |
| WebView2 能力 | `RTCPeerConnection`、`getDisplayMedia`、`getUserMedia`、`canvas.captureStream` 全部可用；`secureContext=true` |
| 原生截屏（GDI） | 42–50 ms/帧（2560×1440 → StretchBlt 到 1600 宽） |
| JPEG 编码 | 16–19 ms/帧（`jpeg-encoder` q70） |
| 端到端视频 | **~16 fps**，对端收到 2560×1440 视频轨且有新帧 |
| 原生注入 | 探针把系统光标精确移到屏幕中心 (1280,720) |
| 端到端注入 | 控制端经 DataChannel 发 6 条移动 → 被控端 `applied=7` → 光标落在 **(2175,1223)**（即 x=0.85,y=0.85 的换算值） |
| 双端协商 | 两个实例经本地信令服务器：`ctl`/`in` 双通道 `open`、`phase=live`、`已连接 · P2P` |

## 已知问题 / 下一步

* **控制端最终状态偶尔回到 idle**：自动化跑双实例时，客机在会话末期偶尔掉回 `idle`
  （主机侧仍是 live）。像是页面信令在「重新协商/重复 offer」时的竞态，需要在
  `onSignal` 里对 `live` 状态下的重复 `offer`/`peer` 做忽略处理。媒体与注入本身已验证可用。
* **原生截屏没有音频**：走 GDI 时只有视频。要系统音频得用 WebView2 的
  `getDisplayMedia`（默认优先），或后续接 WASAPI 环回。
* **画质/帧率**：目前 JPEG + canvas.captureStream（~16 fps）。要 30/60 fps 应改成
  H.264 硬件编码（Media Foundation）直接推 RTP，而不是走 canvas。
* **多显示器**：GDI 目前只抓主屏（`monitor` 配置项已留，取原点待补）。
* **剪贴板**：未实现（页面收到请求会得到 `clip-fail` 的明确回复）。
* **UIPI**：Windows 下无法向管理员权限窗口注入（系统限制）。
* `legacy/` 里保留了早期「Rust 直连 webrtc-rs」的实现（9 处编译错误未修），
  仅在需要无 WebView 的 headless 场景时才值得恢复。

## 安全边界

* 注入默认关闭，必须显式勾选；断开会 `release_all()`，不会留下卡住的修饰键。
* 事件进入注入层前统一 `sanitize()`：坐标夹到 0–1、mods 只留 4 位、滚轮步长封顶、未知鼠标键丢弃。
* 桥接端口不再存在：没有任何本地 HTTP 端点，任意网页都碰不到这套注入。
* 信令服务器若开了 `STRICT_ORIGIN=1`，需要 `TRUSTED_ORIGINS=http://tauri.localhost`
  （桌面端的页面来源是 `tauri.localhost`）。
