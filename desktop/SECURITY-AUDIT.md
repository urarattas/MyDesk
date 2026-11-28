# 安全审计 — MyDesk 桌面端

日期：2026-11-28
范围：`desktop/`（`mydesk-core` + `mydesk-desktop`）、它内嵌的页面逻辑（`MyDesk.html` 的服务端模式部分）、以及 `server.js` 最近一次加固（`c885b5c`）。
环境：Arch Linux / Rust 1.100 / WebKitGTK 2.x；Windows 侧未在本轮复测（用户已自测）。

## 结论

共 10 项，其中 1 项高危（**已修复并有回归测试**），3 项中危（**已修复**），
1 项低危已加告警，其余 2 项低危与 4 项提示级问题仍建议处理。
整体设计是「默认拒绝」的：注入默认关闭、没有本地 HTTP 端口、坐标与按键统一 `sanitize()`、握手码有大小上限。
主要风险集中在**页面与桌面端共用的那段信令代码**——它承担了「授权」职责，却缺少角色约束。

## 阻断级：Linux WebView 没有 WebRTC（未解决）

真机自检（niri, WebKitGTK 2.54.3, `MYDESK_SELFTEST=1`）结果：

```
href=tauri://localhost        secureContext=true
RTCPeerConnection=undefined   ← 阻断
getDisplayMedia=true  canvasCaptureStream=true  clipboard=true
input_status={... "backend":"uinput"}      monitors=[{"name":"eDP-1",...}]
```

页面整套逻辑（信令、offer/answer、DataChannel、视频显示）都依赖 `RTCPeerConnection`，
WebKitGTK 默认构建不含 WebRTC，所以在 Linux 上**桌面端目前无法建立会话**——
CSP、注入、截屏、配置这些都正常，唯独最核心的 WebRTC 缺失。

三条可选路径（需要产品决策，本轮未动）：

1. **Linux 继续用网页版**（浏览器 + `bridge.py`，已验证可用），桌面端只在 Windows 发布；
2. **换 webview**：CEF / 自带 Chromium —— Tauri 不支持，工作量巨大；
3. **复活 `legacy/` 里的 webrtc-rs 实现**：Rust 原生 WebRTC，不依赖 webview，
   但要先修那 9 处编译错误，并自己实现 SDP/ICE/Datachannel 与屏幕编码。

## 高危（已修复）

### H-1 绕过「允许连接」直接建立会话

* 位置：`MyDesk.html` → `onSignal()` 的 `case "offer"`。
* 问题：服务端只是转发器，真正的「允许连接」门槛在页面里。但页面收到 `offer` 时不检查角色，
  任何人只要知道房间号，就能在 `join` 之后**自己推一条 offer**，主机端会立刻 `guestAnswerViaServer()`
  建好 RTCPeerConnection 并回 answer——`允许连接` 的提示形同虚设，且重复 offer 还能把已建立的会话顶掉。
* 复现：`node test/auth-bypass.mjs`（对修复前的页面 `pc=created`，修复后 `pc=null`）。
* 修复：只有 `SRV.role === "guest"` 才处理 `offer`；只有 `role === "host"` 且已有 `pc` 才处理 `answer`；
  `peer` 事件也加了角色/去重判断。
* 影响：仅绕过「有人请求连接」的确认；共享屏幕与「允许控制」仍需被控端手动开启。但足以让主机在无感知下
  被拉进一个会话，属高危。

## 中危（已修复）

### M-1 `autotest_write` 是无条件的任意文件写入

* 位置：`mydesk-desktop/src/commands.rs`。
* 问题：`autotest_write(path, content)` 直接把参数写盘，任何环境变量都不需要。WebView 里只要有一处 XSS
  （页面日志会渲染对端可控的字符串），就能以当前用户身份写任意文件。
* 修复：未设置 `MYDESK_AUTOTEST` / `MYDESK_SELFTEST` 时直接返回错误。

### M-2 `inject_probe` 自授权，绕过 `allow_remote`

* 位置：`mydesk-desktop/src/commands.rs`。
* 问题：探针会临时把 `allow_remote = true` 再移动光标，等于给页面一个「随时绕过授权开关」的入口。
* 修复：同样只在自检/自动化环境可用。

## 中危（已修复）

### M-3 没有 CSP

* 位置：`tauri.conf.json`（原来是 `"csp": null`）。
* 风险：页面是单文件内联脚本，CSP 必然要放 `'unsafe-inline'`，但 `connect-src` / `object-src` /
  `base-uri` / `frame-ancestors` 仍然能挡住一部分注入与数据外带；原来完全没有。
