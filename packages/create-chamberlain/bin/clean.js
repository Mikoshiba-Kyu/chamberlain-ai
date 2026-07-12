#!/usr/bin/env node
// scaffold で生成した workspace 外のプロジェクトを削除する。
// デフォルトは create.js のデフォルト target と同じ。

import { rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(__dirname, "..", "..", "..");

const target = path.resolve(
  process.argv[2] ?? path.join(homedir(), ".chamberlain-scaffold-check"),
);

if (
  target === workspaceRoot ||
  target.startsWith(workspaceRoot + path.sep)
) {
  console.error(`refusing to clean inside source workspace: ${target}`);
  process.exit(1);
}

if (!existsSync(target)) {
  console.log(`nothing to clean at ${target}`);
  process.exit(0);
}

await rm(target, { recursive: true, force: true });
console.log(`removed ${target}`);
