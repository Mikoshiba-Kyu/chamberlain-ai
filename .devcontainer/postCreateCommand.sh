#!/bin/bash
set -euo pipefail

echo "[*] postCreateCommand.sh を実行しています..."

# ─────────────────────────────────────────────────────────────────────────────
# .env セットアップ
# ─────────────────────────────────────────────────────────────────────────────
if [ -f ".env.example" ]; then
    if [ ! -f ".env" ]; then
        echo "  [*] .env.example をコピーして .env を作成しています..."
        cp .env.example .env
        echo "  [ok] .env を作成しました (.env.example からコピー)"
    else
        echo "  [skip] .env は既に存在するためスキップします"
    fi
else
    echo "  [skip] .env.example が見つからないためスキップします"
fi

# ─────────────────────────────────────────────────────────────────────────────
# Claude Code の ~/.claude.json を volume 内へ symlink
#   ~/.claude/ は volume マウント済みだが、~/.claude.json はホーム直下にあり
#   rebuild で消える。これが無いと再ログインを要求されるため symlink で退避する。
# ─────────────────────────────────────────────────────────────────────────────
CLAUDE_JSON_HOME="$HOME/.claude.json"
CLAUDE_JSON_VOL="$HOME/.claude/.claude.json"

if [ ! -L "$CLAUDE_JSON_HOME" ]; then
    if [ -f "$CLAUDE_JSON_HOME" ] && [ ! -f "$CLAUDE_JSON_VOL" ]; then
        echo "  [*] 既存の ~/.claude.json を volume 内へ移動しています..."
        mv "$CLAUDE_JSON_HOME" "$CLAUDE_JSON_VOL"
    fi
    [ -f "$CLAUDE_JSON_HOME" ] && rm -f "$CLAUDE_JSON_HOME"
    [ ! -f "$CLAUDE_JSON_VOL" ] && echo '{}' > "$CLAUDE_JSON_VOL"
    ln -s "$CLAUDE_JSON_VOL" "$CLAUDE_JSON_HOME"
    echo "  [ok] ~/.claude.json -> ~/.claude/.claude.json を symlink しました"
else
    echo "  [skip] ~/.claude.json は既に symlink のためスキップします"
fi
