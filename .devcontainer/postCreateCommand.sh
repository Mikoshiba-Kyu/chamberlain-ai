#!/bin/bash
set -euo pipefail

echo "[*] postCreateCommand.sh を実行しています..."

# ─────────────────────────────────────────────────────────────────────────────
# Claude Code の LSP プラグイン
#   .claude/settings.json の enabledPlugins は有効化フラグでしかなく、
#   プラグイン本体 (~/.claude/plugins/cache/) は取得されない。明示的な
#   install が要る。
# ─────────────────────────────────────────────────────────────────────────────
if command -v claude >/dev/null 2>&1; then
    echo "  [*] Claude Code の LSP プラグインをセットアップしています..."
    # ネットワーク不通でも devcontainer 全体の作成は止めない
    if claude plugin marketplace add anthropics/claude-plugins-official >/dev/null 2>&1; then
        echo "  [ok] marketplace claude-plugins-official を登録しました"
    else
        echo "  [skip] marketplace は登録済みか、取得に失敗しました"
    fi
    for plugin in typescript-lsp rust-analyzer-lsp; do
        if claude plugin install "${plugin}@claude-plugins-official" >/dev/null 2>&1; then
            echo "  [ok] ${plugin} を install しました"
        else
            echo "  [warn] ${plugin} の install に失敗しました (後で手動実行してください)"
        fi
    done
else
    echo "  [skip] claude コマンドが見つからないためスキップします"
fi

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
