# syntax=docker/dockerfile:1
#
# Build environment for the Linux desktop client (Tauri 2 + WebKitGTK).
# The CI job mounts the repository and runs cargo inside this image, so the
# toolchain and system libraries are pinned instead of relying on the runner.
#
#   docker build -f docker/linux-build.Dockerfile -t mydesk-linux-build .
#   docker run --rm -v "$PWD:/src" -w /src --user "$(id -u):$(id -g)" \
#     -e CARGO_HOME=/src/desktop/.cargo-home -e HOME=/tmp \
#     mydesk-linux-build bash -lc 'cd desktop && cargo build --release'

FROM ubuntu:22.04

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
        patchelf \
        rpm \
    && rm -rf /var/lib/apt/lists/*

# 固定 Rust 版本，和 rust-toolchain 保持一致
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain 1.82.0 \
    && rustc --version && cargo --version

# Tauri CLI（打包 deb/AppImage 时需要）
RUN cargo install tauri-cli --version "^2" --locked

WORKDIR /src
CMD ["bash"]
