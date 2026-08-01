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
│       └── src/
│           ├── lib.rs              # builder() + discovery + invoke commands + TauriHost
│           ├── worker.rs           # 心拍の配線層 (WorkerHost 境界)
│           ├── history.rs          # 実行履歴レイヤ (SQLite / retention)
│           ├── tasks.rs            # タスクリストと分類 (純関数)
│           ├── schedule.rs         # schedule DSL と発火時刻の計算 (純関数)
│           ├── secrets.rs          # keyring + JS op
│           ├── chat.rs / ai.rs     # Type II 秘書チャット / Anthropic クライアント
│           └── http.rs             # トリガー向け fetch
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
│  ・schedule 展開器 → タスクリスト               │
│  ・due タスクの取り出しと実行                   │
│  ・tray icon                                │
│  ・OS notification                          │
│  ・JS runtime host (deno_core embedded)     │
│  ・trigger discovery                        │
│  ・state store + task store (JSON)          │
└─────┬──────────────────────────────┬────────┘
      │                              │
 tick │ activity event   discovery / │ tick / call tick()
      ▼                              ▼
┌──────────────────┐         ┌──────────────────┐
│  Web UI (React)  │         │    triggers/     │
│                  │         │                  │
│  ・トリガー一覧    │         │   <id>/          │
│  ・予定リスト      │         │    manifest.json │
│  ・アクティビティ  │         │    index.ts      │
│  ・停止/再開/手動  │         │    <assets>      │
└──────────────────┘         └──────────────────┘
```

- **Rust コア** = 永久に動いている心臓。時計、トレイ、OS 通知、JS ランタイム、永続化を担う
- **TS トリガー** = エージェント開発者が書くビジネスロジック。各トリガーは自分の `tick()` を提供
- **Web UI** = 秘書の動作全体を観測する窓 (詳細は [観測面](#観測面-observability-plane) 節)

責務分割の要諦は [`overview.md`](overview.md) と一致する: コアが常駐性・トレイ・OS 通知・チャット UI を提供し、エージェント開発者は「いつ何を確認して何を通知するか」に集中する。

### スケジュールの実体はタスクリスト (#26)

0.2.0 でスケジューラの骨格が入れ替わった。`manifest.schedule` は「発火条件の記述」ではなく **絶対時刻の生成規則** であり、展開器がそれを具体的な時刻を持つタスクに変換する。

```
manifest.schedule ──(展開器: 閾値条件)──▶ タスクリスト ──(心拍: due 取り出し)──▶ 実行
                                             ▲                                  │
手動実行 / (将来) Type II ──(直接積む)────────┘                                  ▼
                                                                          実行/破棄の記録
