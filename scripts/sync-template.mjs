#!/usr/bin/env node
// templates/react → examples/react の同期 (#39)。
//
// フロントエンドのソースが 2 箇所にあり、手コピーで同期されていた。#26 の実装中に
// 7 ファイルを `cp` する事故が起きたので、単一のコマンドに閉じ込める。
//
//   pnpm sync:template          同期する
//   pnpm sync:template --check  差分があれば非 0 で終わる (CI 用)
//
// **真実は `packages/create-chamberlain/templates/react` 側**。エージェント開発者に
// 配られるものが正であり、`examples/react` は「それが実際に動くことの証明」でしかない。
//
// ## なぜ全ファイルをコピーしないのか
//
// `examples/react` は template の byte 単位のコピーではない。以下は**意図的に**違う。
// これらを同期すると examples が壊れる。
//
// | ファイル | 差異 | 理由 |
// |---|---|---|
// | `package.json` | `name` | workspace のパッケージ名 (`pnpm --filter` の対象) |
// | `src-tauri/Cargo.toml` | crate 名 / lib 名 / description / authors | 同上 (cargo workspace のメンバー名) |
// | `src-tauri/Cargo.toml` | `chamberlain-core` の依存 | **examples は `path` 依存**。これが無いとローカルの core を検証できず、examples の存在意義が消える |
// | `src-tauri/tauri.conf.json` | `productName` / `identifier` / `title` | identifier は app_data のパスを決める。`dev.chamberlain.interval-notifier` 固定でないと検証のたびに state が別の場所に行く |
// | `src-tauri/src/main.rs` | lib 名の参照 | Cargo.toml の lib 名に従う |
//
// ## なぜ symlink にしないのか
//
// `examples/react/src` を template への symlink にすれば「コピー」自体が消えるが、
// Tauri は Windows を配布対象に含んでおり、native Windows で clone した際の symlink の
// 扱い (要 developer mode / core.symlinks) と vite の解決を保証できない。
// 「同期漏れが CI で必ず落ちる」ところまで担保できれば目的は達成されるので、
// 素朴なコピー + 検証を採る。

import { readFile, writeFile, mkdir, readdir } from "node:fs/promises";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const SOURCE = path.join(repoRoot, "packages/create-chamberlain/templates/react");
const TARGET = path.join(repoRoot, "examples/react");

/**
 * プロジェクト固有の値を持つため同期しないファイル (上表参照)。
 * 追加するときは「なぜ examples 側で違っていて良いのか」を上表にも書く。
 */
const DIVERGENT = new Set([
  "package.json",
  "src-tauri/Cargo.toml",
  "src-tauri/tauri.conf.json",
  "src-tauri/src/main.rs",
]);

/**
 * scaffold 時にしか意味を持たないので examples には置かないファイル。
 * - `_gitignore`  npm publish が `.gitignore` を落とす仕様への対処。examples はレポの
 *                 ルート `.gitignore` に従う
 * - `README.md`   scaffold されたプロジェクトの README。examples には不要
 * - `.env.example` examples では実物の `.env` を使う (gitignore 済み)
 */
const TEMPLATE_ONLY = new Set(["_gitignore", "README.md", ".env.example"]);

/**
 * scaffold 時に `.` 付きへ戻るディレクトリ (`bin/create.js` の renameDotfiles())。
 * **配下は丸ごと examples に出さない** — `_` のままコピーされても誰からも読まれない。
 *
 * - `_github/` scaffold されたプロジェクト用の GitHub Actions ワークフロー (#37)。リポ
 *              本体は同じ役割を `.github/workflows/build-example.yml` が持つ (#38)。
 *              テンプレのものは自己完結でなければならないので別ファイル
 * - `_claude/` トリガー仕様書の skill (#60)。中身の実体は core にある (下記)
 *
 * ファイル単位ではなく接頭辞で見るのは、`_claude/` が skills ディレクトリで、2 つ目の
 * skill や参照ファイルが増えるのが前提の場所だから。1 つ足すたびにこの表を直す規律を
 * 作ると、忘れた分が examples に死骸として生える。
 */
const TEMPLATE_ONLY_DIRS = ["_github", "_claude"];

/**
 * **core が実体を持ち、template へ配るもの** (#60)。
 *
 * 上の原則 (真実は template) の唯一の例外。トリガー仕様書は `packages/core` に置いて
 * バイナリに焼き込んである (`include_str!`) — 配布されたアプリから skill として書き出せる
 * 必要があり、かつ**実装と同じバージョンの仕様が出てこなければ意味が無い**ため。
 *
 * 配り先は scaffold されたプロジェクトの `.claude/skills/`。ただの md を置いても「読めと
 * 言われないと読まない」が、skill なら「トリガーを足して」で載る。中身は配布アプリが
 * 書き出すものと byte 単位で同じ。
 */
