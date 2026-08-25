# Changelog

このファイルは [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) 形式で記述します。バージョニングの方針は [`docs/versioning.md`](docs/versioning.md) を参照してください。

`chamberlain-core` (crates.io) と `create-chamberlain` (npm) は常に同じバージョンでリリースされます。

## [Unreleased]

### Changed

- **BREAKING**: `manifest.json` の `requiredSecrets` が実行時の権限になった (#56 / #55)。`chamberlain.getSecret(name)` は宣言した名前しか返さず、宣言外は `null` + 活動ログに `[denied]` が残る。焼き込みか実行時登録かで区別しない。実行時登録 (#55) を開く前に「宣言と実際の権限の乖離」を潰しておくための変更
- **BREAKING**: `anthropic_api_key` はトリガーから読めなくなった。`requiredSecrets` に書いても `null` が返る。framework が持つキーであり、トリガーが AI を使うなら `chamberlain.ai.complete` を経由する
- **BREAKING**: `chamberlain.http.fetch` の宛先を `manifest.json` の `allowedHosts` で宣言するようになった (#57 / #55)。**宣言しなければ一切ネットワークに出られない。** 宣言外のホストへの fetch は例外になり `[denied]` が残る。`"api.github.com"` (完全一致) と `"*.example.com"` (サブドメインのみ) を書ける
- **BREAKING**: `http.fetch` は https のみになった。平文が通るのはループバック (`localhost` と `127.0.0.0/8`) だけ
- `http.fetch` のリダイレクト追跡を reqwest から core に移した。**ホップごとに `allowedHosts` と照合する** (宣言済みホストが 302 を返すだけで制限が抜けるのを防ぐ)。上限は 5 ホップで、超えたら 3xx をそのまま返す
- **BREAKING**: `http.fetch` の 30s タイムアウトは**リダイレクトを含む全体**の上限になった (従来は 1 リクエスト単位)。自前追跡では放置すると 30s × ホップ数まで伸び、その間ほかのトリガーの tick が止まる
- 別ホストへリダイレクトするとき `Authorization` / `Cookie` / `Proxy-Authorization` 等を落とす。reqwest の redirect policy を切った代わりに core 側で行う。両方のホストが宣言済みでも、片方向けの認証情報をもう片方に渡す理由は無い
- **BREAKING**: `manifest.json` の `entry` はトリガーのフォルダ内を指していなければならなくなった (#58)。`"../x.ts"` のようにフォルダの外を指すものは構成エラーとして実行対象から外れる。V8 に読ませるファイルを決める値なので、焼き込みにも同じ検証をかける
- **BREAKING**: トリガーの JS 実行 1 回に 110 秒の上限がついた (#59 / #55)。超えたトリガーは中断され、活動ログに `[error] 実行が 110 秒の上限を超えたため中断しました` が残る。心拍は次のトリガーへ進む。無限ループを書いたトリガー 1 つが全トリガーを止めるのを防ぐための変更で、実行時登録 (#58) で他人や AI が書いたトリガーを受け入れる以上、悪意が無くても事故として起きる。上限は `ai.complete` の 90 秒より長く、schedule 猶予 (prod 120 秒) より短い
- `[denied]` と `[ai]` は 1 実行につき種類ごとに 1 行にまとめ、回数を添える (`... (×1000)`)。種類数も 32 で頭打ちにし、本文は 200 文字で切る。ループ内の呼び出しが履歴を埋め尽くして本物のイベントを押し流すのを防ぐ
- **BREAKING**: `chamberlain.ai.complete` の応答が上限に達して切れた場合、**切れたテキストを返さず例外を投げる**ようになった (#68)。従来は切り捨てが成功として扱われ、途中で切れた文章がそのまま通知本文に載っていた。「切れた要約を通知した」より「要約に失敗した」の方が秘書として正しい。長い応答が要るトリガーは `maxTokens` を上げる
- `[ai]` の行に**消費したトークン数**が載るようになった (#71)。`ai.complete model=… (×2) tokens in=3120 out=880` の形で、モデル別・トリガー別にどれだけ課金されているかが活動ログだけで読める。金額には換算しない (モデル別単価を core が抱えると #27 のカタログ保守を背負うため)。トークン数は本文ではなく行の数値フィールドに持ち、1 行に畳むときに加算する — 本文に入れると呼び出しごとに文字列が割れて畳みが効かなくなる。**prompt は引き続き残さない** (数値なので #57 の線を越えない)
- `ai::complete` / `ai::complete_with_tools` (Rust) の戻り値が `ai::Response { content, stop, usage }` になった。#68 で 2 要素になったタプルに `usage` が加わって 3 要素になり、位置で読む形では誤りに気づけないため
- **心拍がトリガーの JS を実行しなくなった (#81)。** 心拍は「誰のどの予定が今やるべきものか」まで判断して executor スレッドへ渡し、実行の完了を待たずに次へ進む。1 本のトリガーが長引いても鼓動そのものは止まらない。実行が別スレッドに出たことで「配ったがまだ終わっていない」という状態が生まれるが、これは永続化しないので、途中でプロセスが落ちたタスクはリストに残って次回起動で再試行される (at-least-once はそのまま)
- 順番待ちのうちに猶予を超えた予定は、走らせずに `[skipped]` / `[expired]` (由来による) として破棄する (#81)。実行が心拍と切り離された以上、「配った時点では間に合っていたが番が回ってきた頃には手遅れ」が起こりうるので、猶予の判定を実行の直前にもう一度当てる。#26 決定事項 8 (遅れた予定は実行しない) が引き渡しの向こう側でも効くようにするための変更
- タスクリストの永続化が「心拍 1 回につき 1 回」から「心拍 1 回 + 実行 1 件につき 1 回」になった (#81)。実行の完了時刻が心拍と揃わなくなったので、タスクを消すのは実行し終えた側の責任になる
- 秘書 (Type II) がトリガーを生成するときの上限を 6144 トークンにした (#68 / #61)。仕様書 (#60) は型の自己宣言を勧めていて生成物が長くなる方向にあり、既定の 4096 では `index.ts` の途中で切れていた。切れた場合の文言も分けた — 原因が分からないまま「もう少し具体的に」と促すと、より長い依頼になって同じ場所で切れる

### Added

- **1 回の実行に 110 秒では足りないトリガーを書けるようになった (#81)。** `manifest.json` に `maxRuntimeSec` (111〜1800) を書くと、そのトリガーはその時間まで走れる。フィードを集めて何十本も要約して組み立てる、のように AI 呼び出しを何度も積む仕事が載るようになった
  - **書くと 2 つのことが同時に変わる。** (1) 専用の枠で実行される — 宣言していないトリガーの邪魔をしない代わりに、宣言したトリガーどうしは順番待ちになる。(2) **落ちてもやり直されない** (at-most-once)。AI 呼び出しを何十回も積む仕事はエンドユーザーの実費なので (#71 / #72)、クラッシュ後に黙って丸ごと焼き直すと請求が倍になる。言い忘れより高くつくため、長い仕事だけ配送保証を倒す
  - 範囲外の値は**丸めずに構成エラー**にする。黙って丸めると宣言と実際の上限がずれたまま誰も気づけない (#68 の `maxTokens` と同じ判断)
  - 宣言は同意画面 (#58) と `TriggerListItem` / `TriggerCandidate` に出る。`requiredSecrets` / `allowedHosts` と同じく core が実際に強制している宣言なので、「入れる前に見せる」対象として同格に扱う
- **トリガーが同時に複数動くようになった (#81)。** 実行の枠は標準 2 + 長い仕事用 1 で、それぞれ独立した V8 isolate を持つ。同じトリガーが二重に動くことはない (前回が終わるまで次は始まらない) — 並行に走らせると両方が state を読んでから書くので後の書き込みが前の結果を消す
  - 帰結として、**同じトリガーの予定が同一心拍に 2 件溜まっていた場合、2 件目は次の心拍に回る** (prod で最大 1 分)。従来は 1 本のスレッドで続けて実行されていた。待ちきれない遅れは今までどおり猶予超過として掃除される
  - **モジュールのトップレベル (`tick` の外) は枠ごとに 1 回ずつ実行される。** 従来は isolate が 1 つだったので 1 回だった。トップレベルに置いた変数は実行のたびに引き継がれるとは限らないので、持ち越したい値は `state` に入れること (仕様書 §6 に明記)
  - isolate を分けても実行文脈 (#56 / #57) と番犬 (#59) の前提は変わらない — どちらも「1 つの isolate の中では直列」の上に成り立っており、枠ごとに isolate が閉じているため
- **トリガーを実行時に登録・解除できるようになった (#58 / #55)。** エンドユーザーが秘書に仕事を増やせる。焼き込み (resource dir) に加えて `<app_data>/triggers/` が 2 つ目の走査元になり、**discovery から先は出どころで区別しない** (権限の宣言も同じように強制される)
  - 受け取り口は「UI からフォルダを選ぶ」1 本。`manifest.json` があるフォルダを選ぶと、そのトリガーの `requiredSecrets` / `allowedHosts` を見せて確認をとってからコピーする。#56 / #57 で宣言が強制力を持っているので、同意画面に出る文字列と実際の制限は一致する
  - **登録の反映は再起動から。** 走っているプロセスにトリガーが増えないので、「`discover_triggers()` は起動時 1 回で確定する」前提 (#26) が保たれる
  - **解除は即時。** 積まれていた予定とトリガー state も同時に消える。同梱トリガーは外せない (停止はできる)
  - id が衝突したら焼き込みが勝つ。アプリに同梱された「そのアプリらしさ」を後から乗っ取らせない
- **トリガーの書き方をまとめた仕様書を同梱するようになった (#60 / #55)。** 実行時登録で開いた供給元 (a)「エンドユーザーが自分で書く」は実際には「外部の生成 AI に書かせる」になるので、その AI に渡せる自己完結した 1 ファイルが要る
  - 実体は `packages/core/src/trigger-spec.md` 1 つ。**core のバイナリに焼き込む** (`include_str!`) ので、仕様書のバージョンは常に動いている実装と一致する
  - 配り方は **skill** (`chamberlain-triggers/SKILL.md`)。本文をコピーさせる形は採らない — 貼り付け経路だと AI が返した 2 ファイルをエンドユーザーが手で保存することになり、TS が書けない人向けの経路としてそこだけ人力で残る。skill として載れば AI がフォルダごと書き出せる
  - エンドユーザーには「トリガー」画面の **[書き方を skill として保存…]** から届く (invoke command `save_trigger_skill`)。エージェント開発者には scaffold されたプロジェクトの `.claude/skills/chamberlain-triggers/SKILL.md` として届く (同期は `scripts/sync-template.mjs`)
  - 仕様書の内容はテストで実装に結んである。載せた manifest は `validate_manifest` を通り、「使える記法」の表は `parse_schedule` を通り、「使えない記法」(cron 式 / `@every 7m` 等) は通らない。「素の `fetch` は無い」「相対 import は解決できない」は実物の V8 isolate を立てて確認する
- invoke command に `pick_trigger_folder` / `register_trigger` / `unregister_trigger` / `restart_app` を追加 (#58)。フォルダ選択のダイアログは core が Rust 側で開くので、エージェント開発者側に capability の宣言もフロントの依存も増えない
- `TriggerListItem` に `source` (`"bundled"` | `"registered"`) を追加。UI がバッジで出し分け、登録したものにだけ「解除」を出す
- 活動ログの kind に `registered` / `unregistered` を追加。「誰かが後から仕事を増やした」も履歴に残る
- 活動ログの kind に `denied` を追加。manifest の宣言の外に出ようとして止められたことを表す
- 活動ログの kind に `ai_call` (`[ai]`) を追加。トリガーの `chamberlain.ai.complete` 呼び出しを記録する — framework の API キーの持ち出しにあたるため。model と回数とトークン数のみで、**prompt は残さない**
- **秘書自身 (Type II) の AI 消費も活動ログに載るようになった (#71)。** 秘書チャット (`chat.send`) とトリガーの下書き生成 (`draft.generate`) を `source` が `__meta__` の `[ai]` 行として残す。履歴を毎回全部送るチャットこそ消費が積み上がるので、Type I にしか記録が無ければ一番大きい部分が見えない
- 応答の `usage` から `cache_creation_input_tokens` / `cache_read_input_tokens` も読むようになった (#71)。**0 でないときだけ**行に出る。プロンプトキャッシュを入れたときに効いているかを見る手段がこれしか無い (最小キャッシュ長に満たない prefix はエラーにならず黙って無視される)
- 活動ログの kind に `config_error` (`[config error]`) を追加。`allowedHosts` の書式不正など、manifest が壊れているトリガーは実行対象から外れる。**`schedule` / `tz` の失敗もこの kind に統合された** (`schedule_error` は保存済みの行を読むためだけに残る) — UI から見て意味があるのは「manifest が壊れていて動かせない」という 1 つの概念で、どの項目かは message が持つ
- `TriggerListItem` に `requiredSecrets` / `allowedHosts` を追加。トリガー一覧が「何を読み、どこへ出るのか」を表示する。実行時登録 (#55) の同意画面はこれと同じものを入れる前に見せる
- `chamberlain.ai.complete` に `maxTokens` を追加 (#68)。既定は 4096、上限は 6144。切り捨てが例外になる以上、長い応答が要るトリガーには逃げ道が要る。範囲外の値は丸めずに例外 (黙って丸めると「上げたのに切れる」が起きる)。上限は 90 秒の timeout から導いてあり、待てない長さは案内しない — 指定できても届かないと、切り捨てではなく `operation timed out` で落ちて原因が読めなくなる
- 応答が切れた呼び出しは `[ai]` に別の行として残る (`ai.complete model=… truncated at maxTokens`)。例外は `catch` で握り潰せるので、切り捨てを黙って通す経路をひとつも残さない

### Migration

`getSecret` を呼んでいるトリガーは、読む名前を `manifest.json` の `requiredSecrets` に列挙してください。宣言漏れは例外ではなく `null` として現れるため、**トリガー側は「未設定」と同じ経路に落ちます**。活動ログの `[denied]` が実質的な検出手段です。

`http.fetch` を呼んでいるトリガーは、出る先を `allowedHosts` に列挙してください。こちらは例外になるので気付けます。http で外部を叩いていた場合は https に変える必要があります。

`ai.complete` に長い応答を書かせているトリガーは、`maxTokens` を上げるか prompt で長さを抑えてください。従来は切れたテキストがそのまま返っていましたが、これからは例外になります (`try` / `catch` で包んでいれば、通知が「切れた本文」から「失敗の報告」に変わります)。

```json
{
  "requiredSecrets": ["github_token"],
  "allowedHosts": ["api.github.com"]
}
```

## [0.2.0] - 2026-07-30

### Added

- **タスクリストがスケジュールの唯一の実体になった** (#26 Phase 1)。`manifest.schedule` は展開器が絶対時刻のタスクに変換し、心拍は「due なタスクを取り出して実行して消す」だけを行う。秘書が未来に対して書ける場所ができた
- schedule DSL に時間内の解像度を追加 — `@hourly :45` (毎時 :45) と `@every 10m` (毎時 N 等分)。`@every` の N は 5 / 10 / 15 / 20 / 30 のみ
- 予定リスト UI (`list_tasks` / `delete_task`)。「秘書がこれから何をするつもりか」が 1 画面で見え、削除できる。削除した予定は展開でよみがえらない
- 手動実行 (`run_trigger_now`) — トリガーを今すぐ 1 回実行する (#20 を #26 Phase 1 に吸収)
- 起動時に `manifest.schedule` / `tz` の変更を検知して該当トリガーを再展開 (#26 決定事項 7)
- 起動時に、消えたトリガーを指す予定を破棄する孤児掃除 (#26 追加決定 9)
- `tick(ctx)` に `scheduledAt` / `delayMs` を追加。遅延をどう伝えるかは framework ではなくトリガーが決める (#26 追加決定 11)

### Changed

- **BREAKING**: interval schedule (`"5m"` / `"1h"` / `"10s"`) を廃止。`manifest.schedule` は `@` 始まりのみになった (#26 決定事項 4)。旧形式は移行先を名指しするエラーで reject される
- **BREAKING**: `TriggerListItem.scheduleType` を削除 (interval 系統廃止により `nextFireAt` の意味論が分岐しなくなった)。代わりに生の `schedule` 文字列を返す
- **BREAKING**: `TriggerListItem.nextFireAt` はタスクリストの投影になった。予定を削除すれば null になる
- **BREAKING**: `CHAMBERLAIN_DEV=1` は心拍のみ緩和する。schedule 下限の緩和 (秒スケール) は廃止 — 分グリッドに秒は載らない。dev の反復手段は手動実行ボタンに移った
- `__meta__.fire_times` を廃止。タスクリストが唯一の真実になった。起動時に残骸を掃除する
- pause 中も展開は続き、予定はリストに残る。due 取り出し時に破棄され `[paused]` として記録される (#26 追加決定 10)
- missed-fire の猶予を出自別にした — schedule 由来は心拍 2 回分、ad-hoc は 24h (#26 決定事項 8)

### Migration

`triggers/*/manifest.json` の `"schedule"` を書き換える必要があります。

| 旧 | 新 |
|---|---|
| `"1h"` | `"@hourly"` |
| `"5m"` / `"10m"` / `"30m"` | `"@every 5m"` / `"@every 10m"` / `"@every 30m"` |
| `"6h"` | `"@daily HH:MM"` を複数トリガーに分けるか、`"@hourly"` + トリガー側で間引く |
| `"10s"` (dev) | 表現不可。UI の「今すぐ実行」ボタンを使う |

## [0.1.1] - 2026-07-25

### Fixed

- 破損した `__meta__` で worker が silent death する経路を除去 (#21)
- Anthropic 呼び出しに 90s timeout を追加、`reqwest` Client を共有化 (#21)
- `http.fetch` のレスポンス body を 10 MiB 上限でチャンク読みに (#21)
- chat 履歴のパース失敗を silent overwrite せず退避キーに保存 (#21)
- `chat_send` で AI エラー時に user メッセージが消える不具合を修正 (#21)
- トリガー dedup を id 昇順から先勝ちに変更し、fs 順序依存を排除 (#21)
- `fire_times` の per-fire read-modify-write を tick 末 1 回の batch write に統合 (#21)
- 通知 permission 要求を setup 時に main thread から 1 度だけ実行 (#21)
- schedule の u64/i64 変換を `try_from` に置き換え、pre-epoch / overflow を防御 (#21)
- secret 名から env var 名への正規化を `[A-Z0-9]` 以外すべて `_` に丸める形に (#21)

### Changed

- `TriggerListItem` に `scheduleType` を追加し、`nextFireAt` の意味論を明確化 (#21)

## [0.1.0] - 2026-07-20

初回リリース。

### Added

- **常駐エージェントのコア** — heartbeat tick、トリガー discovery、永続 state 層 (#2, #7, #8)
- **JS 実行環境** — rustyscript (deno_core) 上でトリガーの `check` / `notify` を実行 (#3)
- **観測面としての UI** — activity ログ、トリガー一覧 (#6)
- **OS 通知とトレイ** — `tauri-plugin-notification` / tray-icon
- **secret store** — OS credential manager 経由の保存と `ctx` API、env-var fallback (#13)
- **秘書自身の AI (Type II)** — チャット UI と、Type I / Type II 共通の `chamberlain.ai.complete` (#14)
- **`chamberlain.http.fetch`** — トリガーからの HTTP 呼び出し
- **トリガー毎の schedule 宣言** — interval + wall-clock DSL、TZ セマンティクス (#17, #18)
- **shipped-app でのトリガー配置** — Tauri resource dir に統一 (#19)
- **`create-chamberlain`** — React テンプレを同梱する scaffold CLI (#9)
- **モノレポ構成** — `packages/core` + `packages/create-chamberlain` + `examples/react` (#10, #11)
- **サンプルトリガー** — `github-issues-count` (Type I の初実装, #15)

[Unreleased]: https://github.com/Mikoshiba-Kyu/chamberlain-ai/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Mikoshiba-Kyu/chamberlain-ai/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/Mikoshiba-Kyu/chamberlain-ai/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Mikoshiba-Kyu/chamberlain-ai/releases/tag/v0.1.0
