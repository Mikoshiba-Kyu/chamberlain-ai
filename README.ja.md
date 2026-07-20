# Chamberlain

[![chamberlain-core](https://img.shields.io/crates/v/chamberlain-core?style=flat-square&logo=rust&label=chamberlain-core)](https://crates.io/crates/chamberlain-core)
[![create-chamberlain](https://img.shields.io/npm/v/create-chamberlain?style=flat-square&logo=npm&label=create-chamberlain)](https://www.npmjs.com/package/create-chamberlain)
[![docs.rs](https://img.shields.io/docsrs/chamberlain-core?style=flat-square&logo=docsdotrs&label=docs.rs)](https://docs.rs/chamberlain-core)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue?style=flat-square)](#ライセンス)
![status](https://img.shields.io/badge/status-0.x%20unstable-orange?style=flat-square)
[![platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey?style=flat-square)](#quick-start)

[![Rust](https://img.shields.io/badge/Rust-000000?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Tauri](https://img.shields.io/badge/Tauri-FFC131?style=flat-square&logo=tauri&logoColor=black)](https://tauri.app/)
[![TypeScript](https://img.shields.io/badge/TypeScript-3178C6?style=flat-square&logo=typescript&logoColor=white)](https://www.typescriptlang.org/)
[![React](https://img.shields.io/badge/React-61DAFB?style=flat-square&logo=react&logoColor=black)](https://react.dev/)

[English](README.md) | **日本語**

Chamberlain は、秘書のように振る舞う自律型デスクトップアプリケーションを、開発者が最小限のコードで構築するための Tauri ベースのフレームワークです。

Rust コアが常駐して各トリガーを定期的に実行し、開発者は JavaScript / TypeScript でスケジュール・判定ロジック・通知を記述します。エージェント開発者は `npm create chamberlain@latest` で雛形から始められます。

> [!WARNING]
> 現時点では開発初期段階のため、minor バージョンでも破壊的変更が入ります。

## Quick start

```bash
npm create chamberlain@latest my-secretary
```

詳しい導入手順は [`docs/getting-started.md`](docs/getting-started.md) をご覧ください。

## ドキュメント

- [`docs/overview.md`](docs/overview.md) — 何を作るためのフレームワークか (vision, what/why)
- [`docs/architecture.md`](docs/architecture.md) — 今の骨格 (責務分割・契約・意思決定)
- [`docs/getting-started.md`](docs/getting-started.md) — セットアップと最初の動作確認 (エージェント開発者向け)

## コントリビュート

フレームワーク本体の開発サイクル、DevContainer、レポ構造の同期ルールなどは [`CONTRIBUTING.md`](CONTRIBUTING.md) にまとめてあります。進行中の設計論点・タスクは GitHub Issues (`gh issue list`) で追跡しています。

## ライセンス

`MIT OR Apache-2.0` の dual license です。利用者はどちらか好きな方を選べます (Rust エコシステムの慣行に合わせています)。

- [LICENSE-MIT](LICENSE-MIT)
- [LICENSE-APACHE](LICENSE-APACHE)
