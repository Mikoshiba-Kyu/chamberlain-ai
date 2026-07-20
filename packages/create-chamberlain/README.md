# create-chamberlain

[Chamberlain](https://github.com/Mikoshiba-Kyu/chamberlain-ai) 秘書エージェントアプリの scaffold CLI。生成物はそのままビルド可能な Tauri プロジェクトで、フレームワーク本体 ([`chamberlain-core`](https://crates.io/crates/chamberlain-core)) を crates.io から依存として受け取ります。

## 状態

**0.x = unstable.** minor バージョンで破壊的変更を入れます (npm / crates.io の 0.x 慣行に沿う)。

## 使い方

```
npm create chamberlain@latest my-secretary
```

または pnpm / yarn:

```
pnpm create chamberlain my-secretary
yarn create chamberlain my-secretary
```

生成された後は:

```
cd my-secretary
pnpm install
pnpm tauri dev
```

## オプション

```
create-chamberlain <target-dir> [project-name] [--template <name>]
```

- `target-dir` (必須) — 生成先ディレクトリ。既存だとエラー
- `project-name` (任意) — Cargo/npm パッケージ名や Tauri productName に使う識別子。省略時は `target-dir` の basename。`/^[a-z][a-z0-9-]*$/` にマッチする必要あり
- `--template <name>` / `-t <name>` — 使うテンプレート。デフォルト `react` (現状 `react` のみ。将来 `vue` 等が追加予定)

## 生成物の中身

- Tauri アプリ (Rust + フロントエンド)、コアは `chamberlain-core` (crates.io) を version 依存
- サンプルトリガー数個 (`triggers/greeter-*`, `triggers/github-issues-count`)
- `.env.example` — dev 用の secret 注入テンプレ

## Chamberlain 自体を知る

- [Overview](https://github.com/Mikoshiba-Kyu/chamberlain-ai/blob/main/docs/overview.md)
- [Architecture](https://github.com/Mikoshiba-Kyu/chamberlain-ai/blob/main/docs/architecture.md)

## ライセンス

MIT OR Apache-2.0 dual license。詳細は [`LICENSE-MIT`](./LICENSE-MIT), [`LICENSE-APACHE`](./LICENSE-APACHE)。
