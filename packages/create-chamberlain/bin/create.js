#!/usr/bin/env node
// create-chamberlain: Chamberlain の scaffold CLI。
// 使い方:
//   npm create chamberlain            # target/name を対話的に決める代わりに引数で
//   npm create chamberlain <target>
//   npm create chamberlain <target> --template react
//   npm create chamberlain <target> <name>
//   npm create chamberlain <target> <name> --template react
//
// テンプレは同梱の templates/<name>/ をコピーし、project 固有の name/identifier を
// 書き換えるだけの薄い CLI。Vue 版などのスタックが増えたら templates/ に並べる。

import { cp, readFile, readdir, rename, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// packages/create-chamberlain/bin -> packages/create-chamberlain
const packageRoot = path.resolve(__dirname, "..");
const templatesRoot = path.join(packageRoot, "templates");

// --- args ---
const { positional, options } = parseArgs(process.argv.slice(2));
const [targetArg, nameArg] = positional;

if (!targetArg) {
  usage("target directory is required");
}

const template = options.template ?? "react";
const templateDir = path.join(templatesRoot, template);
if (!existsSync(templateDir)) {
  const available = (await listTemplates()).join(", ") || "(none)";
  usage(`unknown template: "${template}" (available: ${available})`);
}

const targetDir = path.resolve(targetArg);
const projectName = sanitizeName(nameArg ?? path.basename(targetDir));
const projectLibName = projectName.replaceAll("-", "_") + "_lib";
const projectIdentifier = `com.example.${projectName.replaceAll("-", "_")}`;

// --- guards ---
if (existsSync(targetDir)) {
  console.error(`target already exists: ${targetDir}`);
  process.exit(1);
}

// --- copy ---
await cp(templateDir, targetDir, { recursive: true });

// --- transforms ---
await renameDotfiles();
await transformCargoToml();
await transformMainRs();
await transformTauriConf();
await transformPackageJson();

console.log("");
console.log(`scaffolded ${projectName} at ${targetDir}`);
console.log("");
console.log("next:");
console.log(`  cd ${path.relative(process.cwd(), targetDir) || "."}`);
console.log("  pnpm install");
console.log("  pnpm tauri dev");
console.log("");
console.log(
  "secrets: copy .env.example to .env and fill in your API keys, or use the Settings tab after launch.",
);

// --- helpers ---

function parseArgs(argv) {
  const positional = [];
  const options = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--template" || a === "-t") {
      options.template = argv[++i];
    } else if (a.startsWith("--template=")) {
      options.template = a.slice("--template=".length);
    } else if (a === "--help" || a === "-h") {
      usage();
    } else if (a.startsWith("-")) {
      usage(`unknown option: ${a}`);
    } else {
      positional.push(a);
    }
  }
  return { positional, options };
}

function usage(msg) {
  if (msg) console.error(`error: ${msg}\n`);
  console.error(
    "usage: create-chamberlain <target-dir> [project-name] [--template <name>]",
  );
  console.error("");
  console.error("options:");
  console.error(
    "  --template, -t <name>   template to use (default: react)",
  );
  console.error("");
  process.exit(msg ? 1 : 0);
}

async function listTemplates() {
  try {
    const entries = await readdir(templatesRoot, { withFileTypes: true });
    return entries.filter((e) => e.isDirectory()).map((e) => e.name);
  } catch {
    return [];
  }
}

/**
 * npm は tarball 生成時にドットファイルを特殊扱いする (`.gitignore` は落とされる)。
 * テンプレ側では `_` 始まりで保存し、scaffold 時に復元する。
 *
 * `.github` が同じ扱いを受けるかは仕様上グレーなので、`npm pack --dry-run` で事後に
 * 気づくより既存のパターンに乗せて不確実性ごと消している (#37)。
 */
async function renameDotfiles() {
  // 配列を関数内に置くのは、この helper が top-level await から呼ばれるため
  // (helpers は呼び出しより下に定義されており、const は巻き上がらない)。
  const dotfiles = [
    ["_gitignore", ".gitignore"],
    ["_github", ".github"],
    ["_claude", ".claude"],
  ];
  for (const [from, to] of dotfiles) {
    const src = path.join(targetDir, from);
    if (existsSync(src)) {
      await rename(src, path.join(targetDir, to));
    }
  }
}

async function transformCargoToml() {
  const p = path.join(targetDir, "src-tauri", "Cargo.toml");
  let s = await readFile(p, "utf8");
  s = s.replace(/^name = "chamberlain-app"$/m, `name = "${projectName}"`);
  s = s.replace(
    /^description = ".*"$/m,
    `description = "${projectName} — Chamberlain agent app"`,
  );
  s = s.replace(
    /^name = "chamberlain_app_lib"$/m,
    `name = "${projectLibName}"`,
  );
  await writeFile(p, s);
}

async function transformMainRs() {
  const p = path.join(targetDir, "src-tauri", "src", "main.rs");
  let s = await readFile(p, "utf8");
  s = s.replaceAll("chamberlain_app_lib", projectLibName);
  await writeFile(p, s);
}

async function transformTauriConf() {
  const p = path.join(targetDir, "src-tauri", "tauri.conf.json");
  const conf = JSON.parse(await readFile(p, "utf8"));
  conf.productName = projectName;
  conf.identifier = projectIdentifier;
  if (conf.app?.windows?.[0]) {
    conf.app.windows[0].title = projectName;
  }
  await writeFile(p, JSON.stringify(conf, null, 2) + "\n");
}

async function transformPackageJson() {
  const p = path.join(targetDir, "package.json");
  const pkg = JSON.parse(await readFile(p, "utf8"));
  pkg.name = projectName;
  await writeFile(p, JSON.stringify(pkg, null, 2) + "\n");
}

function sanitizeName(raw) {
  const name = raw.toLowerCase().replace(/^\.+/, "");
  if (!/^[a-z][a-z0-9-]*$/.test(name)) {
    console.error(
      `invalid project name: "${raw}" (expected /^[a-z][a-z0-9-]*$/)`,
    );
    process.exit(1);
  }
  return name;
}
