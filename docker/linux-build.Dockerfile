# syntax=docker/dockerfile:1
#
# Build environment for the Linux desktop client (Tauri 2 + WebKitGTK).
# The CI job mounts the repository and runs cargo inside this image, so the
# toolchain and system libraries are pinned instead of relying on the runner.
#
#   docker build -f docker/linux-build.Dockerfile -t mydesk-linux-build .
#   docker run --rm -v "$PWD:/src" -w /src --user "$(id -u):$(id -g)" \
#     -e HOME=/tmp -e CARGO_HOME=/src/desktop/.cargo-home \
#     mydesk-linux-build bash -lc 'cd desktop && cargo build --release'
#
# Why trixie instead of ubuntu:22.04: xcap pulls pipewire-rs, whose libspa-sys
# build script runs bindgen against the *system* PipeWire headers. 22.04 ships
# 0.3.48, which predates spa_meta_first / spa_meta_region_is_valid and the
# spa_video_info_raw.flags field, so libspa does not compile there at all.
# trixie ships PipeWire 1.4.x, which the crate expects.

FROM debian:trixie

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_TERM_COLOR=always \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential curl wget file ca-certificates git pkg-config \
        libssl-dev \
        libwebkit2gtk-4.1-dev \
        libgtk-3-dev \
        libayatana-appindicator3-dev \
        librsvg2-dev \
        libxdo-dev \
        libxkbcommon-dev \
        libwayland-dev \
        libpipewire-0.3-dev \
        libspa-0.2-dev \
        libgbm-dev \
        libdrm-dev \
        libdbus-1-dev \
        libclang-dev \
        clang \
        patchelf \
        rpm \
    && rm -rf /var/lib/apt/lists/*

# 固定 Rust 版本，和 rust-toolchain 保持一致
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain 1.82.0 \
    && rustc --version && cargo --version

# Tauri CLI（打包 deb/AppImage 时需要）
RUN cargo install tauri-cli --version "^2" --locked

# 把关键库版本打进构建日志：一旦某个 -sys crate 因头文件太旧而报错，
# 日志里能直接看出是哪个库、什么版本。
RUN echo "== system library versions ==" \
    && for p in libpipewire-0.3 libspa-0.2 webkit2gtk-4.1 javascriptcoregtk-4.1 \
                gtk+-3.0 libdrm gbm dbus-1 wayland-client; do \
         printf '%-24s %s\n' "$p" "$(pkg-config --modversion "$p" 2>/dev/null || echo '(missing)')"; \
       done \
    && echo "== clang ==" && clang --version | head -1

WORKDIR /src
CMD ["bash"]
