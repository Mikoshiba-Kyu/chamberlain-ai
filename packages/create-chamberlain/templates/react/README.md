# Chamberlain App

`create-chamberlain` で生成された、常駐する秘書エージェントアプリ。フレームワーク本体は [`chamberlain-core`](https://crates.io/crates/chamberlain-core) が提供する。

## セットアップ

```
pnpm install
```

必要な secret を設定する (2 経路のどちらか):

1. **Settings UI 経由** — `pnpm tauri dev` で起動し、「設定」タブから保存 (OS の credential manager に入る)
2. **dev 用の逃げ道** — `.env.example` を `.env` にコピーして値を入れる

## 開発

```
pnpm tauri dev
```

## テスト

```
pnpm test
```

秘書 UI の表示用純関数に vitest のテストが付いています。時刻表示はローカル TZ に依存するため、テストだけ `vite.config.ts` の `test.env` で TZ を固定しています。

## ビルド (配布用)

```
pnpm tauri build
```

手元の OS 向けのバンドルが `src-tauri/target/release/bundle/` にできます。

### 3 プラットフォーム分をまとめて作る

`.github/workflows/build.yml` が同梱されています。GitHub の Actions タブから **Build** を選んで Run workflow を押すと、Windows / macOS (Apple Silicon / Intel) / Linux のバンドルが artifact として手に入ります。

- **起点は手動 (`workflow_dispatch`) だけです。** タグ運用やリリースの形はあなたが決めるもので、Chamberlain は規定しません
- **久しぶりの実行は 30 分前後かかります。** JS ランタイム (deno_core) のビルドが重く、GitHub の cache は 7 日で evict されるためです。連続して回せば数分で終わります
- **証明書が無くてもビルドは通ります。** コード署名は secrets が設定されている場合だけ有効になります。設定するものはワークフロー内のコメントに書いてあります

ローカルでの Windows 向けクロスコンパイル (`cargo-xwin`) は、可視ウィンドウを持つ Tauri アプリでは動きません。Windows 向けバンドルが要るときは上のワークフローを使ってください。

## 何を編集するか

- `triggers/<id>/` — トリガーの追加・変更。各トリガーは `manifest.json` と `index.ts` のペア
- `src/app/` — 秘書 UI (React)。トリガー一覧・アクティビティ・チャット・設定パネル
- `src-tauri/tauri.conf.json` — アプリ ID / アイコン / bundle 設定
- `src-tauri/src/lib.rs` — Chamberlain 本体を起動する薄いエントリ (通常触らなくていい)

## 詳しく知る

- [Chamberlain overview](https://github.com/Mikoshiba-Kyu/chamberlain-ai/blob/main/docs/overview.md)
- [Chamberlain architecture](https://github.com/Mikoshiba-Kyu/chamberlain-ai/blob/main/docs/architecture.md)
