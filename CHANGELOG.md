# Changelog

このファイルは [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) 形式で記述します。バージョニングの方針は [`docs/versioning.md`](docs/versioning.md) を参照してください。

`chamberlain-core` (crates.io) と `create-chamberlain` (npm) は常に同じバージョンでリリースされます。

## [Unreleased]

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
