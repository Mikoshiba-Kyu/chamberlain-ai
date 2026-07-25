# Changelog

このファイルは [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) 形式で記述します。バージョニングの方針は [`docs/versioning.md`](docs/versioning.md) を参照してください。

`chamberlain-core` (crates.io) と `create-chamberlain` (npm) は常に同じバージョンでリリースされます。

## [Unreleased]

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

[Unreleased]: https://github.com/Mikoshiba-Kyu/chamberlain-ai/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/Mikoshiba-Kyu/chamberlain-ai/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Mikoshiba-Kyu/chamberlain-ai/releases/tag/v0.1.0
