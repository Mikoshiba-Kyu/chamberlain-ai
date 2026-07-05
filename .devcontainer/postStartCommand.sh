#!/bin/bash
set -euo pipefail

echo "[*] postStartCommand.sh を実行しています..."

# ─────────────────────────────────────────────────────────────────────────────
# GitHub CLI 認証確認
# ─────────────────────────────────────────────────────────────────────────────
echo "  [*] GitHub CLI の認証状態を確認しています..."
if gh auth status &>/dev/null; then
    echo "  [ok] GitHub CLI は認証済みです"
else
    echo ""
    echo "  [!] GitHub CLI が認証されていません。"
    echo "      WSL 側で認証済みの場合は gh credential の共有により自動で認証されます。"
    echo "      未認証の場合は以下のコマンドで認証してください:"
    echo ""
    echo "          gh auth login"
    echo ""
fi

# ─────────────────────────────────────────────────────────────────────────────
# Claude Code 認証確認
# ─────────────────────────────────────────────────────────────────────────────
echo "  [*] Claude Code の認証状態を確認しています..."
if claude --version &>/dev/null && claude config get -g &>/dev/null 2>&1; then
    echo "  [ok] Claude Code は認証済みです"
else
    echo ""
    echo "  [!] Claude Code が未認証または未設定です。"
    echo "      設定を永続化するには devcontainer.json に以下のマウントを追加してください:"
    echo ""
    echo "          \"mounts\": [\"source=claude-data,target=/home/node/.claude,type=volume\"]"
    echo ""
    echo "      認証するには以下のコマンドを実行してください:"
    echo ""
    echo "          claude"
    echo ""
fi