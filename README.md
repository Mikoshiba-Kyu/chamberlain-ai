# Chamberlain

常駐する秘書エージェントアプリを作るための Tauri ベースのフレームワーク。

まず読むもの:
- [`docs/overview.md`](docs/overview.md) — 何を作るためのフレームワークか (vision, what/why)
- [`docs/architecture.md`](docs/architecture.md) — 今の骨格 (責務分割・契約・意思決定)
- [`AGENTS.md`](AGENTS.md) — 共同開発者としての振る舞い方 (人間 / エージェント問わず)

## レポ構造 (概略)

```
packages/
  core/                       # フレームワーク本体 (Rust クレート)
  create-chamberlain/         # scaffold CLI (段階A、publish なし)
examples/
  react/                      # 常設プレイグラウンド (フル Tauri + React アプリ)
docs/                         # 設計文書
```

詳細は `docs/architecture.md` の「レポ構造 (workspace)」節。

## 前提

- Node 20+ / pnpm 9+
- Rust stable
- Tauri prerequisites — OS 別。[Tauri 公式ガイド](https://v2.tauri.app/start/prerequisites/) 参照
- DevContainer 使用時は上記はコンテナ側で揃っている

## よく使うコマンド

### フレームワークの開発サイクル (日常)

```
pnpm install
cd examples/react
pnpm tauri dev
```

`packages/core` を触ると workspace 経由で自動反映される。`examples/react` が **フレームワーク開発者の日常サイクル用の常設プレイグラウンド**。DevContainer からでも WSLg 経由で窓が Windows 側に描画される (詳細は下記「DevContainer」節)。

### Windows exe を作る (CI 経由)

`main` に push すると GitHub Actions (`.github/workflows/build.yml`) が **windows-latest** で `examples/react` を Tauri でビルドし、`.exe` と bundle をアーティファクトとしてアップロードする。手元での cross-compile は現状使わない (メモリ `feedback_tauri_ci_windows_build` の教訓)。

Actions の実行後にアーティファクト `chamberlain-windows` をダウンロードし、Windows 実機で起動して確認する。

### scaffold の設計検証 (時々)

「別プロジェクトから core を呼び出しても動くか」を確かめたいときに:

```
pnpm scaffold:create     # $HOME/.chamberlain-scaffold-check に生成
pnpm scaffold:clean      # 生成物を削除
```

生成物は使い捨て。フレームワーク開発の日常サイクルには使わない (それは `examples/react` の仕事)。詳細は #9 のクローズコメント参照。

## API キー・シークレットの投入

Chamberlain の秘書 chat (`ChatPanel`) や `chamberlain.ai.complete` を使うトリガーは Anthropic の API キーを必要とする。設定方法は 2 通り:

### 通常経路: Settings UI からキーリングに保存

アプリを起動 → 「設定」タブ → `anthropic_api_key` に貼り付け → 保存。値は OS の credential manager (Windows Credential Manager / macOS Keychain / Linux Secret Service) に入る。Chamberlain 上には残らない。

### 開発用の逃げ道: `.env` (env-var fallback)

`chamberlain.getSecret(name)` は keyring を叩く前に `CHAMBERLAIN_SECRET_<UPPERCASE_NAME>` を先に見る。dev 環境で `.env` に書いておけば、`builder()` が起動時に dotenvy で読み込むので Settings UI での保存を省略できる。

**推奨位置**: workspace root (`/workspaces/chamberlain-ai/.env` 相当)。dotenvy は cwd 起点で親ディレクトリを辿るので、`examples/react/src-tauri/` から起動しても workspace root の `.env` が見つかる。

例:

```
# <workspace root>/.env
CHAMBERLAIN_SECRET_ANTHROPIC_API_KEY=sk-ant-...
CHAMBERLAIN_SECRET_GITHUB_TOKEN=ghp_...
```

env-var が設定されていれば keyring より優先。設定されていなければ従来通り keyring を使う。`.env` / `.env.local` / `.env.*.local` は `.gitignore` 済み。

**scaffold で作った外部プロジェクトを使うとき** (`pnpm scaffold:create` 後) は、生成先ディレクトリの中に `.env` を置く (workspace root からは cwd 的に届かないため)。

## DevContainer

VS Code Remote-Containers を使うと、以下が自動でセットアップされる:

- **WSLg 経由の display 転送** — `DISPLAY=:1`, `WAYLAND_DISPLAY`, `/tmp/.X11-unix` マウント。`pnpm tauri dev` の窓は Windows 側に描画される
- **WebKitGTK + 日本語フォント** (`.devcontainer/Dockerfile`)
- **D-Bus セッションバス + gnome-keyring** — `postStartCommand.sh` が起動時に立ち上げる。dummy password で auto-unlock される。keyring クレート → Secret Service backend が動作する

### 動くもの

- `cargo check --workspace` / `pnpm --filter chamberlain-example-react build` — コンパイル・型検査
- `pnpm tauri dev` の窓表示、React / CSS の反復
- トリガー discovery + activity ログ
- **secret store** (Settings UI からの保存も、`.env` 経由の env-var も、両方)
- **秘書チャット** (API キーを入れれば実際に Anthropic に POST される)
- **`chamberlain.ai.complete` からの API 呼び出し** (トリガー側から)

### 動かないもの (仕様の制約)

- **系統トレイのアイコン表示** — WSLg が Linux 用の systray host を提供しない。tray 作成コード自体は動くが、Windows のタスクバーにアイコンは出ない
- **OS 通知バブルの実発火** — devcontainer 内に notification daemon が居ないので Windows 側のトースト通知としては見えない。activity ログには残るので観測面原則は生きる (#16)

上記が問題になるのは「トレイをクリックしたときの挙動を目で見たい」「通知の見た目を確認したい」といったケース。それらは CI ビルドの Windows exe で確認する。

### 出るが無害な警告 (WSLg 特有)

- `libayatana-appindicator is deprecated ...` — Tauri が依存する系統トレイライブラリの upstream 警告。うちからは直せない
- `Gtk-CRITICAL ... gtk_widget_get_scale_factor: assertion 'GTK_IS_WIDGET (widget)' failed` — WSLg で tray widget が取れないときに Tauri 内部が吐く警告。動作には影響しない
- `Couldn't get key from code: Backquote` (等) — WebKitGTK が WSLg 環境の keyboard layout で一部 keycode の変換に失敗したときの警告。日本語入力ができないのも同じ層 (WSLg → WebKitGTK IME ブリッジ未整備) の話

いずれも Windows 実機ビルドでは出ない。

## トリガー側から見える API (現状)

トリガー (`triggers/*/index.ts`) は ambient global `chamberlain.*` を通じて Rust 側と対話する。

```ts
chamberlain.getSecret(name: string): Promise<string | null>
chamberlain.ai.complete(opts: {
  prompt: string;
  system?: string;
  model?: string;    // 省略時は claude-sonnet-5
}): Promise<string>
```

将来 `chamberlain.readAsset(...)` 等がここに足される。`ctx` は tick に渡される純粋データ (`{ now, state }`) で、side-effect API は `chamberlain.*` 側に分けてある。詳細は `docs/architecture.md` 参照。

## Issue と設計論点

進行中の設計論点・タスクは GitHub Issues で追跡。`gh issue list` を見るのが一次情報。

「未確定の論点」の全体像は `docs/architecture.md` の同名節。