```

タスクリストは「未来への意図」を表す単一の場所であり、manifest 由来のタスクも手動実行で積まれたタスクも同じリストに並ぶ。これにより:

- **心拍から schedule の解釈が消えた**。心拍は「`scheduled_at <= now` を取り出して実行して消す」だけを行う
- **秘書が未来に対して書ける場所ができた**。Phase 3 でチャット / Type II からの登録がここに乗る
- **観測面が強化された**。「秘書がこれから何をするつもりか」が 1 画面で見え、しかも削除できる

## Rust コアの責務

### Heartbeat tick

`tokio::time::sleep(TICK_INTERVAL)` を回す非同期タスク。心拍は通常 `1m`、`CHAMBERLAIN_DEV=1` 時は `10s` に切り替わる。心拍は「due なタスクを取り出す粒度」であり、発火時刻の精度そのものではない (時刻はタスクの `scheduled_at` が持っている)。

一度動き出したら停止イベントは無く、プロセスが生きている限り tick が刻まれる。TS 側が壊れても心臓は止まらない、という設計の要。

手動実行 (`run_trigger_now`) は心拍を待たずに割り込む。UI 側から `mpsc::Sender` で心拍を 1 回起こす。

心拍 1 回の流れ:

1. **展開** — `expanded_until` が閾値に迫ったトリガーをホライズンまで展開する
2. **due 取り出し** — `scheduled_at <= now` なタスクを昇順に取る。ここに schedule の解釈は入らない
3. **分類** — 孤児 / pause / 遅延超過を破棄し、残りを実行する
4. **後片付け** — 処理済みタスクを消して 1 回だけ永続化する

### 心拍の層分け (#46)

心拍は 3 層に分かれている。**下の層ほど副作用を持たず、テストできる。**

| 層 | 置き場 | 持つもの |
|---|---|---|
| 判断 | `tasks.rs` / `schedule.rs` | 「due なタスク 1 件をどう扱うか」「次の発火時刻はいつか」。純関数 |
| 配線 | `worker.rs` | 副作用を繋ぐ**順序**。`heartbeat(host, specs, store, now, grace)` が一巡を持つ |
| 接続 | `lib.rs` の `TauriHost` | `AppHandle` / `rustyscript::Runtime` / ロード済みモジュールの実体 |

配線層が触れる副作用は `WorkerHost` trait の 6 メソッド (state 読み書き / `tick()` 呼び出し / OS 通知 / activity / タスク永続化) に数え上げてある。**ここに無い副作用を worker が持ってはならない** — 持った瞬間その経路がテストの外に出るため。

`heartbeat` が `now` を引数に取るので、テストは fake host を差して時計を進めるだけでシナリオを書ける (長期停止からの復帰・スリープ復帰・pause 中の due・schedule 変更・孤児掃除・at-least-once)。JS 実行も境界の裏なので、心拍のテストに V8 は要らない。

### schedule DSL (#26 決定事項 4 / 5)

`manifest.schedule` は `@` 始まりの生成規則のみを受け付ける。**0.2.0 で interval schedule (`"5m"` / `"1h"`) は廃止された。**

| 記法 | 発火時刻 |
|---|---|
| `@hourly` | 毎時 :00 |
| `@hourly :45` | 毎時 :45 |
| `@every 10m` | 毎時 :00,:10,:20,:30,:40,:50 |
| `@daily HH:MM` | 毎日 HH:MM |
| `@weekly [MON\|TUE\|WED\|THU\|FRI\|SAT\|SUN] HH:MM` | 毎週指定曜日の HH:MM (英字 3 文字大文字) |
| `@monthly D HH:MM` | 毎月 D 日 HH:MM。D が存在しない月 (2 月の 30 日等) は skip |
| `@at YYYY-MM-DDTHH:MM` | その時刻に 1 回だけ |

`@hourly :MM` と `@every Nm` は生成器として同一である。どちらも「毎時、指定した分集合で発火」であり、前者は分集合が 1 要素、後者は N 等分。実装は分集合を作る関数 1 本。

**`@every <N>m` は N が 5 / 10 / 15 / 20 / 30 のみ。** 60 の約数なら毎時同じ分集合になるため、展開が各時間で独立・冪等になり、展開済み境界 1 つで完結する。下限 5 分は旧 interval の下限を踏襲している。

採らないもの:

- **interval 系統** — 展開型では interval は「wall-clock の生成規則のひとつ」に格下げされ、`@hourly` が 1 時間 interval と同義になる。さらに `"7m"` のようなグリッドに割り切れない値は日の境目で間隔が崩れ、展開器が「前回展開の末尾時刻」を覚える必要が生じる (境界 1 つで済まなくなる)
- **カンマ区切りリスト** (`@hourly :00,:15,:45`) — 認めると「曜日リストは? 月リストは?」と滑り出して cron に着地する (#18)
- **full 5-field cron** (`0 9 * * *`) — 秘書用途では表現力過剰、cron parser クレート依存を避けたい、意味論を背負い込みたくない
- **`CHAMBERLAIN_DEV=1` での秒スケール** — グリッドが分単位である以上、秒は原理的に載らない。dev の反復手段は手動実行ボタンに移った

discovery でのバリデーション:

- schedule 省略 → discovery で完全に捨てる (manifest 不正と同じ扱い)
- schedule パース失敗 / tz 解決失敗 → **トリガー自体は list_triggers に `error` フィールド付きで残す**。worker は load / 展開しない。stderr と activity にも `[schedule error]` で流すが、activity は `.setup()` 内で emit されるため UI 未接続で捨てられる可能性が高く、`list_triggers().error` が実質的な観測面
- 発火間隔の下限チェックはもう無い。下限は DSL パーサが構文として担保する (`@every` の許可値が 5 分以上)
- 予約 id `__meta__` → discovery で完全に捨てる (framework 内部用の namespace 衝突)

### 展開器 (#26 決定事項 2 / 3 / 6)

展開器は core が固定で持つ。「1 日 1 回の展開」自体をタスクとしてリストに積む (self-hosting) 形は綺麗だが、そのタスクを消されたら秘書が二度と動かなくなるので、ブートストラップの堅牢性を優先した。

**展開済み境界**: トリガーごとに `expanded_until` を 1 つだけ持つ。展開器はこの境界より後の時刻しか生成しない。境界より前にあった「秘書 AI やエンドユーザーが消したタスク」は二度と生成されないので、tombstone 無しで冪等性が自明になる。

**閾値条件で起こす**: Chamberlain はデスクトップアプリで常時起動していない。日次パスを 00:00 に置くと、9:00〜18:00 しか PC を開かないユーザーの環境では一度も走らない。代わりに心拍ごとに条件を見る。

```
心拍ごとに: expanded_until < now + 24h なら展開する (ホライズン 48h)
```

比較 1 つなのでコストは無視でき、「結果として 1 日 1 回程度走る」が保証される。閾値 24h とホライズン 48h の差が余裕として働く。

**過去は生成しない**: 展開の起点は `max(expanded_until, now)`。1 週間アプリを閉じていた後の起動で 1 週間分の過去タスクを生成してから全部破棄する、という無駄と観測面のノイズを避ける。wall-clock の missed-fire skip (#18) は「生成しない」ことで達成される。

展開対象は「実行可能なトリガー」だけ。構成エラーのあるトリガーや JS のロードに失敗したトリガーを展開しても、実行できないタスクを積んで due 時に破棄するだけになる。

### 起動時の突き合わせ (#26 決定事項 7 / 追加決定 9)

アプリのアップデートで `manifest` が変わるため、起動時に永続タスクリストを現在の manifest と突き合わせる。

- **schedule 変更検知** — `schedule` / `tz` 文字列を前回値と比較し、変わっていたらそのトリガーの未実行タスク (schedule 由来のみ) を破棄して `expanded_until` を `now` に戻す。これが無いと `@daily 09:00` → `@daily 08:00` の変更が最大 48 時間反映されない
- **孤児掃除** — `trigger_id` が現存トリガー集合に無いタスクを破棄する。放置すると心拍が解決できず、due になった瞬間から失敗し続ける

「アプリを閉じている間に予定が過ぎた」ぶんはここでは扱わない。それは再起動に固有の話ではないので、心拍側の[猶予超過の掃除](#猶予超過の掃除-50--53)が担当する。


突き合わせには **discovery で見えている全トリガー**を渡す (ロードに失敗したものも含む)。ロード失敗は一時的なこともあり、それだけで展開済み境界や積まれたタスクを破棄すると「1 回のビルド事故でスケジュールの記憶が消える」ことになる。

猶予窓は設けない。Mastra の `missesBeforeDelete` は「デプロイ順序でスケジューラがワークフロー登録より先に回る」レース対策だが、Chamberlain の `discover_triggers()` は起動時 1 回で確定するのでこのレースが存在しない。

### 猶予超過の掃除 (#50 / #53)

猶予を超えた schedule 由来タスクは**絶対に実行されない** (`classify_due` が `SkippedLate` を返す)。これを心拍が 1 件ずつ発見して報告すると、長期の不在から復帰したときに同じことを言う行が観測面を埋める。48h 先まで展開された状態で 2 日空くと `@every 5m` で 576 件になり、**その回にしか出ない `[expanded]` / `[rescheduled]` を押し流す**。#42 で履歴が永続化されてからは retention も食う。

そこで **due 取り出しの前に一括で掃除し、トリガーごとにまとめて報告する**。判定は `classify_due` と同一で、変えているのは報告の粒度だけ。

- **2 件以上なら `[stale]` に畳む** — 「n 件の予定を遅延で破棄しました (最古 …)」。個々の時刻より「いつから空いていたか」が要る場面
- **1 件なら従来どおり `[skipped]`** — 心拍が 1 回遅れただけ、という日常的なケース。1 行にまとめても情報が増えず、代わりにその予定の時刻が消える

掃除は**心拍の中**にある。#50 では起動時突き合わせだけの特別扱いにしていたが、それだとプロセスを保ったままのスリープ復帰 (ノート PC を閉じて週末を挟む等) が塞がらなかった (#53)。掃除が心拍にあれば「再起動したか」を区別する必要がなくなる。

ad-hoc は対象外。猶予 24h 内なら遅れても実行しなければならず (決定事項 8)、超過分は手動由来で件数が知れているので `[expired]` として 1 件ずつ扱う。

### due 判定と missed-fire (#26 決定事項 8 / 追加決定 10 / 11)

心拍が due なタスクを取り出したときの分岐。起動時もスリープ復帰も通常運転も経路は 1 本である (「起動時の掃除」という専用経路を持たない)。

| 条件 | 処理 | 記録 |
|---|---|---|
| 親トリガーが現存しない | 破棄 | `[orphaned]` |
| 親トリガーが実行不能 (構成エラー / ロード失敗) | 破棄 | `[unavailable]` |
| 親トリガーが pause 中 | 破棄 | `[paused]` |
| 遅れ ≤ 猶予 | **実行** | 通常の notify / activity |
| schedule 由来で猶予超過 | 破棄 | `[skipped]` |
| ad-hoc で猶予超過 | 破棄 | `[expired]` |

**猶予は origin で違う。**

- **schedule 由来: 心拍 2 回分** (prod 2m / dev 20s)。「心拍が拾い損ねた」ぶんだけを許す。#18 の missed-fire policy: skip と整合し、「今更 09:00 の挨拶をしても不自然」を守る。定期タスクには次の機会がある
- **ad-hoc: 24h**。一回きりで消えたら二度と来ないので破棄してはならない。遅延を明示して実行する。ただし無制限だと「1 週間前のリマインドが起動時に一斉に鳴る」ので上限を置く

> Issue #26 決定事項 8 のフロー図は「遅れ < 猶予 → 実行」を両 origin 共通に書いているが、猶予を 24h 一律にすると同じ決定事項が挙げている「今更 09:00 の挨拶をしても不自然」と矛盾する。両方の意図を満たす読みは origin 別の猶予しかないため、そう実装している。

**pause はトリガー側の属性**であり、タスクレコードには持たせない。pause 中も展開は続き、タスクはリストに積まれたまま見える (「止めている間に何が来ていたか」が可視化されるほうが秘書メタファに合う)。due 取り出し時に破棄されるので、resume 時の特別処理も要らない。

**遅延はタスクの `scheduled_at` から導出する**。framework は通知本文に前置きを足さず、`tick(ctx)` に `scheduledAt` / `delayMs` を渡してトリガー / Type II 側に判断させる。通知の文面はエージェント開発者の領分であり、framework が前置きを注入すると文体・言語・敬体が破綻する。

実行順序は state読 → `tick(ctx)` → notify → state保存 → **タスク削除**。タスク削除を最後にすることで、実行中にクラッシュしたタスクはリストに残り次回起動で再試行される (at-least-once)。秘書は「1 回多く言う > 一言忘れる」の立場を取る。`tick()` がエラーを返してもタスクは消す (エラーで毎心拍リトライになるノイズを避ける)。

#### TZ セマンティクス (#18)

- **デフォルト: user local** (OS TZ を [`iana_time_zone`] クレートで解決 → [`chrono_tz`] で TimeZone を取得)
- **上書き: `manifest.tz`** に IANA name (例: `"Asia/Tokyo"`)。省略時は user local
- shipped Tauri アプリはユーザーの OS TZ が正しく設定されている前提。dev container の TZ 問題は `.devcontainer/devcontainer.json` の `containerEnv.TZ` で解決済み (#17)

#### DST 明文化 (#18)

user local を採用した副作用として:

- **spring-forward (存在しない時刻)**: skip。「02:30 にセット→ 3 月の DST 日は 02:30 が存在しない」→ その日は fire しない、翌日 02:30 が返される
- **fall-back (重複時刻)**: 1 回だけ fire。「02:30 にセット→ 秋の DST 日は 02:30 が 2 回来る」→ 1 回目 (earlier UTC) のみ発火。2 回目の UTC 時刻 `X+1h` に対して `next_scheduled_after(X, ...)` は翌日の予定を返すので、2 回目は判定式で自然に skip される
- `@at` が spring-forward の gap にヒットした場合も skip (永久 fire しない)

JST は現状 DST 無しなので日本ユーザーには直接影響しないが、将来的にユーザーが海外環境で使う場合の期待挙動として明文化する。

dev モードは compile-time feature ではなく env-var 単独判定。「本番配布ビルドでは env を渡す口が Tauri bundle 側で塞がっている」ことを暗黙の前提にしている (詳細議論は #17)。0.2.0 では dev モードが緩和するのは心拍だけである。

### Trigger discovery と runtime

起動時に `triggers/*/manifest.json` を走査し、各パッケージから `id`, `entry` を得る。同一の rustyscript Runtime に N モジュールをロードして保持する (V8 isolate は 1 つだけ)。

トリガーディレクトリの位置は Tauri の resource dir 経由で解決する (#19)。エージェント開発者の `tauri.conf.json` に `bundle.resources: { "../triggers/": "triggers/" }` を宣言してもらうと、`tauri-build` (build.rs) が dev では `target/{debug,release}/triggers/` に、shipped では platform ごとの resource dir (Windows: exe と同居 / Linux: `/usr/lib/{name}/` or `${APPDIR}/usr/lib/{name}/` / macOS: `{name}.app/Contents/Resources/`) にコピーする。core は `app.path().resolve("triggers", BaseDirectory::Resource)` で常に統一的に解決する。エージェント開発者の app crate 側には dev/shipped の分岐コードが要らない。

Runtime は V8 の thread affinity を守るため、専用の `std::thread` に閉じ込める。tokio 側からは `std::sync::mpsc` で tick 信号を送るだけ。JS 実行は常にこの 1 スレッド上で直列に行われる。

失敗は隔離される: 1トリガーの load / instantiate / tick() が失敗しても、そのトリガーだけスキップされ、他は続行する。エラーは activity ストリームに `[error]` / `[load error]` / `[instantiate error]` プレフィックス付きで emit される。

### Task store

`tauri-plugin-store` の JSON ファイル (`<app_data>/tasks.json`)。pending なタスクリストと、トリガーごとの展開状態 (`expanded_until` + 前回の schedule 文字列) を持つ。

**トリガー state と別ファイルにする理由**: `tauri-plugin-store` は `save()` でファイル全体を書く。同居させると「トリガーが state を 1 つ書くたびに数百件のタスク配列も書き直される」write amplification が起きる。`@every 5m` 1 つで 288 件/日を生成するので、この分離は効く。

抽象レイヤ (storage domain の trait) は作らない。バックエンドが 1 つの段階では早い。ただし将来 SQLite に置き換える判断に備えて、境界に置く操作の粒度は Mastra の `SchedulesStorage` (`listDue` / `updateNextFire` / `recordTrigger` / `listTriggers`) に寄せてある。

タスクリストは SQLite に移していない (#42)。**エンドユーザーが手で編集できること**を設計に織り込んでいるためで、履歴レイヤとは性質が違う。

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

- `list_triggers() -> Vec<TriggerListItem>` — 起動時に discover したトリガーを UI 表示用に返す。`nextFireAt` は**タスクリストの投影**であり、framework が別に持っている「次回発火予定」ではない。`scheduleType` は 0.2.0 で削除された (interval 系統廃止により意味論が分岐しなくなったため)
- `pause_trigger(id: String)` / `resume_trigger(id: String)`
- `run_trigger_now(id: String)` — 今すぐ 1 回実行する (#20 を #26 Phase 1 に吸収)。実装は「即 due な ad-hoc タスクを 1 件積んで心拍を起こす」。ad-hoc タスクは展開済み境界を触らないので、手動実行が定期スケジュールを乱すことはない

タスクリスト (#26):

- `list_tasks() -> Vec<TaskListItem>` — pending なタスクを `scheduled_at` 昇順で返す
- `delete_task(id: String)` — 予定を 1 件削除する。展開済み境界があるので次の展開で復活しない

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
- `schedule` (必須) — 発火時刻の生成規則。`@` 始まりのみ。`"@hourly"` / `"@hourly :45"` / `"@every 10m"` / `"@daily 09:00"` / `"@weekly MON 09:00"` / `"@monthly 15 09:00"` / `"@at 2026-08-01T18:30"` (詳細は [schedule DSL](#schedule-dsl-26-決定事項-4--5) 節)。**0.2.0 で interval 形式 (`"5m"` / `"1h"`) は廃止された**
- `tz` (任意) — IANA TZ 名 (例: `"Asia/Tokyo"`)。省略時は OS の user local を [`iana_time_zone`] で解決 (#18)
- `requiredSecrets` (任意) — このトリガーが `chamberlain.getSecret(name)` で読む予定の secret 名一覧。Settings UI が「未設定です」の表示に使う (#13)

manifest を分離ファイルにする理由は「Rust が JS を動かさずに一覧を作れる」「Chrome/VS Code/npm と同じパターンで開発者に説明不要」「将来 marketplace の話が出た時にそのまま嵌る」など。決定の経緯は #8。

### index.ts の contract

エントリスクリプトは `tick(ctx)` 関数を export する。TypeScript は rustyscript が内部で transpile する。

```typescript
type State = { /* トリガー固有 */ };

