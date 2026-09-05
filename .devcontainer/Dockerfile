# syntax=docker/dockerfile:1
#
# Discordia development container. Everything the workspace needs to fmt, lint
# and test on Linux — so an agent can run with --dangerously-skip-permissions
# against a filesystem that holds nothing but this repo.
#
# NOT covered, and cannot be: running the client. wry needs a display, voice
# needs an audio device and screen capture is Windows/macOS-only, so the
# `#[ignore]`d platform tests stay on the host. The client *compiles* here,
# which is what CI's desktop-build job checks.
# Trixie, not bookworm: the dx release binary is built against glibc 2.39 and
# bookworm ships 2.36, so dx there dies with `GLIBC_2.39 not found` — a linker
# error that says nothing about the distro being the cause. Building dx from
# source instead would cost ~15 minutes on every image rebuild.
FROM mcr.microsoft.com/devcontainers/rust:1-trixie

# Three groups, in order: init-firewall.sh; the wry/GTK/ALSA set CI installs
# for the client (.github/workflows/ci.yml — keep the two lists together);
# clang and cmake for the bindgen and C++ build scripts under livekit
# (webrtc-sys) and sqlx's bundled SQLite. lld is the linker, see the compose
# file for why. sqlite3 is for reading a data dir by hand.
RUN apt-get update && export DEBIAN_FRONTEND=noninteractive \
    && apt-get install -y --no-install-recommends \
        iptables ipset iproute2 dnsutils jq ca-certificates \
        libwebkit2gtk-4.1-dev libgtk-3-dev libxdo-dev \
        libasound2-dev libssl-dev pkg-config \
        clang libclang-dev cmake lld \
        sqlite3 pinentry-tty openssh-client unzip \
    && apt-get clean && rm -rf /var/lib/apt/lists/*

# Source paths are relative to the build context, which is the repo root — not
# to this file.
#
# Baked into the image rather than run from .devcontainer/ on the bind mount,
# and this is the load-bearing part: the mounted copy is writable by the agent.
# A root-invocable script that the agent can first edit is not a restriction,
# it is a way to become root. Root owns these, vscode cannot write them, and
# editing one therefore takes an image rebuild — which is also why a change to
# the allowlist does not take effect until you rebuild.
COPY .devcontainer/init-firewall.sh .devcontainer/prepare-mounts.sh /usr/local/bin/
RUN chown root:root /usr/local/bin/init-firewall.sh /usr/local/bin/prepare-mounts.sh \
    && chmod 0755 /usr/local/bin/init-firewall.sh /usr/local/bin/prepare-mounts.sh

# Named-volume mount points that DO exist in the image, so Docker seeds each
# volume with this ownership on first use. The volume mounted inside the
# workspace gets no such inheritance — prepare-mounts.sh exists for that one.
RUN install -d -o vscode -g vscode \
        /home/vscode/.claude \
        /home/vscode/.dx \
        /commandhistory

# The base image grants `vscode` blanket passwordless sudo. That would let a
# skipped-permissions agent flush the very firewall it is meant to be held by,
# so it goes, and the two commands that must stay root replace it. To add a
# package, rebuild with it in the apt line above — the mutable-root escape
# hatch is a large part of what the allowlist is protecting.
RUN rm -f /etc/sudoers.d/vscode \
    && echo 'vscode ALL=(root) NOPASSWD: /usr/local/bin/init-firewall.sh, /usr/local/bin/prepare-mounts.sh' \
        > /etc/sudoers.d/discordia-devcontainer \
    && chmod 0440 /etc/sudoers.d/discordia-devcontainer

# dx is not optional tooling here: trap 13 says a new Tailwind class only
# reaches client/assets/tailwind.css through `dx build`, and that file is
# committed. The version tracks DIOXUS_CLI_VERSION in .github/workflows/ci.yml,
# which CI checks against the dioxus version in Cargo.lock — bump all three
# together. binstall, because building dx from source costs ~10 minutes.
#
# cargo/registry and cargo/git are created before the chown even though both
# are empty: a named volume inherits ownership from the image path it covers
# and lands root:root when that path is absent. binstall downloads binaries
# rather than crates, so nothing else in this build creates either one, and the
# first `cargo check` then fails on its own cache directory.
RUN curl -fsSL https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash \
    && cargo binstall dioxus-cli@0.7.10 cargo-audit --no-confirm --locked \
    && install -d /usr/local/cargo/registry /usr/local/cargo/git \
    && chown -R vscode:vscode /usr/local/cargo /usr/local/rustup
