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
- `[denied]` と `[ai]` は 1 実行につき種類ごとに 1 行にまとめ、回数を添える (`... (×1000)`)。種類数も 32 で頭打ちにし、本文は 200 文字で切る。ループ内の呼び出しが履歴を埋め尽くして本物のイベントを押し流すのを防ぐ

### Added

- 活動ログの kind に `denied` を追加。manifest の宣言の外に出ようとして止められたことを表す
- 活動ログの kind に `ai_call` (`[ai]`) を追加。トリガーの `chamberlain.ai.complete` 呼び出しを記録する — framework の API キーの持ち出しにあたるため。model と回数のみで、**prompt は残さない**
- 活動ログの kind に `config_error` (`[config error]`) を追加。`allowedHosts` の書式不正など、manifest が壊れているトリガーは実行対象から外れる。**`schedule` / `tz` の失敗もこの kind に統合された** (`schedule_error` は保存済みの行を読むためだけに残る) — UI から見て意味があるのは「manifest が壊れていて動かせない」という 1 つの概念で、どの項目かは message が持つ
- `TriggerListItem` に `requiredSecrets` / `allowedHosts` を追加。トリガー一覧が「何を読み、どこへ出るのか」を表示する。実行時登録 (#55) の同意画面はこれと同じものを入れる前に見せる

### Migration

`getSecret` を呼んでいるトリガーは、読む名前を `manifest.json` の `requiredSecrets` に列挙してください。宣言漏れは例外ではなく `null` として現れるため、**トリガー側は「未設定」と同じ経路に落ちます**。活動ログの `[denied]` が実質的な検出手段です。

`http.fetch` を呼んでいるトリガーは、出る先を `allowedHosts` に列挙してください。こちらは例外になるので気付けます。http で外部を叩いていた場合は https に変える必要があります。

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