interface Ctx {
  now: number;         // ms since epoch (Rust から渡される)
  state: State;        // 前回 tick() が返した state (未保存なら {})
  scheduledAt: number; // このタスクが実行を意図された絶対時刻 (#26 追加決定 11)
  delayMs: number;     // now - scheduledAt。遅延をどう伝えるかはトリガーが決める
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

### タスク実行の順序

due なタスク 1 件について以下の順で実行される。schedule の判定はここに一切入らない (時刻はタスクが持っており、心拍は取り出すだけ)。

1. 履歴の retention (閾値条件、1 時間間隔)。実行より先に置くのは、この心拍が積む行を消させないため
2. 猶予超過の掃除 ([猶予超過の掃除](#猶予超過の掃除-50--53) 節)
3. 分類 → 孤児 / 実行不能 / pause 中 / 遅延超過なら破棄 (詳細: [due 判定と missed-fire](#due-判定と-missed-fire-26-決定事項-8--追加決定-10--11) 節)
4. state store から namespace `id` の値を読み出し (未保存なら `{}`)
5. TS `tick({ now, state, scheduledAt, delayMs })` を呼ぶ
6. 戻り値の `notify` を fire (OS 通知 + activity emit + 履歴への追記)
7. 戻り値の `state` を store に書き込み → save
8. タスクをリストから削除 → `tasks.json` に save (心拍あたり 1 回にまとめる)

**notify が state 保存より先** である点は意図的。プロセスクラッシュ時の "at least once" を優先する: 秘書は「1回多く言う > 一言忘れる」。同じイベントを2回通知する方が、忘れて未通知になるより秘書として望ましい。

**タスク削除が最後** である点も同じ理由。実行中にクラッシュしたタスクはリストに残り、次回起動でもう一度試される。

### on-disk 形式

トリガー state (`<app_data>/triggers-state.json`):

```json
{
  "greeter": { "greetCount": 42 },
  "stretch-reminder": { "lastFire": 1721000000000 },
  "__meta__": {}
}
```

トップレベルのキーがトリガー ID (自動 namespace)。値は任意 JSON。framework は中身を関知しない。

**予約 namespace `__meta__`**: framework が内部管理する情報を置くための予約領域。0.1.x はここに `fire_times` (トリガー ID → 最終 fire 時刻) を持っていたが、**0.2.0 で廃止された**。タスクリストが唯一の真実になったため。起動時に残骸が掃除される。この ID を名乗るトリガーは discovery で reject される。

タスクリスト (`<app_data>/tasks.json`):

```json
{
  "tasks": [
    {
      "id": "greeter-morning@1767250800000",
      "origin": "schedule",
      "trigger_id": "greeter-morning",
      "scheduled_at": 1767250800000,
      "created_at": 1767225600000
    },
    {
      "id": "manual-github-issues-count-1767226000000",
      "origin": "adhoc",
      "trigger_id": "github-issues-count",
      "scheduled_at": 1767226000000,
      "created_at": 1767226000000
    }
  ],
  "expansion": {
    "greeter-morning": {
      "expanded_until": 1767398400000,
      "schedule": "@daily 06:00",
      "tz": "Asia/Tokyo"
    }
  }
}
```

`tasks` は `scheduled_at` 昇順・id 一意が不変条件。エンドユーザーが手で編集できる場所にあるため、読み込み時に並べ替えと重複排除を行う。

`expansion` がトリガーごとに持つのは **境界 1 つと前回の schedule 文字列だけ**。境界より前の時刻は二度と生成されないので、削除されたタスクの tombstone は要らない。schedule 文字列は起動時の変更検知に使う。

schedule 由来タスクの id は `{trigger_id}@{scheduled_at}` で決定的に決まる。冪等性は境界だけで担保されるが、境界の書き込みが失敗した場合の二重生成を id で弾ける。

保存先は Tauri が管理する `<app_data>/`:

- Linux: `~/.local/share/<identifier>/`
- Windows: `%APPDATA%\<identifier>\`
- macOS: `~/Library/Application Support/<identifier>/`

`<identifier>` は `tauri.conf.json` の `identifier` (現状 `dev.chamberlain.interval-notifier`)。同じ場所に実行履歴の `history.db` (SQLite) も置かれる。

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
  ts: number;       // ms since epoch (実際に起きた時刻)
  source: string;   // trigger ID (トリガーに紐付かないものは "__task__")
  kind: string;     // 種別の安定した識別子 ("notify" / "skipped" / ...)
  message: string;  // 表示用の 1 行。prefix は kind から組み立てられている
  taskId?: string;         // 元になったタスクのスナップショット
  taskOrigin?: "schedule" | "adhoc";
  scheduledAt?: number;    // 実行を意図された時刻。遅延は ts - scheduledAt
}
```

UI 側 (`chamberlainApi.onActivity`) は `@tauri-apps/api/event` の `listen` で購読する。表示は新しい順に直近 200 件 (`MAX_EVENTS`)。

**`kind` は 0.3.0 で追加された (#42)。** それまで種別は `message` のプレフィックスだけで表現されていたが、フィルタや集計のたびに文字列パースが要るので独立した値にした。`message` のプレフィックスは `kind` から組み立てられており、見た目は 0.2.0 と同じ。

プレフィックス一覧:

| プレフィックス | 意味 |
|---|---|
| `[error]` / `[load error]` / `[instantiate error]` | トリガーの tick / ロード / インスタンス化の失敗 |
| `[schedule error]` | discovery 時点の schedule / tz の構成エラー |
| `[expanded]` | 展開器がタスクを積んだ |
| `[rescheduled]` | schedule 変更を検知して再展開した |
| `[orphaned]` / `[unavailable]` | 実行対象トリガーが消えた / 実行できない状態のため予定を破棄 |
| `[paused]` | 停止中のため予定を破棄 |
| `[skipped]` | schedule 由来の予定が猶予を超えて遅れたため破棄 (1 件だけのとき) |
| `[stale]` | 猶予を超えた予定を 2 件以上まとめて破棄 (長期の不在からの復帰) |
| `[expired]` | ad-hoc の予定が猶予 (24h) を超えたため未実行のまま破棄 |
| `[manual]` / `[deleted]` | 手動実行の予約 / 予定の削除 |

捨てるにしても痕跡を残すのが観測面原則に合う。

### 実行履歴レイヤ (#42 / #26 Phase 2)

タスクリストが「未来への意図」なら、こちらは**過去の記録**である。タスクは完了すると即座に消える (決定事項 1) ので、何が起きたかは別レイヤが持つ。

すべての activity は emit と同時に `<app_data>/history.db` (SQLite) に追記される。

**これが「起動時イベントが誰にも見えない」gap を閉じる。** worker は `.setup()` 内で動き出すため、`[schedule error]` / `[expanded]` / `[rescheduled]` / `[orphaned]` は webview のリスナーが繋がる前に emit されて捨てられていた。UI は起動後に `chamberlainApi.listActivity()` で保存済みの履歴を読み、live 側と混ぜる (重複は ts + source + message で落とす)。

| 列 | 内容 |
|---|---|
| `ts` | 実際に起きた時刻 |
| `source` | trigger ID / `"__task__"` |
| `kind` | 種別。**enum ではなく TEXT** — 使われなくなった kind の行も読めなければならない |
| `message` | プレフィックスを含まない本文 |
| `task_id` / `task_origin` / `scheduled_at` | 元になったタスクのスナップショット |

タスクは実行後に消えるので参照整合性は取れない。外部キーではなく**スナップショット**として持つ。遅延をフィールドに持たず `ts - scheduled_at` で導出するのは決定事項 11 と同じ考え方。

retention は **30 日 + 20,000 行**の併用。日数だけだと `@every 5m` (288 件/日) を入れた環境で膨らみ、件数だけだと「先週何があったか」がトリガー数次第で見えなくなる。掃除は心拍ごとの閾値条件 (1 時間間隔) で走る — 時刻イベントにすると起動保証の問題が出るのは決定事項 6 と同じ理由。

履歴ビューアの UI (フィルタ / 期間指定) は #40 の方針が決まってから。

### エラーもここに流す

トリガーの load / instantiate / tick() 失敗も activity イベントとして emit される (プレフィックス付き)。UI を見るだけで「どのトリガーが壊れているか」がわかる。

開発者は Rust の stderr を追わなくても UI で異常を検出できる、というのが観測面原則の副産物。

## 決着済みの意思決定

各項目の詳細は元 Issue と commit メッセージ。ここでは 1 行サマリのみ。

### 実行環境 — deno_core (rustyscript ラッパ経由) を Rust に埋め込む (#3)

TS を開発者に書かせたい / webview JS は隠しウィンドウで不確実 / fetch/timers/ESM が要る。QuickJS 系は fetch を自作する必要があり framework の実装コストが高い。deno_core は Deno 本体の心臓部で battle-tested。

### 永続化バックエンド — tauri-plugin-store (#7) と SQLite (#42) の併存

トリガー state とタスクリストは `tauri-plugin-store` の JSON。標準プラグイン、cross-platform のパス解決込み、SQLite より始めるコストが低い。

**実行履歴だけ SQLite (`rusqlite`, bundled)。** 追記専用 + 時系列クエリ + retention という形はアクセスパターンが違い、JSON では追記のたびに `save()` がファイル全体を書き直す。

2 バックエンドの併存は妥協ではなく分離である: タスクリストは小さい可変の作業セットで**エンドユーザーが手で編集できること**を設計に織り込んでいる (`TaskStore::normalize` の存在理由)。履歴は追記ログで人が触るものではない。代償は bundled SQLite がビルドに C コンパイラを要求すること (3 プラットフォームビルドは #37 / #38)。

### API 形状 — pure functional (#7)

上記「なぜ pure functional か」参照。

### トリガーはパッケージ構造 (単一ファイル形式は不採用) (#8)

AI 駆動トリガーは prompt / MD / スキーマ等のアセットを持ち込むのが基本形。1ファイル前提は前提を間違えている。単一ファイル併存は「.ts が正か? ディレクトリが正か?」の混乱を撒く。

### 順序 — notify が state 保存より先 (#7)

プロセスクラッシュ時の "at least once" を優先。1回多く言う > 忘れる。#26 でタスク削除も同じ理由で最後になった。

### スケジュールの実体はタスクリスト (#26)

`manifest.schedule` を「発火条件」から「絶対時刻の生成規則」に読み替え、展開器が具体的な時刻のタスクに変換する。心拍は due なタスクを取り出すだけになり、`should_fire` の分岐が実行パスから消えた。動機は「秘書自身が未来に対して何も書けない」という構造上の穴を埋めること。冪等性はトリガーごとの展開済み境界 1 つで担保し、tombstone を持たない。

分散マルチインスタンス前提の Mastra は逆に `nextFireAt` 1 つを CAS で進める形を採っている。展開しないのは「誰が展開したか」の競合を避けるためであり、単一プロセスで、かつタスクを編集可能にしたい Chamberlain には当てはまらない。

### interval schedule の廃止 (#26)

展開型では interval は「wall-clock の生成規則のひとつ」に格下げされ、`@hourly` が 1 時間 interval と同義になる。グリッドに割り切れない値 (`"7m"`) は展開済み境界 1 つで完結しなくなる。破壊的変更だが 0.x なので minor bump (0.2.0) で許容した。

### トリガーの配置とパス解決 — Tauri resource dir に統一 (#19)

`tauri.conf.json` の `bundle.resources` で `../triggers/` を宣言し、runtime は `app.path().resolve("triggers", BaseDirectory::Resource)` で解決。dev/shipped の分岐が消え、エージェント開発者の main.rs から `env!("CARGO_MANIFEST_DIR")` ハックが消えた。0.x publish 前に API 表面を薄くするため `ChamberlainConfig` を撤去し、`builder()` は引数なしに単純化した。

### 既知の gotcha

新規に rustyscript / deno_core を導入する時に踏む可能性が高いので明記しておく。

**cdylib を crate-type から削る必要がある**: V8 の内部 TLS が `R_X86_64_TPOFF32` relocation を生成し、rust-lld の `-shared` 出力に置けない。desktop 用途では `["staticlib", "rlib"]` で十分。モバイル対応時は platform 別調整が必要。

**serde を `=1.0.219` にピンする必要がある**: swc_config 3.0.0 (rustyscript → deno_ast 経由) が削除された `serde::__private::de` を触っている。swc_config 3.x に patch release は無い。rustyscript が新 deno_ast を取り込んだ時点で解除可能。

## 未確定の論点

現時点で議論すべきタイミングになっていない、あるいは実装優先度が下がっている論点。順不同。

### アセット読み込み API

TS 側 (`index.ts`) から自パッケージ内のアセット (prompt.md、schema.json 等) を読み出す API。AI 駆動トリガーの実現に必須 (system prompt を .md に外出しできる、等)。想定形は `chamberlain.readAsset("system-prompt.md")` のような呼び口を TS 側に公開し、実装は Rust 側で deno_core の op として提供する形 (`chamberlain.getSecret` / `chamberlain.ai.complete` と同じレイヤ)。

### ホットリロード

`triggers/**/*.ts` や manifest.json の変更を検出して Runtime を再構築する仕組み。dev DX 向上に効くが、V8 の再初期化コストと state 継続性の扱いが論点。

### cadence / 精度 / DSL

#17 で interval schedule、#18 で wall-clock schedule と TZ セマンティクスに着地し、#26 で展開型スケジューラに転換して interval を廃止した。詳細は上記 [schedule DSL](#schedule-dsl-26-決定事項-4--5) と [展開器](#展開器-26-決定事項-2--3--6) 節を参照。

まだ議論する余地がある論点:

- `chamberlain.time.tz` op (トリガー内で「今 UTC / user local で何時か」を TZ-aware に取れる op)
- カレンダー統合トリガー (MTG N 分前通知の実応用例)
- 動的相対時刻 (`"MTG - 30m"` 等の DSL 表現) — 現状はトリガー内ロジックで書く方針
- **1 日に複数回の実行** — 時間内は分集合を持てる (`@every 10m`) のに、日内は持てない (`@daily 09:00` と `@daily 18:00` を同じトリガーに書けない) という非対称が残っている。現状は「トリガーを 2 つ書く」で回避できる。ここまで開くと cron に近づくため論点として認識するに留める (#26)

### #26 の残り Phase

- **Phase 2** — 実行履歴レイヤ。**0.3.0 で実装済み (#42)。** 詳細は[実行履歴レイヤ](#実行履歴レイヤ-42--26-phase-2)。UI (履歴ビューア) だけ #40 待ち
- **Phase 3** — チャット / Type II からの ad-hoc タスク登録。タスクの中身を「自然言語の指示 (`prompt`)」に開く話であり、`overview.md` の承認モデル (「実際にアクションを実行する場合は、必ず提案としてユーザーに提示し、確認をとってから行う」) と正面から交差する。あわせて「リマインド時刻が来たが秘書はチャット中」の分岐が必要になる (Mastra の `ifActive` / `ifIdle` = deliver / wake / persist / discard が先例)

### notify API の一般化

現状は tick() の戻り値でメッセージを渡す return-based 形式で暫定合意。将来 ops で `chamberlain.notify(msg)` を呼べる副作用形式も許容するか、pure に統一するかは開き。

### AI 動的トリガー

「AI が日次で "今日見張るもの" を生成し、tick() がそれを見て動く」パターンを first-class にするか、開発者定義トリガー内で表現するかは未決。framework の抽象度を大きく左右する論点。

### pause 状態の永続化

現状は毎起動リセット。「停止したままにしておきたい」というユーザーの意図を respect するかは UX 判断。
