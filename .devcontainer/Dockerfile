FROM denoland/deno:bin-2.4.3 AS deno
FROM mcr.microsoft.com/devcontainers/typescript-node:20
COPY --from=deno /deno /usr/local/bin/deno

ENV DEBIAN_FRONTEND=noninteractive
ENV COPILOT_ALLOW_ALL=true

# ─────────────────────────────────────────────────────────────────────────────
# 汎用ツール
# ─────────────────────────────────────────────────────────────────────────────
RUN apt-get update && apt-get install -y \
    curl \
    wget \
    zsh \
    jq \
    fzf \
    && apt-get autoremove -y \
    && apt-get clean -y \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /usr/share/doc/fzf/examples \
    && curl -fsSL \
        "https://raw.githubusercontent.com/junegunn/fzf/master/shell/key-bindings.zsh" \
        -o /usr/share/doc/fzf/examples/key-bindings.zsh

# ─────────────────────────────────────────────────────────────────────────────
# Tauri用のネイティブ依存（Linux: WebKitGTK/appindicator backend）
# ＋ cargo-xwin でのWindowsクロスコンパイルに必要なclang/lld
# ─────────────────────────────────────────────────────────────────────────────
RUN apt-get update && apt-get install -y \
    pkg-config \
    libgtk-3-dev \
    libayatana-appindicator3-dev \
    libwebkit2gtk-4.1-dev \
    clang \
    lld \
    llvm \
    && apt-get autoremove -y \
    && apt-get clean -y \
    && rm -rf /var/lib/apt/lists/*

# ─────────────────────────────────────────────────────────────────────────────
# 日本語フォント（WSLg経由でTauriウィンドウを描画する際の文字化け防止）
# ─────────────────────────────────────────────────────────────────────────────
RUN apt-get update && apt-get install -y \
    fonts-noto-cjk \
    && apt-get autoremove -y \
    && apt-get clean -y \
    && rm -rf /var/lib/apt/lists/*

# ─────────────────────────────────────────────────────────────────────────────
# uv
# ─────────────────────────────────────────────────────────────────────────────
RUN curl -LsSf https://astral.sh/uv/install.sh | UV_INSTALL_DIR=/usr/local/bin sh

# ─────────────────────────────────────────────────────────────────────────────
# コマンド履歴用ディレクトリ（マウントポイントとして使用可能）
# ─────────────────────────────────────────────────────────────────────────────
RUN mkdir -p /commandhistory && chown node:node /commandhistory

# ─────────────────────────────────────────────────────────────────────────────
# GitHub Copilot CLI
# ─────────────────────────────────────────────────────────────────────────────
RUN curl -fsSL https://gh.io/copilot-install | bash

# ─────────────────────────────────────────────────────────────────────────────
# vscode ユーザーの設定
# ─────────────────────────────────────────────────────────────────────────────
USER node

# ─────────────────────────────────────────────────────────────────────────────
# Rust / Tauri（Windows向けクロスコンパイルにはcargo-xwinを使用）
# ─────────────────────────────────────────────────────────────────────────────
ENV PATH="/home/node/.cargo/bin:${PATH}"
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable \
    && rustup target add x86_64-pc-windows-msvc \
    && cargo install cargo-xwin

# pnpmはベースイメージのバージョンがNode.js 20と非互換のため9系に固定
RUN npm install -g pnpm@9

# ─────────────────────────────────────────────────────────────────────────────
# AI CLI 群（nodeユーザーで入れることで自動アップデートを可能にする）
# ─────────────────────────────────────────────────────────────────────────────
RUN npm install -g \
        @openai/codex \
        @google/gemini-cli

# ─────────────────────────────────────────────────────────────────────────────
# Claude Code（公式インストーラでネイティブバイナリを配置）
# ─────────────────────────────────────────────────────────────────────────────
RUN curl -fsSL https://claude.ai/install.sh | bash

# zsh-in-docker: Oh My Zsh + プラグイン設定
RUN SCRIPT_URL="https://github.com/deluan/zsh-in-docker/releases/download/v1.2.0/zsh-in-docker.sh" \
    && EXPECTED_SHA256="f74e5b08c295b6c3886654bb63c688e5ea16c58a4209435c4ddbab2c42fe9b41" \
    && curl -fsSL "$SCRIPT_URL" -o /tmp/zsh-in-docker.sh \
    && echo "$EXPECTED_SHA256 /tmp/zsh-in-docker.sh" | sha256sum -c - \
    && sh /tmp/zsh-in-docker.sh -- \
        -p git \
        -p zsh-autosuggestions \
        -p zsh-syntax-highlighting \
        -a 'HISTFILE=/commandhistory/.zsh_history' \
        -a 'setopt SHARE_HISTORY INC_APPEND_HISTORY' \
        -a 'export DISPLAY="${DISPLAY:-:0}"' \
        -a 'export PATH="$HOME/.local/bin:$PATH"' \
        -x \
    && rm -f /tmp/zsh-in-docker.sh

# ─────────────────────────────────────────────────────────────────────────────
# Claude Code データ永続化用ディレクトリ（マウントポイントとして使用可能）
# ─────────────────────────────────────────────────────────────────────────────
RUN mkdir -p /home/node/.claude && chown node:node /home/node/.claude

# ─────────────────────────────────────────────────────────────────────────────
# Codex CLI データ永続化用ディレクトリ（マウントポイントとして使用可能）
# ─────────────────────────────────────────────────────────────────────────────
RUN mkdir -p /home/node/.codex && chown node:node /home/node/.codex

# ─────────────────────────────────────────────────────────────────────────────
# Gemini CLI データ永続化用ディレクトリ（マウントポイントとして使用可能）
# ─────────────────────────────────────────────────────────────────────────────
RUN mkdir -p /home/node/.gemini && chown node:node /home/node/.gemini