* 修复：加入最小策略（保留 `'unsafe-inline'`，放开 `ws:`/`wss:` 与 Tauri IPC 的
  `ipc: http://ipc.localhost`，禁掉 `object-src`/`base-uri`/`form-action`/`frame-ancestors`）。
* 回归：`node test/csp.mjs` —— 用 `tauri.conf.json` 里的**同一份**策略经代理加载页面，
  验证启动、`connect-src` 放行 WebSocket、零违规。
* 仍待办：`withGlobalTauri: true` 还在，页面能直接拿到 `window.__TAURI__`；后续可改成
  按需 `import` 并关掉它。

## 低危 / 提示

| # | 位置 | 问题 | 建议 |
| --- | --- | --- | --- |
| L-1 | `capture_win.rs` | `CreateCompatibleDC/Bitmap` 失败分支没有 `DeleteDC/DeleteObject`，错误路径漏 GDI 句柄 | 失败分支补释放；只在异常路径，影响有限 |
| L-2 | 页面 / `tauri-shim.js` | ~~明文 `ws://` 无提示~~ **已加告警**：非 loopback 的 `ws://` 会在日志与 toast 里提示房间号/IP/SDP 可被同网段中间人看到（不阻断连接） | — |
| L-3 | `main.rs` | 无显示器时 tao 直接 panic（带 backtrace），不像正常错误 | 捕获 GTK 初始化失败并打印可读提示 |
| I-1 | git 历史 | `c885b5c` 曾提交 `%SystemDrive%/ProgramData/Microsoft/Windows/Caches/*.db`，`2fbfa70` 删除但仍在历史里 | 用 `git filter-repo` 清理；这些是 DNS/浏览器缓存，可能含用户痕迹 |
| I-2 | `commands.rs` | 非 Windows 截屏路径忽略 `max_width`（`dw/dh` 未使用），Linux 上按原分辨率 JPEG | 用 `image` 缩放或在 encode 前 resample |
| I-3 | Linux 注入 | ~~Wayland 只能进 XWayland~~ **已处理**：新增 uinput 后端（`inject_uinput.rs`），Wayland 会话自动启用，niri 实测可创建设备；需 `/dev/uinput` 权限 | — |
| I-4 | 服务端 | ~~房间号 6 位 ≈ 30 bit~~ **已处理**：房间号扩到 8 位（≈ 39.4 bit，`32^8 ≈ 1.1e12`），旧客户端不再兼容 | — |

## 做得对的地方（避免以后改坏）

* `InputEvent::sanitize()`：坐标夹到 0–1、`mods` 只留 4 位、滚轮封顶 ±60、未知鼠标键丢弃。
* `allow_remote` 默认 `false`，关闭时 `release_all()`，断线也会释放，不会留卡住的 Ctrl。
* 桌面端没有任何本地 HTTP 端口——网页版那个能被任意网页打到的 `127.0.0.1:8765` 不存在了。
* 握手码：`MAX_INPUT` / `MAX_OUTPUT` 双上限 + `take(MAX_OUTPUT+1)`，deflate 炸弹有测试覆盖。
* 服务端：连接数上限、房间数上限、join 令牌桶、失败冷却、`Sec-Fetch-Site` 跨站拦截、可配 `TRUSTED_ORIGINS`。

## Linux 复测结果

| 项目 | 结果 |
| --- | --- |
| `cargo test -p mydesk-core` | 21/21 通过 |
| `cargo build --release` | 通过（3m50s，见下方修复） |
| `cargo tauri build --bundles deb,appimage` | 通过：`MyDesk_0.1.0_amd64.deb`（4.5 MB）、`.AppImage`（105 MB） |
| 动态依赖 | `ldd` 无缺失 |
| 无显示器启动 | GTK 初始化 panic（见 L-3），退出码非 0 |

**为了让 Linux 编出来，修了一处清单错误**：`base64`、`jpeg-encoder`、`tracing`、`tracing-subscriber`
原本写在 `[target.'cfg(windows)'.dependencies]` 下，但 `commands.rs` / `inject.rs` / `main.rs` 无条件下引用它们，
Linux 直接 16 个编译错误。已移到通用依赖区。

Linux 与 Windows 的差异仍然存在（不是 bug，但要知道）：

