#!/usr/bin/env node
// examples/react を種にして、workspace の *外* に単体プロジェクトを生成する。
// 目的は #9 の「別プロジェクトから core を呼び出しても動くか」検証のみ。
// publish されるまでは path 依存が絶対パスなので、生成物は使い捨て。

import { cp, readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// packages/create-chamberlain/bin -> workspace root
const workspaceRoot = path.resolve(__dirname, "..", "..", "..");
const templateDir = path.join(workspaceRoot, "examples", "react");
const coreDir = path.join(workspaceRoot, "packages", "core");

// --- args ---
// usage: create.js [target-dir] [project-name]
const [targetArg, nameArg] = process.argv.slice(2);

// デフォルト生成先は $HOME/.chamberlain-scaffold-check。
// 意図的に workspace の外に置く: Cargo/pnpm workspace の解決で
// 「同居によるごまかし」を再生産しないため。
const targetDir = path.resolve(
  targetArg ?? path.join(homedir(), ".chamberlain-scaffold-check"),
);

const projectName = sanitizeName(
  nameArg ?? deriveNameFromDir(targetDir) ?? "chamberlain-scaffold-check",
);
const projectLibName = projectName.replaceAll("-", "_") + "_lib";

// --- guards ---
if (
  targetDir === workspaceRoot ||
  targetDir.startsWith(workspaceRoot + path.sep)
) {
  console.error(
    `refusing to scaffold inside the source workspace: ${targetDir}`,
  );
  console.error(
    `pick a location outside ${workspaceRoot} so path deps prove out end-to-end.`,
  );
  process.exit(1);
}

if (existsSync(targetDir)) {
  console.error(`target already exists: ${targetDir}`);
  console.error(`run: pnpm scaffold:clean`);
  process.exit(1);
}

// --- copy ---
const SKIP = new Set([
  "node_modules",
  "dist",
  "target",
  "gen",
  ".DS_Store",
]);

await cp(templateDir, targetDir, {
  recursive: true,
  filter: (src) => {
    const base = path.basename(src);
    if (SKIP.has(base)) return false;
    if (base.endsWith(".tsbuildinfo")) return false;
    return true;
  },
});

// --- transforms ---
await transformCargoToml();
await transformMainRs();
await transformTauriConf();
await transformPackageJson();
await writeGitignore();

console.log("");
console.log(`scaffolded ${projectName} at ${targetDir}`);
console.log("");
console.log("next:");
console.log(`  cd ${targetDir}`);
console.log("  pnpm install");
console.log("  pnpm tauri dev");
console.log("");
console.log("cleanup (from source repo):");
console.log("  pnpm scaffold:clean");

// --- helpers ---

async function transformCargoToml() {
  const p = path.join(targetDir, "src-tauri", "Cargo.toml");
  let s = await readFile(p, "utf8");

  s = s.replace(
    /^name = "chamberlain-example-react"$/m,
    `name = "${projectName}"`,
  );
  s = s.replace(
    /^description = ".*"$/m,
    `description = "${projectName} — Chamberlain agent app (scaffolded via create-chamberlain)"`,
  );
  s = s.replace(
    /^name = "chamberlain_example_react_lib"$/m,
    `name = "${projectLibName}"`,
  );
  s = s.replace(
    /^chamberlain-core = \{ path = "\.\.\/\.\.\/\.\.\/packages\/core" \}$/m,
    `chamberlain-core = { path = "${toPosix(coreDir)}" }`,
  );

  await writeFile(p, s);
}

async function transformMainRs() {
  const p = path.join(targetDir, "src-tauri", "src", "main.rs");
  let s = await readFile(p, "utf8");
  s = s.replaceAll("chamberlain_example_react_lib", projectLibName);
  await writeFile(p, s);
}

async function transformTauriConf() {
  const p = path.join(targetDir, "src-tauri", "tauri.conf.json");
  const conf = JSON.parse(await readFile(p, "utf8"));
  conf.productName = projectName;
  conf.identifier = `dev.chamberlain.scaffold.${projectName.replaceAll("-", "_")}`;
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

async function writeGitignore() {
  const body = [
    "node_modules",
    "dist",
    "*.tsbuildinfo",
    "src-tauri/target",
    "src-tauri/gen",
    "",
  ].join("\n");
  await writeFile(path.join(targetDir, ".gitignore"), body);
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

function deriveNameFromDir(dir) {
  const base = path.basename(dir).replace(/^\.+/, "");
  return base || null;
}

function toPosix(p) {
  return p.split(path.sep).join("/");
}
