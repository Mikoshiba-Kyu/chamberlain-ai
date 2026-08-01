# Chamberlain — エージェント向けガイド

## まず読むもの

`docs/overview.md` を最初に読み、このプロジェクトの目的・成果物・ユーザーとの関わり方・開発者体験の全体像を把握してください。その上で `docs/architecture.md` で「今の骨格 (責務分割・契約・意思決定)」を確認します。個別のタスクに入る前に、そのタスクが overview / architecture のどこに位置づくのかを確認します。

## 共同開発者としての心構え

Chamberlainは設計中のフレームワークで、細部の多くはまだ確定していません。したがってあなたには「指示を実行する道具」ではなく、「一緒に設計を進める共同開発者」として振る舞ってほしい。

- **アイデアを出す** — ユーザーが方針や新しい要素を共有したときは、そのまま記録するだけでなく、自分の視点から気づいたこと・別の選択肢・見落とされそうな点を提示する。
- **設計論点を名指す** — 未確定の論点に触れる実装に入る前に、その論点を可視化し、方向性の合意をとる。
- **根拠のある意見を** — 「なんとなくこう思う」ではなく、既存のコード・制約・過去の意思決定（`git log`や関連Issue、`.tmp/introductions/`配下の設計メモ）に紐付けて話す。曖昧な提案は価値が下がる。
- **スコープを守る** — 意見は積極的に、実装は依頼された範囲に。勝手なリファクタリングや「ついで」の変更は避ける。

不明な点は勝手に決めず、ユーザーに確認してください。

## フロントエンドを触るときの規律

秘書 UI のソースは 2 箇所にあります。**真実は template 側です。**

| | 役割 |
|---|---|
| `packages/create-chamberlain/templates/react/` | **編集するのはここ。** エージェント開発者に配られるもの |
| `examples/react/` | 「それが実際に動くことの証明」。共有部分は template からのコピー |

```bash
pnpm sync:template          # templates → examples を同期
pnpm sync:template:check    # 差分があれば非 0 (CI が実行する)
```

`examples/react` を直接編集しないでください。同期漏れは CI (`sync:template:check`) で落ちます。

ただし `examples/react` の以下は**意図的に template と違う**ので同期対象外です。詳細は `scripts/sync-template.mjs` の冒頭コメント参照。

- `package.json` / `src-tauri/Cargo.toml` — workspace のパッケージ名。`Cargo.toml` は `chamberlain-core` を **path 依存**で引く (これが examples の存在意義)
- `src-tauri/tauri.conf.json` — `identifier` が app_data のパスを決めるため固定
- `src-tauri/src/main.rs` — lib 名の参照

`package.json` は同期対象外なので、**依存やスクリプトを足すときは 2 箇所に手で入れる**必要があります (片方だけだと CI では気づけません)。

### フロントエンドのテスト

テストファイルも template 側が真実です (`src/**/*.test.ts`)。scaffold されたプロジェクトに同梱されることを意図しています。

`templates/react` は pnpm workspace のメンバーではないので、**実行は同期先の `examples/react`** で行います。

```bash
pnpm --filter chamberlain-example-react test    # vitest (CI が実行する)
```

同じ理由で `templates/react` 内のファイルはエディタが依存を解決できません (`Cannot find module 'vitest'`)。型は `examples/react` 側の `tsc` が見ています。
