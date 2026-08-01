/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],
  // Emit relative asset paths (`./assets/...`) so the built bundle works under
  // Tauri's custom protocol (tauri://localhost / https://tauri.localhost).
  base: "./",
  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
  test: {
    // 表示用の純関数は Date のローカル解釈に依存する。TZ を固定しないと開発者の環境と
    // CI (UTC) で結果が変わるため、テストだけ固定の TZ で回す。UTC 以外を選ぶのは
    // 「ローカル時刻で表示している」ことを assertion で言えるようにするため。
    env: { TZ: "Asia/Tokyo" },
    include: ["src/**/*.test.ts"],
  },
}));