const FROM_CORE = new Map([
  [
    path.join("packages", "core", "src", "trigger-spec.md"),
    path.join("_claude", "skills", "chamberlain-triggers", "SKILL.md"),
  ],
]);

/** examples に配らないもの (上の 2 つの表の合成)。 */
function isTemplateOnly(rel) {
  return (
    TEMPLATE_ONLY.has(rel) ||
    TEMPLATE_ONLY_DIRS.some((dir) => rel.startsWith(dir + path.sep))
  );
}

const checkOnly = process.argv.includes("--check");

/** SOURCE 配下の全ファイルを相対パスで列挙する (node_modules 等は template に無い)。 */
async function listFiles(dir, base = dir) {
  const out = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...(await listFiles(full, base)));
    } else if (entry.isFile()) {
      out.push(path.relative(base, full));
    }
  }
  return out.sort();
}

if (!existsSync(SOURCE)) {
  console.error(`source template not found: ${SOURCE}`);
  process.exit(1);
}

let written = 0;

/**
 * 1 ファイル分の同期。同じかどうかを返し、`--check` でなければ書く。
 * 「同じなら触らない」「書いたら数える」は両方の段で同じ約束なのでここに閉じる。
 */
async function syncFile(fromAbs, toAbs) {
  const src = await readFile(fromAbs);
  const exists = existsSync(toAbs);
  if (exists && src.equals(await readFile(toAbs))) return "same";
  if (!checkOnly) {
    await mkdir(path.dirname(toAbs), { recursive: true });
    await writeFile(toAbs, src);
    written++;
  }
  return exists ? "drifted" : "missing";
}

// core → template を先に流す。この後の template → examples が続きを運ぶ。
const coreDrifted = [];
for (const [from, rel] of FROM_CORE) {
  const state = await syncFile(path.join(repoRoot, from), path.join(SOURCE, rel));
  if (state !== "same") coreDrifted.push(`${from} -> templates/react/${rel}`);
}

const files = await listFiles(SOURCE);
const shared = files.filter((f) => !DIVERGENT.has(f) && !isTemplateOnly(f));

const drifted = [];
const missing = [];

for (const rel of shared) {
  const state = await syncFile(path.join(SOURCE, rel), path.join(TARGET, rel));
  if (state === "missing") missing.push(rel);
  else if (state === "drifted") drifted.push(rel);
}

// template から消えたのに examples に残っているファイルを検出する。
// 「トリガーを 1 つ削除した」ときに examples 側だけ残る事故を拾う。
const stale = [];
if (existsSync(TARGET)) {
  const sharedDirs = ["src", "triggers"];
  for (const dir of sharedDirs) {
    const abs = path.join(TARGET, dir);
    if (!existsSync(abs)) continue;
    for (const rel of await listFiles(abs, TARGET)) {
      if (!files.includes(rel)) stale.push(rel);
    }
  }
}

const problems = [...missing, ...drifted, ...stale, ...coreDrifted];

if (checkOnly) {
  if (problems.length === 0) {
    console.log(`sync:template --check OK (${shared.length} shared files in sync)`);
    process.exit(0);
  }
  // 2 段あるので、どちらがずれたかで直す先が違う。**core 起点のファイルだけは
  // template が真実ではない**ので、同じ fix 文を出すと消える編集を促すことになる。
  if (missing.length + drifted.length + stale.length > 0) {
    console.error("examples/react is out of sync with templates/react.\n");
    for (const f of missing) console.error(`  missing in examples : ${f}`);
    for (const f of drifted) console.error(`  content differs     : ${f}`);
    for (const f of stale) console.error(`  stale in examples   : ${f}`);
    console.error(
      "\nfix: edit packages/create-chamberlain/templates/react (the source of truth), " +
        "then run `pnpm sync:template`.\n",
    );
  }
  if (coreDrifted.length > 0) {
    console.error("the template copy is out of sync with its source in packages/core.\n");
    for (const f of coreDrifted) console.error(`  core copy differs   : ${f}`);
    console.error(
      "\nfix: edit the file under packages/core (the source of truth for these), " +
        "then run `pnpm sync:template`.",
    );
  }
  process.exit(1);
}

if (stale.length > 0) {
  console.error("\nthese files exist in examples/react but not in the template:");
  for (const f of stale) console.error(`  ${f}`);
  console.error("delete them by hand if they were removed from the template.");
}

console.log(
  written === 0
    ? `sync:template: already in sync (${shared.length} shared files)`
    : `sync:template: updated ${written} file(s)`,
);
if (stale.length > 0) process.exit(1);
