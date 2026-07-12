# Chamberlain 基本設計

## 位置づけ

このドキュメントは、Chamberlain が現時点で「どういう骨格で動いているか」を記録する参照資料である。

- [`overview.md`](overview.md) は「何を作るためのフレームワークで、ユーザーとどう関わるか」を書く (vision, what/why)
- [`../AGENTS.md`](../AGENTS.md) は「共同開発者としてどう振る舞ってほしいか」を書く (contribution stance)
- 本ドキュメントは「今のコードがどう組み上がっているか」を書く (how it works now)

「なぜその設計を選んだか」の議論は各 GitHub Issue (#3, #6, #7, #8 など) と commit メッセージに残す。ここでは結論だけを書き、詳細は Issue へのリンクで辿れるようにする。

想定読者は Chamberlain フレームワーク側を触る開発者 (現時点では自分たち)。フレームワーク上でアプリを作る開発者向けのドキュメントは、フレームワークが安定した時点で別途書く。

## 用語 (3 つの役割)

Chamberlain には常に 3 種類の役割が登場する。「ユーザー」と呼ぶと混乱するため、以下で使い分ける。

- **フレームワーク開発者** — Chamberlain 本体を作る人。現時点では我々。`packages/core` を触る
- **エージェント開発者** — Chamberlain を使って秘書エージェントアプリを作る人。create-chamberlain (予定) のユーザー。`examples/react` を雛形として自分のアプリを組み立てる
- **エンドユーザー** — エージェント開発者が配布した秘書アプリを使う人。UI やロジックは変えられない

以降のドキュメントと Issue ではこの 3 語で明示的に呼び分ける。

## AI の 2 種類 (Type I / Type II)

Chamberlain には性質の異なる 2 種類の AI が存在する。設計・実装を議論するときは常にこの区別を明示する。

### Type I: タスク AI

個別のトリガーが必要に応じて呼ぶ LLM。

- モデル・provider・prompt は **エージェント開発者の裁量**
- 短命 — 1 tick の中で API 呼び出しが完結する
- 例: GitHub issue の要約、届いたメールの分類、コード変更のレビュー

### Type II: 秘書自身の AI

Chamberlain という秘書の persona そのもの。

- チャットで会話する主体、ユーザーの習慣・好みを学習する主体
- **framework 側 (`packages/core`) の責務**
- 長命 — 会話履歴とユーザーモデルが永続する

### 実装的な棲み分け

- Type II は core が提供する。エージェント開発者は persona 実装をカスタムしない (create したテンプレの UI ソースは触れるが、Type II の AI 実装は core 由来)
- Type I の実装は各トリガー内。core は plumbing (secret store、共通 `ctx.ai.complete` 等) を提供する
- **共通の依存**: 両方とも API キーが必要で、同じ secret store から読む

### 議論の規律

「Chamberlain が AI で〜」と言ったら通常は Type II、「トリガーで AI を使う」と言ったら Type I を指す。混同すると「チャット機能をトリガーごとに書くのか?」のような設計上おかしな話が発生する。曖昧なときはどちらを指しているか明示する。

### 現状の実装状況

Type I / Type II ともに **未実装**。framework 側の共通基盤 (secret store と `ctx.ai.complete`) を先に整備する予定 (#13, #14)。初の実装例として Type I トリガーを 1 個作る予定 (#15)。

## レポ構造 (workspace)

Chamberlain は cargo + pnpm の 2 系統 workspace として構成される (フレームワーク開発サイクルの詳細は #11 参照)。

```
chamberlain-ai/
├── packages/
│   └── core/                       # フレームワーク本体 (Rust クレート)
│       └── src/lib.rs              # builder(config) + framework logic
├── examples/
│   └── react/                      # 常設プレイグラウンド (フル Tauri + React アプリ)
│       ├── src/                    # React フロントエンド
│       ├── src-tauri/
│       │   ├── Cargo.toml          # chamberlain-core を path 依存
│       │   ├── tauri.conf.json
│       │   └── src/
│       │       ├── lib.rs          # chamberlain_core::builder(...) を呼ぶだけ
│       │       └── main.rs
│       └── triggers/               # サンプル TS トリガー
├── Cargo.toml                      # [workspace]
├── package.json                    # workspace root (最小)
└── pnpm-workspace.yaml
```

- **`packages/core`** — フレームワーク本体。将来 `chamberlain-core` として crates.io に publish される (#10)
- **`examples/react`** — フレームワーク開発者の日常サイクル用。core を workspace path 依存で参照するので、`packages/core` を編集したら即反映される。将来 `templates/react` として create-chamberlain の元ネタになる (#9)
- **workspace root** — pnpm-workspace / cargo workspace のルート。Cargo.lock はここに 1 つ

### フロントエンドフレームワークの選択肢

秘書 UI (トリガー一覧・アクティビティフィード・将来のチャット) は**フレームワーク機能**であり、エージェント開発者ごとに実装するものではない (`overview.md` の思想と整合)。一方、create-chamberlain 実行時にフロントエンドフレームワーク (React / Vue / 等) を選ばせる方向で設計する (Tauri の create-tauri-app と同じ発想)。

これから必然的に以下の構造になる:

- **contract 本体** は `packages/core` (Rust) が持つ invoke commands + event の集合
- **UI 実装は言語ごとにパラレル**: `templates/react/`, 将来 `templates/vue/` 等
- 共通 helper 層 (`packages/ui-react` 等) を切るかは Vue 対応が視野に入ってから判断

現時点では React 版のみが `examples/react` として存在する。

## 全体像 (2層アーキテクチャ)

Chamberlain は「常駐する Rust コア」と「エージェント開発者が書く TS トリガー」の2層で構成される。

```
┌─────────────────────────────────────────────┐
│            Rust コア (常駐)                   │
│                                             │
│  ・永久 tick timer (現状 10s)                 │
│  ・tray icon                                │
│  ・OS notification                          │
│  ・JS runtime host (deno_core embedded)     │
│  ・trigger discovery                        │
│  ・state store (JSON)                       │
└─────┬──────────────────────────────┬────────┘
      │                              │
 tick │ activity event   discovery / │ tick / call tick()
      ▼                              ▼
┌──────────────────┐         ┌──────────────────┐
│  Web UI (React)  │         │    triggers/     │
│                  │         │                  │
│  ・トリガー一覧    │         │   <id>/          │
│  ・アクティビティ  │         │    manifest.json │
│  ・停止/再開      │         │    index.ts      │
└──────────────────┘         │    <assets>      │
                             └──────────────────┘
```

- **Rust コア** = 永久に動いている心臓。時計、トレイ、OS 通知、JS ランタイム、永続化を担う
- **TS トリガー** = エージェント開発者が書くビジネスロジック。各トリガーは自分の `tick()` を提供
- **Web UI** = 秘書の動作全体を観測する窓 (詳細は [観測面](#観測面-observability-plane) 節)

責務分割の要諦は [`overview.md`](overview.md) と一致する: コアが常駐性・トレイ・OS 通知・チャット UI を提供し、エージェント開発者は「いつ何を確認して何を通知するか」に集中する。

## Rust コアの責務

### Heartbeat tick

`tokio::time::sleep(TICK_INTERVAL)` を回す非同期タスク。現在 `TICK_INTERVAL = 10s`。将来的には 1〜5 分程度 (「早すぎず遅すぎず」) を framework の約束とする方向で、cadence の議論は未確定 (別 Issue 予定)。

一度動き出したら停止イベントは無く、プロセスが生きている限り tick が刻まれる。TS 側が壊れても心臓は止まらない、という設計の要。

### Trigger discovery と runtime

起動時に `triggers/*/manifest.json` を走査し、各パッケージから `id`, `entry` を得る。同一の rustyscript Runtime に N モジュールをロードして保持する (V8 isolate は 1 つだけ)。

Runtime は V8 の thread affinity を守るため、専用の `std::thread` に閉じ込める。tokio 側からは `std::sync::mpsc` で tick 信号を送るだけ。JS 実行は常にこの 1 スレッド上で直列に行われる。

失敗は隔離される: 1トリガーの load / instantiate / tick() が失敗しても、そのトリガーだけスキップされ、他は続行する。エラーは activity ストリームに `[error]` / `[load error]` / `[instantiate error]` プレフィックス付きで emit される。

### State store

`tauri-plugin-store` の JSON ファイル (`<app_data>/triggers-state.json`) を採用。トリガー ID が自動 namespace になり、トリガーは自分の state しか触らない。

Rust 側からは `read_trigger_state(app, id)` / `write_trigger_state(app, id, value)` の薄いラッパで扱う。詳細は [状態モデル](#状態モデル) 節。

### OS 通知

`tauri-plugin-notification` を使用。パーミッションチェックを含む。

Windows では起動時に AUMID (Application User Model ID) をレジストリに自己登録する。これがないと Windows の Action Center 側で通知源が識別できず、開発中の一時通知として扱われる。

### トレイ

Tauri の `TrayIconBuilder`。メニュー: Open Chamberlain / Send test notification / Quit。ウィンドウの close イベントは常に "hide + prevent close" にフックされ、明示的な Quit までプロセスは残る。

### UI 向け invoke commands

現時点で 3 つ:

- `list_triggers() -> Vec<TriggerListItem>` — 起動時に discover したトリガーを UI 表示用に返す
- `pause_trigger(id: String)` — 指定 ID を停止
- `resume_trigger(id: String)` — 指定 ID を再開

pause 状態は `Arc<AtomicBool>` per trigger で in-memory 保持。再起動でリセット (MVP 判断、UX 上の必要が出た時点で永続化を検討)。

## トリガーの契約 (エージェント開発者が書くもの)

### パッケージ構造

各トリガーはディレクトリで表現される。

```
triggers/
  <id>/
    manifest.json   # 必須
    index.ts        # 必須 (エントリ)
    <assets>        # 任意 (prompt.md, schema.json, ...)
```

Chrome 拡張 / VS Code 拡張 / npm package と同じ mental model。単一ファイル形式との併存は取らない (「.ts が正か? ディレクトリが正か?」の混乱を避けるため)。決定の経緯は #8。

### manifest.json スキーマ

```json
{
  "id": "sample-10s",
  "name": "10秒サンプル",
  "description": "10秒ごとにテスト通知を出す",
  "version": "0.1.0",
  "author": "Mikoshiba-Kyu",
  "entry": "index.ts"
}
```

Rust 側で `serde_json` によりパース。

- `id` (必須) — namespace キー、activity source、UI 表示に使う。alphabetical で execution order が決まる。重複した場合は先勝ちで後発をスキップ + stderr log
- `name` (必須) — UI に表示する人間可読名
- `description` (任意) — UI 詳細表示用
- `version`, `author` (任意) — 現状 UI には露出していないが、将来コミュニティ配布・marketplace 用のメタとして parse だけしておく
- `entry` (必須) — パッケージ dir 相対のエントリスクリプトパス。通常は `"index.ts"`

manifest を分離ファイルにする理由は「Rust が JS を動かさずに一覧を作れる」「Chrome/VS Code/npm と同じパターンで開発者に説明不要」「将来 marketplace の話が出た時にそのまま嵌る」など。決定の経緯は #8。

### index.ts の contract

エントリスクリプトは `tick(ctx)` 関数を export する。TypeScript は rustyscript が内部で transpile する。

```typescript
type State = { /* トリガー固有 */ };

interface Ctx {
  now: number;      // ms since epoch (Rust から渡される)
  state: State;     // 前回 tick() が返した state (未保存なら {})
}

interface TickResult {
  notify?: { message: string };
  state?: State;
}

export function tick(ctx: Ctx): TickResult | null {
  // ...
}
```

戻り値のルール:

- `null` — 何もしない (通知も state 保存もなし)
- `{ notify }` — OS 通知 + activity emit
- `{ state }` — state を丸ごと差し替えて永続化 (部分更新は自前スプレッド)
- `{ notify, state }` — 両方
- `{}` — 何もしない (`null` と等価)

## 状態モデル

### なぜ pure functional か

「tick() が state を丸ごと返す」形は Node/Deno 風の副作用 API (`ctx.state.set(k, v)`) より制約が強い。しかし利点が大きい:

- **1トランザクション**: Rust 側で "notify + state 保存" を単一 tick 内でまとめて扱える
- **テスト容易**: 純粋関数なので `tick({ now: fake, state: fake })` を呼ぶだけで挙動を確認できる
- **dry run**: 未来の `now` を仮定して「もしいまチェックされたら何を返すか」が試せる
- **副作用の順序を framework が管理**: 開発者は「何をしたいか」だけを返し、順序は framework が保証

代償: 部分更新は開発者が自前スプレッド (`{ ...ctx.state, lastFire: ctx.now }`) で書く。state が肥大すると毎 tick コピーが発生する。秘書スケール (単一ユーザー、数十〜数百エントリ程度) では実害なし。

議論の経緯は #7。

### tick 内の順序

per-trigger per-tick で以下の順で実行される:

1. `paused` 判定 → true ならスキップ
2. state store から namespace `id` の値を読み出し (未保存なら `{}`)
3. TS `tick({ now, state })` を呼ぶ
4. 戻り値の `notify` を fire (OS 通知 + activity emit)
5. 戻り値の `state` を store に書き込み → save

**notify が state 保存より先** である点は意図的。プロセスクラッシュ時の "at least once" を優先する: 秘書は「1回多く言う > 一言忘れる」。同じイベントを2回通知する方が、忘れて未通知になるより秘書として望ましい。

### on-disk 形式

```json
{
  "sample-10s": { "tickCount": 259 },
  "greeter": { "greetCount": 42 },
  "stretch-reminder": { "lastFire": 1721000000000 }
}
```

トップレベルのキーがトリガー ID (自動 namespace)。値は任意 JSON。framework は中身を関知しない。

保存先は Tauri が管理する `<app_data>/triggers-state.json`:

- Linux: `~/.local/share/<identifier>/`
- Windows: `%APPDATA%\<identifier>\`
- macOS: `~/Library/Application Support/<identifier>/`

`<identifier>` は `tauri.conf.json` の `identifier` (現状 `dev.chamberlain.interval-notifier`)。

## 観測面 (Observability Plane)

Chamberlain の設計原則の1つ:

> **すべての OS 側動作 (通知発火・提案提示・トリガー実行) は、必ず UI 上のアクティビティログにも同時に記録される。**

これにより:

- 開発者は Linux dev (WSLg) だけで秘書の意味論を検証できる (OS 描画は別レイヤ)
- ユーザーは「見逃した通知」を UI で振り返れる
- 「なぜ秘書がこう振る舞ったか」の説明可能性が担保される

背景は #6。

### Activity イベント

Rust 側から Tauri の `Emitter::emit("activity", ...)` で JS 側に届く。

```typescript
interface ActivityEvent {
  ts: number;       // ms since epoch
  source: string;   // trigger ID
  message: string;  // 通知本文、または [error] / [load error] / [instantiate error] プレフィックス付きエラー
}
```

UI 側 (`chamberlainApi.onActivity`) は `@tauri-apps/api/event` の `listen` で購読する。表示は新しい順に直近 200 件 (`MAX_EVENTS`)。

### エラーもここに流す

トリガーの load / instantiate / tick() 失敗も activity イベントとして emit される (プレフィックス付き)。UI を見るだけで「どのトリガーが壊れているか」がわかる。

開発者は Rust の stderr を追わなくても UI で異常を検出できる、というのが観測面原則の副産物。

## 決着済みの意思決定

各項目の詳細は元 Issue と commit メッセージ。ここでは 1 行サマリのみ。

### 実行環境 — deno_core (rustyscript ラッパ経由) を Rust に埋め込む (#3)

TS を開発者に書かせたい / webview JS は隠しウィンドウで不確実 / fetch/timers/ESM が要る。QuickJS 系は fetch を自作する必要があり framework の実装コストが高い。deno_core は Deno 本体の心臓部で battle-tested。

### 永続化バックエンド — tauri-plugin-store (#7)

標準プラグイン、cross-platform のパス解決込み、SQLite より始めるコストが低い。将来 SQLite 相当が必要になれば置き換え可能 (state レイヤの API を Rust コア内に隠せている限り、影響は閉じる)。

### API 形状 — pure functional (#7)

上記「なぜ pure functional か」参照。

### トリガーはパッケージ構造 (単一ファイル形式は不採用) (#8)

AI 駆動トリガーは prompt / MD / スキーマ等のアセットを持ち込むのが基本形。1ファイル前提は前提を間違えている。単一ファイル併存は「.ts が正か? ディレクトリが正か?」の混乱を撒く。

### 順序 — notify が state 保存より先 (#7)

プロセスクラッシュ時の "at least once" を優先。1回多く言う > 忘れる。

### 既知の gotcha

新規に rustyscript / deno_core を導入する時に踏む可能性が高いので明記しておく。

**cdylib を crate-type から削る必要がある**: V8 の内部 TLS が `R_X86_64_TPOFF32` relocation を生成し、rust-lld の `-shared` 出力に置けない。desktop 用途では `["staticlib", "rlib"]` で十分。モバイル対応時は platform 別調整が必要。

**serde を `=1.0.219` にピンする必要がある**: swc_config 3.0.0 (rustyscript → deno_ast 経由) が削除された `serde::__private::de` を触っている。swc_config 3.x に patch release は無い。rustyscript が新 deno_ast を取り込んだ時点で解除可能。

## 未確定の論点

現時点で議論すべきタイミングになっていない、あるいは実装優先度が下がっている論点。順不同。

### アセット読み込み API

TS 側 (`index.ts`) から自パッケージ内のアセット (prompt.md、schema.json 等) を読み出す API。AI 駆動トリガーの実現に必須 (system prompt を .md に外出しできる、等)。想定形は `ctx.readAsset("system-prompt.md")` のような呼び口を TS 側に公開し、実装は Rust 側で deno_core の op として提供する形。

### shipped-app パス解決

現在 `packages/core::builder(config)` は `triggers_dir: PathBuf` を受け取る形になっており、位置解決はエージェント開発者側 (app crate の `main.rs` / `lib.rs`) の責務。`examples/react` では `env!("CARGO_MANIFEST_DIR")` で app crate 相対に解決しているが、これは dev-only。プロダクション配布時にトリガー群をどう bundle するか (Tauri resource dir?) と、エージェント開発者トリガーとフレームワーク組込トリガーの分離、が論点。

### ホットリロード

`triggers/**/*.ts` や manifest.json の変更を検出して Runtime を再構築する仕組み。dev DX 向上に効くが、V8 の再初期化コストと state 継続性の扱いが論点。

### cadence / 精度 / DSL

現状は全トリガー同一チック (10s)。将来は 1〜5 分の framework 約束 + トリガーごとの hint (`warnBefore: "1h"` 等) を DSL で表現する方向。精度の framework 約束をどう明示するかも論点。

### notify API の一般化

現状は tick() の戻り値でメッセージを渡す return-based 形式で暫定合意。将来 ops で `chamberlain.notify(msg)` を呼べる副作用形式も許容するか、pure に統一するかは開き。

### AI 動的トリガー

「AI が日次で "今日見張るもの" を生成し、tick() がそれを見て動く」パターンを first-class にするか、開発者定義トリガー内で表現するかは未決。framework の抽象度を大きく左右する論点。

### pause 状態の永続化

現状は毎起動リセット。「停止したままにしておきたい」というユーザーの意図を respect するかは UX 判断。
