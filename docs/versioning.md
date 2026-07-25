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

[`.github/workflows/release.yml`](../.github/workflows/release.yml) が `vX.Y.Z` タグの push を起点に publish します。人がやるのは bump とタグを打つ判断だけです。

| | 誰が | 何を |
| --- | --- | --- |
| 1 | 人 | `CHANGELOG.md` を更新し、上記「バージョンを持つ箇所」を bump して PR で main へ |
| 2 | CI | PR 上で `--dry-run` を回す ([`ci.yml`](../.github/workflows/ci.yml)) |
| 3 | 人 | `git tag -a vX.Y.Z && git push origin vX.Y.Z` |
| 4 | CI | タグと各マニフェストの version 一致を検証 → `chamberlain-core` publish → `create-chamberlain` publish → GitHub Release 作成 |

dry-run をタグ後ではなく **PR 時点で** 回すのは、publish が事実上取り消せないためです (crates.io は yank しかできず、npm も 72 時間以内の unpublish のみ)。タグを打った後に壊れていると分かっても手遅れなので、検証はマージ前に済ませます。

bump 内容の決定とタグを打つ判断は自動化しません。「今回は破壊的変更だから minor」は 0.x のセマンティクス上、機械には決められない判断です。

Release ノートは `CHANGELOG.md` の該当バージョンの節がそのまま使われます。節が見つからない場合は自動生成にフォールバックし、warning を出します。

### 作業手順

`X.Y.Z` を出すバージョンに読み替えてください。

**1. bump ブランチを切る**

```bash
git checkout main && git pull
git checkout -b release/X.Y.Z
```

**2. `CHANGELOG.md` を更新する**

- `## [Unreleased]` の内容を `## [X.Y.Z] - YYYY-MM-DD` として切り出す
- 空の `## [Unreleased]` は残す
- 末尾のリンク定義を更新する (`[Unreleased]` の compare 範囲と `[X.Y.Z]` の行)

**3. version を bump する**

「バージョンを持つ箇所」節のとおり、以下を `X.Y.Z` にします。

- `packages/core/Cargo.toml`
- `packages/create-chamberlain/package.json`
- `package.json` (ルート)

**minor bump のときは** `packages/create-chamberlain/templates/react/src-tauri/Cargo.toml` の `chamberlain-core = "0.X"` も同時に更新します (patch bump では不要 — `"0.1"` は 0.1.x に一致するため)。

**4. `Cargo.lock` を更新する**

```bash
cargo check -p chamberlain-core
```

**5. PR を出してマージする**

```bash
git add -A && git commit -m "chore(release): X.Y.Z"
git push -u origin release/X.Y.Z
gh pr create --title "chore(release): X.Y.Z" --body "..."
```

CI (`ci.yml`) が緑になったらマージします。ここで `--dry-run` が通ることを確認しているので、以降は失敗しにくくなります。

**6. タグを打つ**

```bash
git checkout main && git pull
git tag -a vX.Y.Z -m "chamberlain-core X.Y.Z / create-chamberlain X.Y.Z"
git push origin vX.Y.Z
```

**7. Release workflow を見守る**

```bash
gh run watch
```

**8. 結果を確認する**

- <https://crates.io/crates/chamberlain-core>
- <https://www.npmjs.com/package/create-chamberlain>
- `gh release view vX.Y.Z`

### 失敗したときの対処

**publish は取り消せません。** どの段階で落ちたかで対処が変わります。

| 落ちた job | 状態 | 対処 |
| --- | --- | --- |
| `verify` | 何も publish されていない | タグを消して直す (下記)。安全 |
| `publish-crate` | 何も publish されていない | 原因を直し、タグを消して打ち直す |
| `publish-npm` | **core だけ publish 済み** | Actions の **Re-run failed jobs** を使う。全体を re-run すると `cargo publish` が `already exists` で落ちる |
| `github-release` | 両方 publish 済み | 同上、または `gh release create` を手で実行 |

タグを打ち直す場合:

```bash
git push origin --delete vX.Y.Z
git tag -d vX.Y.Z
# 修正して PR → マージ後、改めてタグを打つ
```

**既に publish された version 番号は再利用できません。** `publish-crate` が成功した後にやり直しが必要になったら、次の patch version で出し直してください (crates.io は yank しかできず、npm も 72 時間以内の unpublish のみです)。

### Trusted Publishing

publish の認証は crates.io / npm 双方の Trusted Publishing (OIDC) を使い、トークンも 2FA も CI に持たせません。レジストリ側には以下を登録しています。

- **crates.io** — repository + workflow filename (`release.yml`)
- **npm** — organization/user + repository + workflow filename (`release.yml`)、allowed actions は `npm publish` のみ

**`release.yml` はリネームしないでください。** workflow 名を変えるとレジストリ側の登録と一致せず、publish が失敗します。

### publish 順序を守る理由

テンプレが `chamberlain-core` を version 依存で参照しているため、**core が crates.io に載る前に create-chamberlain を publish すると、scaffold した人の `cargo check` が「その version が存在しない」で失敗します。**

同じ理由で、publish 前のローカル検証では scaffold 生成物の `cargo check` が失敗するのが正常です (詳細は [`CONTRIBUTING.md`](../CONTRIBUTING.md) の「scaffold の動作検証」節)。

### dry-run で押さえたい事故

`packages/core/Cargo.toml` の `include` は明示指定です。`extension!` マクロがコンパイル時に取り込む `src/bootstrap.js` は、Cargo のデフォルト include (`src/**/*.rs`) には載りません。**この種の壊れ方は workspace の `cargo check` では検出できず、crates.io に上げた成果物だけが壊れます。** `include` を触ったリリースでは dry-run を省略しないでください。

## プレリリース

0.x の間は使いません。1.0 に向けてのみ `1.0.0-rc.1` 形式を使います。crates.io / npm とも prerelease を通常の範囲指定では解決しないので安全です (npm 側は `--tag next` を付けて publish します)。
