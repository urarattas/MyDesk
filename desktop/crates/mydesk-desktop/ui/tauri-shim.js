/*
 * MyDesk 原生适配层（Tauri 2）
 *
 * 页面本体（MyDesk.html）一行都不改：所有原生能力都在下面这几个「拦截点」注入，
 * 因为它在浏览器里打开时这套逻辑会整体跳过（没有 __TAURI__）。
 *
 *   1. fetch  -> 拦截打到 127.0.0.1:8765 的桥接请求，转成 Tauri 命令（原生注入）
 *   2. WebSocket -> 把 app 自身的 origin 换成配置里的信令服务器
 *   3. input#roomLink -> 房间链接前缀换成服务器地址
 *   4. navigator.mediaDevices.getDisplayMedia -> WebView2 不支持时的原生截屏兜底
 *   5. 服务器地址输入框 / 授权开关 -> 写回 config.toml
 */
(() => {
  const T = window.__TAURI__;
  if (!T || !T.core) return; // 在普通浏览器里打开：什么也不做
  const invoke = T.core.invoke;

  window.__MYDESK_SHIM__ = true;
  async function dumpError(kind, detail) {
    try {
      const out = window.__MYDESK_AUTOTEST_OUT__;
      if (!out) return;
      await invoke("autotest_write", {
        path: out + "\\error-" + kind + ".json",
        content: JSON.stringify(Object.assign({ kind, at: new Date().toISOString() }, detail), null, 2),
      });
    } catch (e) {
      /* ignore */
    }
  }
  window.addEventListener("error", (e) => dumpError("js", { message: e.message, stack: e.error && e.error.stack, src: e.filename, line: e.lineno }));
  window.addEventListener("unhandledrejection", (e) => dumpError("promise", { reason: String((e.reason && e.reason.stack) || e.reason) }));

  let cfg = {
    mode: "server",
    server: "",
    recent: [],
    allow_remote: false,
    max_fps: 30,
    monitor: 0,
  };
  const serverBase = () => String(cfg.server || "").trim().replace(/\/+$/, "");

  /* ---------------- 1. 本地桥接 -> Tauri 命令 ---------------- */
  const LOCAL = /^https?:\/\/(127\.0\.0\.1|localhost|\[::1\]):8765(\/|$)/;
  const jsonResp = (obj, status = 200) =>
    new Response(JSON.stringify(obj), { status, headers: { "content-type": "application/json" } });
  const emptyResp = (status = 204) => new Response(null, { status });
  const realFetch = window.fetch.bind(window);

  window.fetch = async (input, init) => {
    const url = typeof input === "string" ? input : (input && input.url) || String(input);
    if (!LOCAL.test(url)) return realFetch(input, init);
    const path = (url.replace(LOCAL, "/").split("?")[0] || "/");
    try {
      if (path === "/health") {
        const st = await invoke("input_status");
        return jsonResp({
          ok: true,
          screen: st.screen,
          backend: st.allow_remote ? "enigo·已授权" : "enigo·待授权",
          clipboard: "none",
          error: null,
        });
      }
      if (path === "/input") {
        const body = JSON.parse((init && init.body) || "{}");
        await invoke("inject_events", { events: body.events || [] });
        return emptyResp(204);
      }
      if (path === "/clipboard") {
        // 原生版暂不做剪贴板：明确失败，页面会走它的兜底提示
        return jsonResp({ ok: false, text: "", error: "原生版暂未实现剪贴板" }, 200);
      }
      return emptyResp(204);
    } catch (e) {
      return jsonResp({ ok: false, error: String(e) }, 500);
    }
  };

  /* ---------------- 2. 信令地址指向配置里的服务器 ---------------- */
  const NativeWS = window.WebSocket;
  class WrappedWS extends NativeWS {
    constructor(url, protocols) {
      super(mapWsUrl(url), protocols);
    }
  }
  let plaintextWarned = false;
  function mapWsUrl(raw) {
    const base = serverBase();
    if (!base) return raw;
    try {
      const u = new URL(raw);
      const s = new URL(base);
      u.protocol = s.protocol === "https:" ? "wss:" : "ws:";
      u.host = s.host;
      u.pathname = "/ws";
      // 明文信令会被同网段的中间人看到房间号、IP 与 SDP；本机地址除外
      if (u.protocol === "ws:" && !plaintextWarned) {
        const host = u.hostname.toLowerCase();
        if (host !== "localhost" && host !== "127.0.0.1" && host !== "::1") {
          plaintextWarned = true;
          console.warn("[mydesk] 信令走明文 ws://" + u.host + "，建议改用 https 反代");
        }
      }
      return u.toString();
    } catch (e) {
      return raw;
    }
  }
  window.WebSocket = WrappedWS;

  /* ---------------- 3. 房间链接：换成服务器地址 ---------------- */
  const linkEl = document.getElementById("roomLink");
  if (linkEl) {
    const desc = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
    Object.defineProperty(linkEl, "value", {
      configurable: true,
      get() {
        return desc.get.call(this);
      },
      set(v) {
        let out = String(v);
        const base = serverBase();
        const i = out.indexOf("#room=");
        if (base && i >= 0) out = base + "/" + out.slice(i);
        desc.set.call(this, out);
      },
    });
  }

  /* ---------------- 3.5 自动播放 ---------------- */
  // WebView2 的自动播放策略比普通浏览器严：srcObject 挂上去后 video 可能停在暂停态，
  // 表现就是"有画面尺寸但卡在首帧"。这里在赋 srcObject 的同时主动 play()。
  (() => {
    const desc = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, "srcObject");
    if (!desc || !desc.set) return;
    Object.defineProperty(HTMLMediaElement.prototype, "srcObject", {
      configurable: true,
      get() {
        return desc.get.call(this);
      },
      set(value) {
        desc.set.call(this, value);
        if (!value) return;
        try {
          this.autoplay = true;
          if (this.tagName === "VIDEO") this.muted = true;
        } catch (e) {
          /* ignore */
        }
        const p = this.play();
        if (p && typeof p.catch === "function") {
          p.catch(() => {
            // 真的被拦就只能等一次用户手势
            const resume = () => {
              this.play().catch(() => {});
              window.removeEventListener("pointerdown", resume);
              window.removeEventListener("keydown", resume);
            };
            window.addEventListener("pointerdown", resume, { once: true });
            window.addEventListener("keydown", resume, { once: true });
          });
        }
      },
    });
  })();

  /* ---------------- 4. 原生截屏兜底 ---------------- */
  let nativeActive = false;
  async function nativeStream() {
    const info = await invoke("capture_start", {
      monitor: Number(cfg.monitor) || 0,
      fps: Math.max(1, Math.min(Number(cfg.max_fps) || 30, 60)),
    });
    const canvas = document.createElement("canvas");
    canvas.width = info.width;
    canvas.height = info.height;
    const ctx = canvas.getContext("2d", { alpha: false });
    const stream = canvas.captureStream(info.fps);
    nativeActive = true;
    const img = new Image();
    let seq = 0;
    const interval = 1000 / info.fps;
    const pump = async () => {
      if (!nativeActive) return;
      const t0 = performance.now();
      try {
        // 采集在 Rust 线程里常驻，这里只取"最新一帧"，没有新帧就跳过
        const f = await invoke("capture_frame", { lastSeq: seq });
        if (f && f.jpeg) {
          seq = f.seq;
          img.onload = () => {
            ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
            window.__MYDESK_CAPTURE_DRAWN__ = (window.__MYDESK_CAPTURE_DRAWN__ || 0) + 1;
          };
          img.src = "data:image/jpeg;base64," + f.jpeg;
        }
      } catch (e) {
        /* 单帧失败不致命 */
      }
      const spent = performance.now() - t0;
      setTimeout(pump, Math.max(0, interval - spent));
    };
    pump();
    const track = stream.getVideoTracks()[0];
    if (track) {
      track.addEventListener("ended", () => {
        nativeActive = false;
        invoke("capture_stop").catch(() => {});
      });
    }
    // 原生截屏没有系统音频：如实告知
    window.__MYDESK_NATIVE_CAPTURE__ = true;
    return stream;
  }

  const md = navigator.mediaDevices;
  if (md) {
    const realGDM = md.getDisplayMedia ? md.getDisplayMedia.bind(md) : null;
    md.getDisplayMedia = async (constraints) => {
      if (realGDM && !window.__MYDESK_FORCE_NATIVE__) {
        try {
          return await realGDM(constraints);
        } catch (e) {
          console.warn("[mydesk] getDisplayMedia 不可用，改用原生截屏：", e && e.message);
        }
      }
      return nativeStream();
    };
  }

  /* ---------------- 5. 配置联动 ---------------- */
  function hideWebOnlyBits() {
    // 原生版不需要 bridge.py / exe / 配对令牌
    ["bridgeToken", "bridgeScript", "btnCopyScript", "btnDownloadScript", "btnDownloadExe"].forEach((id) => {
      const el = document.getElementById(id);
      if (el) el.style.display = "none";
    });
  }

  function buildServerRow() {
    const lobby = document.getElementById("srvLobby");
    if (!lobby || document.getElementById("srvUrl")) return;
    const row = document.createElement("div");
    row.className = "row";
    row.style.marginTop = "10px";
    const input = document.createElement("input");
    input.type = "text";
    input.id = "srvUrl";
    input.spellcheck = false;
    input.placeholder = "信令服务器地址，如 https://desk.example.com";
    const btn = document.createElement("button");
    btn.className = "btn";
    btn.id = "btnSrvSave";
    btn.textContent = "保存";
    const hint = document.createElement("p");
    hint.className = "hint";
    hint.id = "srvHint";
    hint.style.marginTop = "6px";
    hint.textContent = "原生版把服务器地址记在 config.toml 里，下次启动自动带出。";
    row.appendChild(input);
    row.appendChild(btn);
    lobby.insertBefore(hint, lobby.firstChild);
    lobby.insertBefore(row, lobby.firstChild);

    btn.addEventListener("click", async () => {
      try {
        const view = await invoke("config_set", { patch: { server: input.value.trim() } });
        applyConfig(view);
        hint.textContent = view.server ? "已保存：" + view.server : "还没有填服务器地址";
      } catch (e) {
        hint.textContent = "保存失败：" + e;
      }
    });
    // 历史地址做成 datalist
    const list = document.createElement("datalist");
    list.id = "srvHistory";
    row.appendChild(list);
    input.setAttribute("list", "srvHistory");
  }

  function applyConfig(view) {
    cfg = Object.assign(cfg, view || {});
    const input = document.getElementById("srvUrl");
    if (input) {
      const best = cfg.server || (cfg.recent && cfg.recent[0]) || "";
      if (!input.value) input.value = best;
      const list = document.getElementById("srvHistory");
      if (list) {
        list.innerHTML = "";
        (cfg.recent || []).forEach((u) => {
          const opt = document.createElement("option");
          opt.value = u;
          list.appendChild(opt);
        });
      }
    }
    const swAllow = document.getElementById("swAllow");
    if (swAllow) swAllow.checked = !!cfg.allow_remote;
    invoke("set_allow_remote", { allow: !!cfg.allow_remote }).catch(() => {});
    hideWebOnlyBits();
    // 页面启动时 S.bridge.ok 是 false，不点一下"检测"它就不会把键鼠转出来。
    // 原生版这里直接触发一次，用户不需要手动点。
    const btn = document.getElementById("btnBridgeConnect");
    if (btn) setTimeout(() => btn.click(), 150);
  }

  document.addEventListener("DOMContentLoaded", () => {
    buildServerRow();
    const swAllow = document.getElementById("swAllow");
    if (swAllow) {
      swAllow.addEventListener("change", () => {
        invoke("set_allow_remote", { allow: swAllow.checked }).catch(() => {});
      });
    }
    const seg = document.getElementById("modeSeg");
    if (seg) {
      seg.addEventListener("click", (ev) => {
        const btn = ev.target && ev.target.closest ? ev.target.closest("[data-mode]") : null;
        if (!btn) return;
        invoke("config_set", { patch: { mode: btn.dataset.mode } }).catch(() => {});
      });
    }
    invoke("config_get")
      .then((view) => {
        applyConfig(view);
        // 配置说用服务器模式就切过去（页面默认会按 origin 猜）
        if (cfg.mode === "server") {
          const btn = document.querySelector('#modeSeg [data-mode="server"]');
          if (btn) btn.click();
        }
      })
      .catch((e) => console.error("config_get 失败", e));
    runSelftest();
    runAutotest();
  });

  /* ---------------- 自动化端到端测试驱动 ---------------- */
  // 由外层测试脚本用环境变量注入：
  //   MYDESK_AUTOTEST=host|guest  MYDESK_AUTOTEST_SERVER/ROOM/OUT/NATIVE/SHARE
  async function runAutotest() {
    let env = null;
    try {
      env = await invoke("autotest_env");
    } catch (e) {
      return;
    }
    if (!env) return;
    window.__MYDESK_AUTOTEST_OUT__ = env.out;
    const steps = [];
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const byId = (id) => document.getElementById(id);
    const state = () => (window.MyDesk && window.MyDesk.state) || null;
    const phase = () => (state() && state().phase) || "?";
    const waitFor = async (pred, ms, step = 150) => {
      const t0 = Date.now();
      while (Date.now() - t0 < ms) {
        try {
          if (pred()) return true;
        } catch (e) {
          /* ignore */
        }
        await sleep(step);
      }
      return false;
    };
    const write = async (name, obj) => {
      if (!env.out) return;
      try {
        await invoke("autotest_write", { path: env.out + "\\" + name, content: JSON.stringify(obj, null, 2) });
      } catch (e) {
        console.error("autotest_write 失败", e);
      }
    };
    const writeSteps = async () => {
      if (!env.out) return;
      try {
        await invoke("autotest_write", {
          path: env.out + "\\steps.json",
          content: JSON.stringify(steps, null, 2),
        });
      } catch (e) {
        /* ignore */
      }
    };
    // 每一步都立刻落盘：进程被杀/卡住时也能看到走到哪了
    const step = async (name, extra) => {
      steps.push({ at: new Date().toISOString(), name, extra });
      await writeSteps();
    };
    const snapshot = () => {
      const st = state() || {};
      const v = byId("remote");
      const tracks = (s) => (s ? s.getVideoTracks().map((t) => ({ state: t.readyState, settings: t.getSettings && t.getSettings() })) : []);
      return {
        at: new Date().toISOString(),
        phase: st.phase,
        dcCtl: st.dcCtl && st.dcCtl.readyState,
        dcInput: st.dcInput && st.dcInput.readyState,
        remoteAllows: st.remoteAllows,
        allowControl: st.allowControl,
        nativeCapture: !!window.__MYDESK_NATIVE_CAPTURE__,
        localVideo: tracks(st.localStream),
        remoteVideo: tracks(st.remoteStream),
        videoEl: v
          ? {
              w: v.videoWidth,
              h: v.videoHeight,
              t: Math.round(v.currentTime * 100) / 100,
              paused: v.paused,
              readyState: v.readyState,
              visibility: document.visibilityState,
              hidden: document.hidden,
            }
          : null,
        pill: (byId("pillText") || {}).textContent,
      };
    };

    // 1) 写服务器配置 + 切到服务器模式
    if (env.server) {
      cfg = Object.assign(cfg, await invoke("config_set", { patch: { server: env.server, mode: "server" } }));
      const input = byId("srvUrl");
      if (input) input.value = env.server;
    }
    await step("boot", { role: env.role, server: env.server, room: env.room });
    const srvBtn = document.querySelector('#modeSeg [data-mode="server"]');
    if (srvBtn) srvBtn.click();
    await sleep(200);

    if (env.force_native) window.__MYDESK_FORCE_NATIVE__ = true;

    // 无人值守测试时允许直接放开"允许对方控制我"
    if (env.role === "host" && env.allow) {
      cfg = Object.assign(cfg, await invoke("config_set", { patch: { allow_remote: true } }));
      const sw = document.getElementById("swAllow");
      if (sw) {
        sw.checked = true;
        sw.dispatchEvent(new Event("change", { bubbles: true }));
      }
      await invoke("set_allow_remote", { allow: true });
      await step("allow-remote", { on: true });
    }

    if (env.role === "host") {
      await step("create-click");
      byId("btnCreateRoom").click();
      const ok = await waitFor(() => /^[A-Z2-9]{6}$/.test((byId("roomCode") || {}).textContent || ""), 15000);
      const code = (byId("roomCode") || {}).textContent || "";
      await step("room-code", { ok, code, pill: (byId("pillText") || {}).textContent });
      await write("room.json", { ok, code, env });
      if (!ok) {
        await invoke("exit_app").catch(() => {});
        return;
      }
      // 有人加入就自动允许（原生弹窗在无人值守测试里没法点）
      await step("await-peer");
      await waitFor(() => {
        const ask = byId("peerAsk");
        if (ask && !ask.hidden) {
          byId("btnPeerAllow").click();
          return true;
        }
        return phase() === "live";
      }, 30000);
      // 共享屏幕（native 模式下走 Rust 截屏，不会弹系统选择框）
      if (env.share) {
        await waitFor(() => !(byId("btnShare") || {}).disabled, 5000);
        byId("btnShare").click();
      }
      await waitFor(() => phase() === "live", 30000);
      await sleep(1500);
      const inputStatus = await invoke("input_status").catch(() => null);
      const captureStats = await invoke("capture_stats").catch(() => null);
      const cursor = await invoke("cursor_pos").catch(() => null);
      await step("inject-status", { inputStatus, cursor });
      await write("host.json", Object.assign(snapshot(), {
        inputStatus,
        cursor,
        captureStats,
        drawnFrames: window.__MYDESK_CAPTURE_DRAWN__ || 0,
      }));
      await invoke("exit_app").catch(() => {});
      return;
    }

    if (env.role === "guest") {
      byId("joinCode").value = env.room;
      await step("join-click", { room: env.room, input: byId("joinCode").value });
      byId("btnJoinRoom").click();
      const wentLive = await waitFor(() => phase() === "live", 40000);
      let injected = null;
      if (wentLive) {
        const st = state();
        if (st && st.dcInput && st.dcInput.readyState === "open") {
          const before = await invoke("cursor_pos").catch(() => null);
          let sent = 0;
          let sendErr = null;
          [0.3, 0.45, 0.6, 0.7, 0.8, 0.85].forEach((x, i) => {
            setTimeout(() => {
              try {
                st.dcInput.send(JSON.stringify({ t: "m", x, y: 0.85, b: 0 }));
                sent += 1;
              } catch (e) {
                sendErr = String(e);
              }
            }, i * 120);
          });
          await sleep(1600);
          const after = await invoke("cursor_pos").catch(() => null);
          const stNow = await invoke("input_status").catch(() => null);
          injected = { before, after, sent, sendErr, allowControl: state() && state().allowControl, hostApplied: stNow && stNow.applied };
          await step("injected", injected);
        }
      }
      await step("live-wait", { wentLive, phase: phase(), pill: (byId("pillText") || {}).textContent });
      // 尾段整体包起来：任何一步抛异常都必须留下证据，而不是静默消失
      let tailError = null;
      try {
        const snap = snapshot();
        await step("pre-frames");
        const v = byId("remote");
        // 等一帧真正到画面上（有上限）
        await waitFor(() => !!(v && v.videoWidth > 0), 15000);
        // 用 getVideoPlaybackQuality 的差分数帧：纯取值，没有回调，不会挂死
        const q0 = v && v.getVideoPlaybackQuality ? v.getVideoPlaybackQuality().totalVideoFrames : -1;
        const t1 = v ? v.currentTime : 0;
        await sleep(1200);
        const q1 = v && v.getVideoPlaybackQuality ? v.getVideoPlaybackQuality().totalVideoFrames : -1;
        const t2 = v ? v.currentTime : 0;
        const frames = q0 >= 0 ? q1 - q0 : -1;
        await step("frames-done", { frames, q0, q1 });
        const finalSnap = snapshot();
        await write("guest.json", Object.assign(snap, finalSnap, {
          videoArrived: !!((byId("remote") || {}).videoWidth > 0),
          playing: t2 > t1,
          currentTime: t2,
          framesIn1200ms: frames,
          injected,
        }));
      } catch (e) {
        tailError = String((e && e.stack) || e);
      }
      if (tailError) {
        await write("guest-error.json", { tailError, steps, snapshot: snapshot() });
      }
      await invoke("exit_app").catch(() => {});
      return;
    }
  }

  /* ---------------- 自检（无 GUI 也能验证 WebView2 能力） ---------------- */
  async function runSelftest() {
    let on = false;
    try {
      on = await invoke("selftest_mode");
    } catch (e) {
      return;
    }
    if (!on) return;
    const probe = async (name, fn) => {
      try {
        return name + "=" + (await fn());
      } catch (e) {
        return name + "=ERR " + ((e && e.message) || e);
      }
    };
    const lines = [
      "href=" + location.href,
      "secureContext=" + window.isSecureContext,
      "ua=" + navigator.userAgent,
      await probe("RTCPeerConnection", () => typeof RTCPeerConnection),
      await probe("getDisplayMedia", () => !!(navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia)),
      await probe("getUserMedia", () => !!(navigator.mediaDevices && navigator.mediaDevices.getUserMedia)),
      await probe("canvasCaptureStream", () => !!document.createElement("canvas").captureStream),
      await probe("clipboard", () => !!(navigator.clipboard && navigator.clipboard.readText)),
      await probe("input_status", async () => JSON.stringify(await invoke("input_status"))),
      await probe("monitors", async () => JSON.stringify(await invoke("capture_monitors"))),
      await probe("inject_probe", async () => JSON.stringify(await invoke("inject_probe"))),
      await probe("capture_stats", async () => JSON.stringify(await invoke("capture_stats"))),
      await probe("config", async () => JSON.stringify(await invoke("config_get"))),
      await probe("ws_url", async () => await invoke("ws_url", { server: "https://desk.example.com" })),
    ];
    try {
      lines.push("report=" + (await invoke("selftest_report", { payload: lines.join("\n") })));
    } catch (e) {
      lines.push("report=ERR " + e);
    }
    invoke("selftest_done").catch(() => {});
  }
})();
