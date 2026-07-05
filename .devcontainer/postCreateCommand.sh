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
