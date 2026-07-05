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
# Codex CLI
# ─────────────────────────────────────────────────────────────────────────────
RUN npm install -g @openai/codex

# ─────────────────────────────────────────────────────────────────────────────
# Gemini CLI
# ─────────────────────────────────────────────────────────────────────────────
RUN npm install -g @google/gemini-cli

# ─────────────────────────────────────────────────────────────────────────────
# vscode ユーザーの設定
# ─────────────────────────────────────────────────────────────────────────────
USER node

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