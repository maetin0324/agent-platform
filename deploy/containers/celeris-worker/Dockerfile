# Celeris の既定のワーカーイメージ（ADR-0043 D3。Phase 56 = A3）。
#
# `workspace.toml` が `[run] mode = "container"` と言っていて `[container] image` も `dockerfile` も
# 書いていないとき、タスクはこのイメージの中で走る。**ハーネスの CLI そのもの**（claude / codex /
# opencode）がこの中で起き、タスクのディレクトリ（worktree・artifacts・runs）は**同じパス**で
# マウントされる。
#
# 非 root のユーザは作らない: podman は `--userns=keep-id`、docker は `--user <uid>:<gid>` で
# **ホストと同じ uid** に見せるので、コンテナの中に同じ uid の行を用意する必要が無い
# （マウントした worktree にそのまま書ける）。そのため `/usr/local` 以下は誰でも読めるようにしておく。
#
# ビルド: `scripts/containers/build-worker.sh`（`cargo test` の一部ではない）
FROM debian:stable-slim

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    PATH=/usr/local/cargo/bin:/usr/local/node/bin:/usr/local/bin:/usr/bin:/bin \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup

# 道具立て（ADR-0043 D3: git, gh, rsync, ssh クライアント, ca-certificates, python3 + uv, node, rust）
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        ca-certificates curl xz-utils git rsync openssh-client \
        build-essential pkg-config libssl-dev \
        python3 python3-venv jq less ripgrep; \
    rm -rf /var/lib/apt/lists/*

# GitHub CLI（`gh`。ADR-0043 D5 の PR の経路をコンテナの中からも使えるように）
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    curl -fsSL "https://github.com/cli/cli/releases/download/v2.63.2/gh_2.63.2_linux_${arch}.tar.gz" -o /tmp/gh.tgz; \
    tar -xzf /tmp/gh.tgz -C /tmp; \
    install -m 0755 "/tmp/gh_2.63.2_linux_${arch}/bin/gh" /usr/local/bin/gh; \
    rm -rf /tmp/gh.tgz "/tmp/gh_2.63.2_linux_${arch}"

# Node LTS + ハーネスの CLI（claude-code / codex）
ARG NODE_VERSION=22.14.0
RUN set -eux; \
    arch="$(uname -m)"; \
    case "$arch" in x86_64) node_arch=x64 ;; aarch64) node_arch=arm64 ;; *) echo "unsupported $arch" >&2; exit 1 ;; esac; \
    curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${node_arch}.tar.xz" -o /tmp/node.tar.xz; \
    mkdir -p /usr/local/node; \
    tar -xJf /tmp/node.tar.xz -C /usr/local/node --strip-components=1; \
    rm /tmp/node.tar.xz; \
    npm install -g @anthropic-ai/claude-code @openai/codex; \
    npm cache clean --force; \
    chmod -R a+rX /usr/local/node

# Rust stable（rustup。`cargo` はホストと同じ版を使えるよう toolchain ごと入れる）
RUN set -eux; \
    curl -fsSL https://sh.rustup.rs -o /tmp/rustup.sh; \
    sh /tmp/rustup.sh -y --no-modify-path --profile minimal --default-toolchain stable --component clippy --component rustfmt; \
    rm /tmp/rustup.sh; \
    chmod -R a+rwX "$CARGO_HOME" "$RUSTUP_HOME"

# uv（python の環境。PaperQA / LDR 自体はホストで走るが、案件側の python はここで動かせる）
RUN set -eux; \
    curl -fsSL https://astral.sh/uv/install.sh -o /tmp/uv.sh; \
    UV_INSTALL_DIR=/usr/local/bin sh /tmp/uv.sh; \
    rm /tmp/uv.sh; \
    chmod a+rx /usr/local/bin/uv /usr/local/bin/uvx

# uid はホストから借りるので `HOME` は決め打ちにしない（Celeris が `--env HOME=<task_dir>` を渡す）。
# 既定のシェルはワーカープロトコルと関係ない（ハーネスの CLI が argv で渡る）。
WORKDIR /
CMD ["/bin/sh"]