* 截屏走 `xcap`（Wayland 走 portal / wlroots，X11 走 xcb），热路径没有 GDI 那样的零拷贝；
* 注入走 enigo 的 X11 后端，Wayland 原生窗口收不到；
* 剪贴板未实现（页面会收到明确的 `clip-fail`）。

---

# 第二轮：全仓复审（2026-11-28）

范围扩大到服务端、桥接、页面与 CI。桌面端 Rust 部分沿用第一轮的结论。

## 已修复

| # | 级别 | 问题 | 处理 |
| --- | --- | --- | --- |
| R-1 | 中 | **链接自动加入房间**：`#room=CODE` 在页面加载 400ms 后自己 `join`。发链接的人当主机时可以直接推 SDP，把访问者单方面拉进会话（IP/SDP 泄露，随后可能被诱导共享屏幕） | 改为只预填房间号 + 提示，必须点「加入」；`test/link-join.mjs` 5/5 覆盖 |
| R-2 | 低（功能） | `slice(0, 6)` 没跟着 8 位房号一起改，从链接进入时房号被截断，必然加入失败 | 改为按 8 位截取并过滤非法字符 |
| R-3 | 中（供应链） | `release.yml` 里**所有 job** 都持有 `contents: write` + `packages: write`；其中 bridge job 会 `pip install` 第三方包，一旦上游被投毒就能写仓库 | 降为默认 `contents: read`，只有 `docker`（packages）与 `publish`（contents）单独授权 |
| R-4 | 中（供应链） | `pyinstaller` 在 CI 里未固定版本，构建产物随上游漂移 | 固定 `pyinstaller==6.22.3` 到 `bridge/requirements.txt` |

## 未修（列为建议或接受）

| # | 级别 | 问题 | 说明 / 建议 |
| --- | --- | --- | --- |
| R-5 | 低 | 桥接 CORS 回显任意 `Origin`，并声明 `Access-Control-Allow-Private-Network: true` | 设计使然：页面可能部署在任意域名。真正的闸门是配对令牌；令牌 128 bit，无法暴力 |
| R-6 | 低 | `/input`、`/clipboard` 的 401 没有限速 | **已修复**：失败时延迟 150 ms（实测坏令牌 151 ms / 好令牌 0.7 ms），把猜测速率压到个位数/秒 |
| R-7 | 低 | 配对令牌存进 `localStorage` | **已修复**：默认 `sessionStorage`，勾「记住配对令牌」才写 `localStorage`，取消勾选会清掉已存的值 |
| R-8 | 低 | `join` 的报错区分「房间不存在」与「房间已满」 | 房间枚举 oracle；8 位房号 + 令牌桶后价值很低，可统一文案 |
| R-9 | 低 | WebSocket 解析未校验 RSV1–3 位 | **已修复**：置位直接 close(1002)，`test/protocol.mjs` 覆盖 |
| R-10 | 中（供应链） | 第三方 Action 用 tag（`@v4`、`@v2`）而非 commit SHA 固定 | **已修复**：12 个 Action 全部固定到 commit SHA；`dtolnay/rust-toolchain` 固定后补了 `toolchain: stable`（它靠 ref 名推断工具链） |
| R-11 | 信息 | `/healthz` 暴露房间数与连接数 | **已修复**：默认只回 `{"ok":true}`，`HEALTHZ_DETAIL=1` 才带计数 |
| R-12 | 信息 | exe / deb / AppImage 均未签名 | README 已要求对照 `SHA256SUMS.txt` 校验 |

## 复核为良好的部分

* **桥接**：校验 `Host` 挡 DNS rebinding；令牌用 `hmac.compare_digest`；`/clipboard` 读也需令牌；请求体上限 1 MB；只监听 `127.0.0.1`；无令牌时 `/healthz` 只回最小信息。
* **服务端**：强制客户端掩码；单帧/单消息上限；续帧累计封顶；控制帧规则；`Sec-Fetch-Site` 拦跨站 WebSocket 劫持；join 令牌桶 + 单连接失败冷却；`MAX_ROOMS` / `MAX_CONNS`；畸形 request-target 退化为 400 而不是崩进程。
* **页面**：没有 `innerHTML` / `eval` / `new Function`，日志与提示全部走 `textContent`；`offer`/`answer` 有角色校验；允许连接需要主机确认。
* **桌面端**：uinput 设备结构体有 1116 字节的尺寸断言；`autotest_write` / `inject_probe` 仅在自动化模式下可用；注入默认关闭且关闭时 `release_all()`；WebView 有最小 CSP。
