# Chamberlain を使い始める

このドキュメントは、Chamberlain を使ってエージェントアプリを作りたい方 (エージェント開発者) 向けの導入手順です。フレームワーク本体へのコントリビュートを目的とする場合は [`CONTRIBUTING.md`](../CONTRIBUTING.md) を参照してください。

## 前提

以下がローカルに揃っている必要があります。

- Node 20+ / pnpm 9+
- Rust stable
- Tauri の prerequisites — OS ごとに異なります。[Tauri 公式ガイド](https://v2.tauri.app/start/prerequisites/) をご確認ください

## プロジェクトを作成する

```bash
npm create chamberlain@latest my-secretary
cd my-secretary
pnpm install
pnpm tauri dev
```

初回はビルドに数分かかります。ウィンドウが起動し、サンプルトリガーが動きはじめれば成功です。生成されたプロジェクトの構成やビルド方法は、生成先の `README.md` をご覧ください。

## API キー・シークレットの投入

秘書チャット (`ChatPanel`) や `chamberlain.ai.complete` を使うトリガーは Anthropic の API キーを必要とします。設定方法は 2 通りあります。

### 通常経路: Settings UI からキーリングに保存する

アプリを起動 → 「設定」タブ → `anthropic_api_key` に貼り付け → 保存。値は OS の credential manager (Windows Credential Manager / macOS Keychain / Linux Secret Service) に保存されます。Chamberlain 上には残りません。

### 開発用の逃げ道: `.env` (env-var fallback)

`chamberlain.getSecret(name)` は keyring を参照する前に `CHAMBERLAIN_SECRET_<UPPERCASE_NAME>` を先に見にいきます。開発中は `.env` に書いておくと `builder()` が起動時に dotenvy で読み込むため、Settings UI からの保存を省略できます。

生成先ディレクトリの直下に `.env` を置いてください。テンプレートに `.env.example` が同梱されているので、コピーして値を埋めるのが楽です。

例:

```
# my-secretary/.env
CHAMBERLAIN_SECRET_ANTHROPIC_API_KEY=sk-ant-...
CHAMBERLAIN_SECRET_GITHUB_TOKEN=ghp_...
```

env-var が設定されていれば keyring より優先されます。設定されていなければ従来通り keyring を使います。`.env` / `.env.local` / `.env.*.local` はテンプレートの `.gitignore` に含まれています。

## トリガーから使える API

トリガー (`triggers/*/index.ts`) は ambient global `chamberlain.*` を通じて Rust 側と対話します。

```ts
chamberlain.getSecret(name: string): Promise<string | null>
chamberlain.ai.complete(opts: {
  prompt: string;
  system?: string;
  model?: string;    // 省略時は claude-sonnet-5
}): Promise<string>
```

`ctx` は tick に渡される純粋データ (`{ now, state }`) で、副作用のある API は `chamberlain.*` 側に分けてあります。詳細な設計意図と契約は [`docs/architecture.md`](./architecture.md#ambient-global-chamberlain) を参照してください。

### 読む secret は manifest に宣言する

`getSecret(name)` は **`manifest.json` の `requiredSecrets` に書いた名前しか返しません** (0.3.0 / #56)。宣言していない名前を渡すと `null` が返り、活動ログに `[denied]` が出ます。「keyring に入れたのに null が返る」ときはまず宣言を確認してください。

```json
{
  "requiredSecrets": ["github_token"]
}
```

`anthropic_api_key` だけは宣言しても返りません (framework が持つキーです)。トリガーから AI を使う場合は `chamberlain.ai.complete` を呼んでください。
