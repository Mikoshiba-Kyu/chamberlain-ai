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

const files = await listFiles(SOURCE);
const shared = files.filter((f) => !DIVERGENT.has(f) && !TEMPLATE_ONLY.has(f));

const drifted = [];
const missing = [];
let written = 0;

for (const rel of shared) {
  const from = path.join(SOURCE, rel);
  const to = path.join(TARGET, rel);
  const src = await readFile(from);

  if (!existsSync(to)) {
    missing.push(rel);
  } else {
    const dst = await readFile(to);
    if (src.equals(dst)) continue;
    drifted.push(rel);
  }

  if (!checkOnly) {
    await mkdir(path.dirname(to), { recursive: true });
    await writeFile(to, src);
    written++;
  }
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

const problems = [...missing, ...drifted, ...stale];

if (checkOnly) {
  if (problems.length === 0) {
    console.log(`sync:template --check OK (${shared.length} shared files in sync)`);
    process.exit(0);
  }
  console.error("examples/react is out of sync with templates/react.\n");
  for (const f of missing) console.error(`  missing in examples : ${f}`);
  for (const f of drifted) console.error(`  content differs     : ${f}`);
  for (const f of stale) console.error(`  stale in examples   : ${f}`);
  console.error(
    "\nfix: edit packages/create-chamberlain/templates/react (the source of truth), " +
      "then run `pnpm sync:template`.",
  );
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
    : `sync:template: updated ${written} file(s) from the template`,
);
if (stale.length > 0) process.exit(1);
