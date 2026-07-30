# chamberlain-core

Chamberlain フレームワークの Rust コア。常駐する秘書エージェント向けデスクトップアプリを最小限のコードで構築するためのランタイム本体。

このクレートは **Tauri アプリの内側で動く** フレームワークコンポーネントです。単体で `main()` を持つバイナリではありません。エージェント開発者は `create-chamberlain` (npm) でスキャフォールドしたプロジェクトの中でこれを依存として利用します。

## 状態

**0.x = unstable.** minor バージョンで破壊的変更を入れる方針です (Rust の慣行に沿う)。API を安定させる契約は 1.0 手前で別途扱います。

## 何をやるか

- **心拍 tick** — `tokio::time::sleep` を回す常駐タスク。通常 `1m`、`CHAMBERLAIN_DEV=1` 時 `10s`
- **schedule の展開** — `@daily 09:00` / `@hourly :45` / `@every 10m` 等の DSL を解釈し、絶対時刻を持つタスクリストに展開する。心拍は due なタスクを取り出して実行するだけ
- **タスクリスト** — 「秘書がこれから何をするつもりか」の単一の実体。UI から閲覧・削除でき、手動実行もここに積まれる
- **JS ランタイムホスト** — rustyscript (deno_core 経由) 上で TypeScript トリガーを実行
- **State store** — `tauri-plugin-store` の JSON ファイルにトリガー毎の state とタスクリストを永続化
- **OS 通知** — `tauri-plugin-notification` 経由 (Windows は AUMID 自己登録込み)
- **トレイ + チャット UI** — 秘書 persona (Type II AI) を Anthropic Messages API で駆動

詳細は [リポジトリの docs/architecture.md](https://github.com/Mikoshiba-Kyu/chamberlain-ai/blob/main/docs/architecture.md) 参照。

## 最小使い方

エージェント開発者は自分の Tauri アプリの `src-tauri/src/lib.rs` でこう書きます:

```rust
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    chamberlain_core::builder()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

トリガーは `tauri.conf.json` の `bundle.resources` で宣言し、runtime は resource dir 経由で解決します:

```json
{
  "bundle": {
    "resources": { "../triggers/": "triggers/" }
  }
}
```

具体的には [`create-chamberlain`](https://www.npmjs.com/package/create-chamberlain) で生成されるプロジェクトが正解の形をそのまま持っています。

## ライセンス

MIT OR Apache-2.0 dual license。詳細は [`LICENSE-MIT`](./LICENSE-MIT), [`LICENSE-APACHE`](./LICENSE-APACHE)。
