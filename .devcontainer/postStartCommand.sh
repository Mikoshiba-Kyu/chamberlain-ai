#!/bin/bash
set -euo pipefail

echo "[*] postStartCommand.sh を実行しています..."

# ─────────────────────────────────────────────────────────────────────────────
# D-Bus セッションバスと gnome-keyring を起動
# 目的: chamberlain の secret store (keyring クレート → Secret Service backend)
#       が devcontainer 内でも動作するようにする
# 詳細: README「DevContainer 内で動く / 動かないもの」
# ─────────────────────────────────────────────────────────────────────────────
echo "  [*] D-Bus / gnome-keyring を起動しています..."

if ! pgrep -f "dbus-daemon --session" > /dev/null 2>&1; then
    ADDR=$(dbus-daemon --session --fork --print-address)
    echo "export DBUS_SESSION_BUS_ADDRESS='$ADDR'" > "$HOME/.dbus-env"
    echo "  [ok] D-Bus session bus を起動しました ($ADDR)"
else
    echo "  [skip] D-Bus session bus は既に起動しています"
fi

# 以降の gnome-keyring 呼び出しのためこのシェルにも load
source "$HOME/.dbus-env"

if ! pgrep -f "gnome-keyring-daemon" > /dev/null 2>&1; then
    # dummy password で自動 unlock。devcontainer 内の secret を保護する意味は無いので
    # 固定値で OK。実 Windows/Mac 側では OS の keychain を素直に使う。
    echo -n "devcontainer" | gnome-keyring-daemon --daemonize --unlock --components=secrets > /dev/null 2>&1
    echo "  [ok] gnome-keyring-daemon を起動しました"
else
    echo "  [skip] gnome-keyring-daemon は既に起動しています"
fi

# ~/.zshenv に .dbus-env の source 行を冪等に入れる。
# base image (typescript-node) の common-utils feature が .zshrc を書き換えるため
# Dockerfile 側の zsh-in-docker -a フラグは残らない。.zshenv は zsh が常に最初に
# 読むので、テーマや plugins に関係なく生き残る。
DBUS_ENV_LINE='[ -f "$HOME/.dbus-env" ] && source "$HOME/.dbus-env"'
if ! grep -Fqx "$DBUS_ENV_LINE" "$HOME/.zshenv" 2>/dev/null; then
    echo "$DBUS_ENV_LINE" >> "$HOME/.zshenv"
    echo "  [ok] ~/.zshenv に dbus-env の source 行を追加しました"
else
    echo "  [skip] ~/.zshenv には既に dbus-env の source 行があります"
fi


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