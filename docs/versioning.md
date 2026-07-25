# バージョン・リリース・ブランチ戦略

Chamberlain フレームワーク本体のリリース運用ルールです。エージェント開発者が自分のアプリをどうバージョニングするかは、この文書の対象外です (それは各アプリの裁量)。

## 単一バージョン (lockstep)

Chamberlain は 2 つの成果物を配布します。

| 成果物 | レジストリ | ソース |
| --- | --- | --- |
| `chamberlain-core` | crates.io | `packages/core` |
| `create-chamberlain` | npm | `packages/create-chamberlain` |

**この 2 つは常に同じバージョン番号でリリースします。** 片方だけ上げることはしません (変更が一方にしか無い場合でも、両方を同じ番号で publish する)。

理由は両者が実質的に結合しているためです。`create-chamberlain` が同梱するテンプレは `chamberlain-core` を version 依存で参照しており (`packages/create-chamberlain/templates/react/src-tauri/Cargo.toml`)、独立採番にすると「create-chamberlain 0.4 はどの core に対応するか」という互換マトリクスを維持し続ける義務が生まれます。単一番号なら docs も issue も「Chamberlain 0.3」と書けます。

### バージョンを持つ箇所

bump 対象は以下です。

- `packages/core/Cargo.toml` の `version`
- `packages/create-chamberlain/package.json` の `version`
- `packages/create-chamberlain/templates/react/src-tauri/Cargo.toml` の `chamberlain-core = "0.1"` — **依存指定**。minor bump のときだけ書き換える (0.x では minor が破壊的変更の単位なので、`"0.1"` は 0.1.x にしか一致しない)
- ルート `package.json` の `version` — private なので必須ではないが、揃えておくと混乱がない

以下は **bump 対象外** です。フレームワークの成果物ではなく、雛形・プレイグラウンドとしての値だからです。

- `packages/create-chamberlain/templates/react/` 配下の `package.json` / `Cargo.toml` 自身の `version` — scaffold 生成物の初期値。エージェント開発者が引き継ぐ番号
- `examples/react/` 配下の `version` — 常設プレイグラウンド。配布しない

## 0.x のセマンティクス

1.0 に到達するまでは **unstable** です。

- **minor bump (0.1 → 0.2)** — 破壊的変更を含みうる
- **patch bump (0.1.0 → 0.1.1)** — 後方互換な修正・追加のみ

これは Cargo と npm のキャレット解決 (`"0.1"` / `^0.1.0` は 0.1.x にのみ一致) と一致しているので、ルールとツールの挙動が自動で噛み合います。エージェント開発者は minor bump のたびに移行作業が発生しうる、と理解してください。

## 1.0 の定義

**1.0 = 以下の契約を凍結する宣言** です。逆に言えば、いずれか 1 つでも動かす意図が残っているうちは 0.x のままにします。

1. **トリガーモジュールの契約** — `export default { schedule, check, notify }` の shape
2. **`chamberlain.*` グローバル API** — `ai.complete` / `http.fetch` / `getSecret` などの signature
3. **トリガー配置パスの解決規則** — Tauri resource dir を起点とする現行の規則 (#19)
4. **`create-chamberlain` が生成するプロジェクト構造** — ディレクトリ配置とエントリポイント

1.0 以降、これらの破壊的変更には major bump が必要になります。

## タグ

- 形式は `vX.Y.Z` (lockstep なので 1 つだけ)。パッケージ名を含めた `chamberlain-core@X.Y.Z` 形式は使いません
- **annotated tag** で打ちます (`git tag -a`)
- publish 済みバージョンは必ずタグを持ちます。タグと GitHub Release を紐付け、Release ノートに CHANGELOG の該当節を載せます

## ブランチ

trunk-based で運用します。`develop` は設けません。

```
main                    ← 常にリリース可能。直 push は禁止 (PR 経由)
  feat/17-schedule-dsl  ← issue 番号入り、short-lived、squash merge して削除
  fix/21-review-items
```

- ブランチ名は `<type>/<issue番号>-<短い説明>`。`type` はコミットメッセージの prefix (`feat` / `fix` / `docs` / `chore` / `ci`) に揃えます
- マージ後のブランチは削除します
- **maintenance branch (`release/0.x`) は先回りして作りません。** 1.0 以降に旧系のパッチが実際に必要になった時点で切ります

## リリース手順

1. CHANGELOG.md を更新する (Keep a Changelog 形式)
2. 上記「バージョンを持つ箇所」を bump し、PR で main にマージする
3. `cargo publish -p chamberlain-core --dry-run` と `npm publish --dry-run` (create-chamberlain) で成果物を確認する
4. **`chamberlain-core` を先に publish する**
5. **その後で `create-chamberlain` を publish する**
6. `vX.Y.Z` を annotated tag で打ち、push して GitHub Release を作る

### publish 順序を守る理由

テンプレが `chamberlain-core` を version 依存で参照しているため、**core が crates.io に載る前に create-chamberlain を publish すると、scaffold した人の `cargo check` が「その version が存在しない」で失敗します。**

同じ理由で、publish 前のローカル検証では scaffold 生成物の `cargo check` が失敗するのが正常です (詳細は [`CONTRIBUTING.md`](../CONTRIBUTING.md) の「scaffold の動作検証」節)。

### dry-run で押さえたい事故

`packages/core/Cargo.toml` の `include` は明示指定です。`extension!` マクロがコンパイル時に取り込む `src/bootstrap.js` は、Cargo のデフォルト include (`src/**/*.rs`) には載りません。**この種の壊れ方は workspace の `cargo check` では検出できず、crates.io に上げた成果物だけが壊れます。** `include` を触ったリリースでは dry-run を省略しないでください。

## プレリリース

0.x の間は使いません。1.0 に向けてのみ `1.0.0-rc.1` 形式を使います。crates.io / npm とも prerelease を通常の範囲指定では解決しないので安全です (npm 側は `--tag next` を付けて publish します)。
