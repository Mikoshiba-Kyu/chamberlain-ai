# Chamberlain へのコントリビュート

Chamberlain フレームワーク本体を開発・改善する方向けのガイドです。エージェントアプリを Chamberlain の上に作りたい場合は [`docs/getting-started.md`](docs/getting-started.md) を参照してください。

## まず読むもの

- [`docs/overview.md`](docs/overview.md) — 何を作るためのフレームワークか (vision, what/why)
- [`docs/architecture.md`](docs/architecture.md) — 今の骨格 (責務分割・契約・意思決定)

## 開発環境

### DevContainer (第一選択)

VS Code Remote-Containers を使うと、以下が自動でセットアップされます。基本的にはこちらを使ってください。

- **WSLg 経由の display 転送** — `DISPLAY=:1`, `WAYLAND_DISPLAY`, `/tmp/.X11-unix` マウント。`pnpm tauri dev` の窓は Windows 側に描画されます
- **WebKitGTK + 日本語フォント** (`.devcontainer/Dockerfile`)
- **D-Bus セッションバス + gnome-keyring** — `postStartCommand.sh` が起動時に立ち上げます。dummy password で auto-unlock されるため、keyring クレート → Secret Service backend が動作します
- **TZ=Asia/Tokyo** (`containerEnv`) — トリガー内 (rustyscript の V8) と UI (WebKitGTK) の両方が親プロセスの TZ を継承します。デフォルトの UTC のままだと greeter や ActivityPanel の時刻が UTC になり、誤発火・混乱の原因になります。UTC 圏の開発者は `.devcontainer/devcontainer.json` の `containerEnv.TZ` を自分のタイムゾーン (例: `America/Los_Angeles`) に書き換えてリビルドしてください。shipped Tauri アプリは端末 OS の TZ を使うので、この設定は dev-only です

### DevContainer を使わない場合

前提物 (Node / Rust / Tauri prereq) のセットアップは [`docs/getting-started.md`](docs/getting-started.md#前提) の「前提」節を参照してください。同じ prereq で workspace を回せます。

## レポ構造

```
packages/
  core/                            # フレームワーク本体 (Rust クレート)
                                   #   → crates.io: chamberlain-core
  create-chamberlain/              # scaffold CLI
                                   #   → npm: create-chamberlain
    templates/react/               # React テンプレ (CLI に同梱、version 依存で配布)
examples/
  react/                           # フレームワーク開発者向けの常設プレイグラウンド
                                   #   (path 依存、workspace 内で dev サイクルに使う)
docs/                              # 設計文書
```

詳細は [`docs/architecture.md`](docs/architecture.md) の「レポ構造 (workspace)」節をご覧ください。

## `templates/react` と `examples/react` の関係

役割が違うので併存させています。

- **`examples/react`** — フレームワーク開発者用。`chamberlain-core` を `path` で参照し、workspace 内で `pnpm tauri dev` の日常サイクルに使います
- **`packages/create-chamberlain/templates/react`** — エージェント開発者に配布する雛形。`chamberlain-core = "0.1"` の version 依存で書かれ、`create-chamberlain` が npm パッケージに同梱します

**同期ルール**: どちらか一方の shape (トリガー構成、UI パネル、Cargo/pnpm 設定) を変更したら、もう一方にも手で反映してください。テンプレの `Cargo.toml` / `tauri.conf.json` は placeholder な既定値 (`chamberlain-app`, `Chamberlain App`, `com.example.chamberlain-app` 等) にしてあり、scaffold 時に `create-chamberlain` の CLI が実 name に書き換えます。

## 日常の開発サイクル

```
pnpm install
cd examples/react
pnpm tauri dev
```

`packages/core` を触ると workspace 経由で自動反映されます。`examples/react` が **フレームワーク開発者の日常サイクル用の常設プレイグラウンド** です。DevContainer からでも WSLg 経由で窓が Windows 側に描画されます。

## scaffold の動作検証

publish された `create-chamberlain` のふるまいを確認したいときは、以下を実行します。

```
node packages/create-chamberlain/bin/create.js /tmp/chamberlain-scaffold-check
```

生成物は使い捨てです。フレームワーク開発の日常サイクルには使いません (それは `examples/react` の仕事です)。**publish 前は生成物の `cargo check` が「`chamberlain-core = "0.1"` が crates.io に無い」で失敗するのが正常** です。テンプレは配布状態を前提に書かれています。

## DevContainer の挙動詳細

### 動くもの

- `cargo check --workspace` / `pnpm --filter chamberlain-example-react build` — コンパイル・型検査
- `pnpm tauri dev` の窓表示、React / CSS の反復
- トリガー discovery + activity ログ
- **secret store** (Settings UI からの保存も、`.env` 経由の env-var も、両方)
- **秘書チャット** (API キーを入れれば実際に Anthropic に POST されます)
- **`chamberlain.ai.complete` からの API 呼び出し** (トリガー側から)

### 動かないもの (仕様の制約)

- **系統トレイのアイコン表示** — WSLg が Linux 用の systray host を提供しません。tray 作成コード自体は動きますが、Windows のタスクバーにアイコンは出ません
- **OS 通知バブルの実発火** — devcontainer 内に notification daemon が居ないので、Windows 側のトースト通知としては見えません。activity ログには残るので観測面原則は生きています (#16)

上記が問題になるのは「トレイをクリックしたときの挙動を目で見たい」「通知の見た目を確認したい」といったケースです。それらは CI ビルドの Windows exe で確認します。

### 出るが無害な警告 (WSLg 特有)

- `libayatana-appindicator is deprecated ...` — Tauri が依存する系統トレイライブラリの upstream 警告。うちからは直せません
- `Gtk-CRITICAL ... gtk_widget_get_scale_factor: assertion 'GTK_IS_WIDGET (widget)' failed` — WSLg で tray widget が取れないときに Tauri 内部が吐く警告。動作には影響しません
- `Couldn't get key from code: Backquote` (等) — WebKitGTK が WSLg 環境の keyboard layout で一部 keycode の変換に失敗したときの警告。日本語入力ができないのも同じ層 (WSLg → WebKitGTK IME ブリッジ未整備) の話です

いずれも Windows 実機ビルドでは出ません。

## CI

PR と main への push で [`.github/workflows/ci.yml`](.github/workflows/ci.yml) が走ります。手元で先に確認したいときは以下と同じことをしています。

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo publish -p chamberlain-core --dry-run
pnpm --filter chamberlain-example-react build
```

**Tauri の bundle (msi / nsis / deb) は CI では作りません。** Chamberlain はフレームワークであり、実行ファイルを作るのはエージェント開発者の手元だからです。CI が見るのはコンパイル・lint・配布物の中身までです。

`cargo publish --dry-run` と `npm pack --dry-run` を PR 時点で回しているのは、crates.io / npm の publish が事実上取り消せないためです。`packages/core/Cargo.toml` の `include` を触ったときに壊れやすい箇所なので、マージ前に検出します。

## バージョンとリリース

バージョニング (lockstep)、0.x のセマンティクス、1.0 の定義、タグ・ブランチ規約、publish 手順は [`docs/versioning.md`](docs/versioning.md) にまとまっています。ブランチを切る前とリリース作業の前に確認してください。

## Issue と設計論点

進行中の設計論点・タスクは GitHub Issues で追跡しています。`gh issue list` を見るのが一次情報です。「未確定の論点」の全体像は [`docs/architecture.md`](docs/architecture.md) の同名節にまとまっています。

---

このリポジトリは生成AI向けの指示を [AGENTS.md](AGENTS.md) に記載しています。
