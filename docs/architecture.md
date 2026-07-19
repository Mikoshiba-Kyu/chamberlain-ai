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
- Type I の実装は各トリガー内。core は plumbing (secret store、共通 `chamberlain.ai.complete` 等) を提供する
- **共通の依存**: 両方とも API キーが必要で、同じ secret store から読む

### 議論の規律

「Chamberlain が AI で〜」と言ったら通常は Type II、「トリガーで AI を使う」と言ったら Type I を指す。混同すると「チャット機能をトリガーごとに書くのか?」のような設計上おかしな話が発生する。曖昧なときはどちらを指しているか明示する。

### 現状の実装状況

- **Type II**: 実装済み (#14)。秘書チャット (`ChatPanel`) が Anthropic Messages API を叩き、履歴を `tauri-plugin-store` に永続化する
- **共通基盤**: secret store 実装済み (#13)。keyring クレート + Settings UI + dev 環境用の env-var fallback (`CHAMBERLAIN_SECRET_<UPPERCASE>`)
- **Type I**: 未実装。初の実装例として `github-issues-count` サンプルトリガーを予定 (#15)

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
│  ・永久 tick timer (1m / dev 10s)             │
│  ・per-trigger schedule 判定 (5m 以上)         │
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

`tokio::time::sleep(TICK_INTERVAL)` を回す非同期タスク。心拍は通常 `1m`、`CHAMBERLAIN_DEV=1` 時は `10s` に切り替わる (#17)。心拍は「発火判定を回す粒度」であり、実際の発火頻度はトリガー毎の `manifest.schedule` で決まる。

一度動き出したら停止イベントは無く、プロセスが生きている限り tick が刻まれる。TS 側が壊れても心臓は止まらない、という設計の要。

### Per-trigger schedule 判定 (#17 / #18)

心拍で回されるのは「発火判定」であって、実際に tick() を呼ぶかは per-trigger の schedule で決まる。

`manifest.schedule` は 1 フィールドで 2 系統を扱う。先頭文字で判別:

- 数字始まり (`"5m"` / `"1h"` 等) → **interval schedule** (#17 Phase 1)
- `@` 始まり (`"@daily 09:00"` 等) → **wall-clock schedule** (#18 Phase 2)

manifest フィールドが増えないのでエージェント開発者の学習コストが低い。パースは [`crate::schedule`] モジュールに一元化。

共通ルール:

- `last_fire_at` は tick() を呼んだら常に `now` に更新される (成功・エラー・null 返しに関わらず)。schedule の意味を「発火試行の頻度」に統一し、エラー時に毎心拍リトライになるノイズを避けるため
- `last_fire_at` の保存先: `triggers-state.json` の予約 namespace `__meta__.fire_times` (詳細: [状態モデル](#状態モデル) 節)
- schedule 省略 → discovery で完全に捨てる (manifest 不正と同じ扱い)
- schedule パース失敗 / interval 下限違反 / tz 解決失敗 → **トリガー自体は list_triggers に `error` フィールド付きで残す**。worker は load / tick しない。stderr と activity にも `[schedule error]` で流すが、activity は `.setup()` 内で emit されるため UI 未接続で捨てられる可能性が高く、`list_triggers().error` が実質的な観測面
- 予約 id `__meta__` → discovery で完全に捨てる (framework 内部用の namespace 衝突)

#### interval schedule (#17)

- DSL: `"5m"` / `"1h"` / `"10s"`。単位は `s` / `m` / `h`。単位無し・複合単位 (`1h30m`) は非対応
- 意味: 「前回 fire から N 経過したら次 fire」。TZ 非依存
- 判定式: `now - last_fire_at >= schedule`。初回 (`last_fire_at` が無い) は即発火する
- 最小粒度: 通常 `5m`、`CHAMBERLAIN_DEV=1` 時は `10s` (discovery で下限バリデーション)
- catch-up: `last_fire_at` を `now` に上書きするので、プロセス停止で溜まった発火は捨てられる (「1 回だけ発火」)

#### wall-clock schedule (#18)

- DSL: 5 種類の cron-lite エイリアス
  - `@hourly` — 毎時 :00
  - `@daily HH:MM` — 毎日 HH:MM
  - `@weekly [MON|TUE|WED|THU|FRI|SAT|SUN] HH:MM` — 毎週指定曜日の HH:MM (英字 3 文字大文字)
  - `@monthly D HH:MM` — 毎月 D 日 HH:MM。D が存在しない月 (2 月の 30 日等) は skip
  - `@at YYYY-MM-DDTHH:MM` — 特定日時に 1 回だけ fire、以降永久 skip
- full 5-field cron (`0 9 * * *`) は採らない (秘書用途では表現力過剰、cron parser クレート依存を避けたい、意味論を背負い込みたくない)
- 意味: TZ に紐付いた wall-clock 時刻で fire
- 判定式: `next_scheduled_after(last_fire_at, spec, tz) <= now`
- 最小粒度チェックは無し (wall-clock は最短でも `@hourly` = 60m で interval 最小 5m を大きく上回るため)
- **missed-fire policy: skip**。プロセス停止中に過ぎた予定は「秘書として今更 09:00 の挨拶しても不自然」なので捨てる
  - 実装: worker 起動時に「fire_times に履歴の無い wall-clock トリガー」を `startup_now` で seed。以降の判定は上記式で行われ、「起動時点で過ぎている当日の予定」は自然に skip される
  - 例: `@daily 09:00` のアプリを 10:00 に起動 → 今日の 09:00 は捨てて明日 09:00 を待つ
  - `@at` で期日が既に past の場合も同じ仕組みで永久 skip される (`next_scheduled_after` が `None` を返す)

#### TZ セマンティクス (#18)

- **デフォルト: user local** (OS TZ を [`iana_time_zone`] クレートで解決 → [`chrono_tz`] で TimeZone を取得)
- **上書き: `manifest.tz`** に IANA name (例: `"Asia/Tokyo"`)。省略時は user local
- interval schedule は TZ 非依存なので `tz` は wall-clock schedule のみ影響する
- shipped Tauri アプリはユーザーの OS TZ が正しく設定されている前提。dev container の TZ 問題は `.devcontainer/devcontainer.json` の `containerEnv.TZ` で解決済み (#17)

#### DST 明文化 (#18)

user local を採用した副作用として:

- **spring-forward (存在しない時刻)**: skip。「02:30 にセット→ 3 月の DST 日は 02:30 が存在しない」→ その日は fire しない、翌日 02:30 が返される
- **fall-back (重複時刻)**: 1 回だけ fire。「02:30 にセット→ 秋の DST 日は 02:30 が 2 回来る」→ 1 回目 (earlier UTC) のみ発火。2 回目の UTC 時刻 `X+1h` に対して `next_scheduled_after(X, ...)` は翌日の予定を返すので、2 回目は判定式で自然に skip される
- `@at` が spring-forward の gap にヒットした場合も skip (永久 fire しない)

JST は現状 DST 無しなので日本ユーザーには直接影響しないが、将来的にユーザーが海外環境で使う場合の期待挙動として明文化する。

dev モードは compile-time feature ではなく env-var 単独判定。「本番配布ビルドでは env を渡す口が Tauri bundle 側で塞がっている」ことを暗黙の前提にしている (詳細議論は #17)。

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

トリガー制御:

- `list_triggers() -> Vec<TriggerListItem>` — 起動時に discover したトリガーを UI 表示用に返す
- `pause_trigger(id: String)` / `resume_trigger(id: String)`

Secret store (#13):

- `list_declared_secrets() -> Vec<DeclaredSecretItem>` — トリガー manifest の `requiredSecrets` を集約して UI に返す。framework 必須の `anthropic_api_key` を先頭に含む
- `has_secret(name) -> bool` / `set_secret(name, value)` / `delete_secret(name)`

Type II チャット (#14):

- `chat_history() -> Vec<ChatMessage>` / `chat_send(message) -> ChatMessage` / `chat_clear()`

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
  "id": "greeter-morning",
  "name": "朝の挨拶",
  "description": "毎朝 06:00 におはようと言う",
  "entry": "index.ts",
  "schedule": "@daily 06:00",
  "tz": "Asia/Tokyo",
  "requiredSecrets": []
}
```

Rust 側で `serde_json` によりパース。unknown フィールドは silently 無視される (serde デフォルト)。

- `id` (必須) — namespace キー、activity source、UI 表示に使う。alphabetical で execution order が決まる。重複した場合は先勝ちで後発をスキップ + stderr log。予約語 `__meta__` は使えない (framework 内部用)
- `name` (必須) — UI に表示する人間可読名
- `description` (任意) — UI 詳細表示用
- `entry` (必須) — パッケージ dir 相対のエントリスクリプトパス。通常は `"index.ts"`
- `schedule` (必須) — 発火頻度の DSL 文字列
  - interval: `"5m"` / `"1h"` / `"6h"` 形式。単位は `s` / `m` / `h`。意味は「前回 fire から N 経過したら次 fire」。最小粒度は通常 `5m`、dev 時は `10s` (#17)
  - wall-clock: `"@daily 09:00"` / `"@weekly MON 09:00"` / `"@monthly 15 09:00"` / `"@hourly"` / `"@at 2026-08-01T18:30"` (#18)
- `tz` (任意) — wall-clock schedule 用の IANA TZ 名 (例: `"Asia/Tokyo"`)。省略時は OS の user local を [`iana_time_zone`] で解決。interval schedule では無視される (#18)
- `requiredSecrets` (任意) — このトリガーが `chamberlain.getSecret(name)` で読む予定の secret 名一覧。Settings UI が「未設定です」の表示に使う (#13)

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
  notify?: { title?: string; body: string };
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

`notify.title` を省略した場合、フレームワークが `manifest.name` を通知タイトルに使う。「どのタスクから来た通知か」は manifest が既に知っているので、トリガー側は通常 `body` だけを書けば良い。トリガー側で明示的に title を出したいとき (例: 同じトリガーが「エラー通知」と「成功通知」を出し分ける) だけ title を渡す。

### ambient global `chamberlain.*`

`ctx` は tick に渡される純粋データ (`{ now, state }`)。副作用のある API は ambient global の `chamberlain.*` として分離してある。deno_core の op で提供され、TS 側からは await で呼ぶ:

```typescript
chamberlain.getSecret(name: string): Promise<string | null>
chamberlain.ai.complete(opts: {
  prompt: string;
  system?: string;
  model?: string;    // 省略時は claude-sonnet-5
  maxTokens?: number;
}): Promise<string>
chamberlain.http.fetch(url: string, opts?: {
  method?: string;                   // 省略時 GET
  headers?: Record<string, string>;
  body?: string;
}): Promise<{ status: number; body: string }>
```

なぜ `ctx` に入れず ambient global にしたか: `ctx` は「今 tick のスナップショット」で pure data。keyring 参照や外部 API 呼び出しはスナップショットではないので分ける。将来 `chamberlain.readAsset(...)` 等もここに増える (未確定の論点参照)。

`chamberlain.http.fetch` が独立した op として存在するのは、rustyscript の JS runtime に Web `fetch` が入っていないため。「HTTP は core が握る (JS 側は薄い呼び出しだけ)」という方針を選んだ。理由は (1) 既に `ai.complete` で HTTP が core にある、(2) tauri app の権限モデル / ネットワーク境界を将来 core 側で一元管理しやすい、(3) rustyscript の web feature を有効化すると runtime が肥大化し JS 側挙動の予測性が下がる、の 3 点。

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
2. schedule 判定 → `now - last_fire_at < schedule` ならスキップ (詳細: [Per-trigger schedule 判定](#per-trigger-schedule-判定-17) 節)
3. state store から namespace `id` の値を読み出し (未保存なら `{}`)
4. TS `tick({ now, state })` を呼ぶ
5. 戻り値の `notify` を fire (OS 通知 + activity emit)
6. 戻り値の `state` を store に書き込み → save
7. `__meta__.fire_times[id]` に `now` を書き込み → save

**notify が state 保存より先** である点は意図的。プロセスクラッシュ時の "at least once" を優先する: 秘書は「1回多く言う > 一言忘れる」。同じイベントを2回通知する方が、忘れて未通知になるより秘書として望ましい。

### on-disk 形式

```json
{
  "greeter": { "greetCount": 42 },
  "stretch-reminder": { "lastFire": 1721000000000 },
  "__meta__": {
    "fire_times": {
      "greeter": 1721000000000,
      "stretch-reminder": 1721000060000
    }
  }
}
```

トップレベルのキーがトリガー ID (自動 namespace)。値は任意 JSON。framework は中身を関知しない。

**予約 namespace `__meta__`**: framework が内部管理する情報を置くための予約領域 (#17)。現在は `fire_times`: 「トリガー ID → 最終 fire 時刻 (ms since epoch)」のマップだけを持つ。この ID を名乗るトリガーは discovery で reject される。

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

TS 側 (`index.ts`) から自パッケージ内のアセット (prompt.md、schema.json 等) を読み出す API。AI 駆動トリガーの実現に必須 (system prompt を .md に外出しできる、等)。想定形は `chamberlain.readAsset("system-prompt.md")` のような呼び口を TS 側に公開し、実装は Rust 側で deno_core の op として提供する形 (`chamberlain.getSecret` / `chamberlain.ai.complete` と同じレイヤ)。

### shipped-app パス解決

現在 `packages/core::builder(config)` は `triggers_dir: PathBuf` を受け取る形になっており、位置解決はエージェント開発者側 (app crate の `main.rs` / `lib.rs`) の責務。`examples/react` では `env!("CARGO_MANIFEST_DIR")` で app crate 相対に解決しているが、これは dev-only。プロダクション配布時にトリガー群をどう bundle するか (Tauri resource dir?) と、エージェント開発者トリガーとフレームワーク組込トリガーの分離、が論点。

### ホットリロード

`triggers/**/*.ts` や manifest.json の変更を検出して Runtime を再構築する仕組み。dev DX 向上に効くが、V8 の再初期化コストと state 継続性の扱いが論点。

### cadence / 精度 / DSL (Phase 3 以降)

Phase 1 (#17) で interval schedule、Phase 2 (#18) で wall-clock schedule と TZ セマンティクスに着地した。詳細は上記 [Per-trigger schedule 判定](#per-trigger-schedule-判定-17--18) 節を参照。

Phase 3 以降で議論する余地がある論点:

- `chamberlain.time.tz` op (トリガー内で「今 UTC / user local で何時か」を TZ-aware に取れる op)。interval schedule + 動的判定パターン (MTG 30 分前通知等) を書けるようにする
- カレンダー統合トリガー (MTG N 分前通知の実応用例)
- 動的相対時刻 (`"MTG - 30m"` 等の DSL 表現) — 現状はトリガー内ロジックで書く方針

### notify API の一般化

現状は tick() の戻り値でメッセージを渡す return-based 形式で暫定合意。将来 ops で `chamberlain.notify(msg)` を呼べる副作用形式も許容するか、pure に統一するかは開き。

### AI 動的トリガー

「AI が日次で "今日見張るもの" を生成し、tick() がそれを見て動く」パターンを first-class にするか、開発者定義トリガー内で表現するかは未決。framework の抽象度を大きく左右する論点。

### pause 状態の永続化

現状は毎起動リセット。「停止したままにしておきたい」というユーザーの意図を respect するかは UX 判断。
