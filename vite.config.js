import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import fs from "fs";
import path from "path";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

function findSvelteFiles(dir) {
  const results = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) results.push(...findSvelteFiles(full));
    else if (entry.name.endsWith(".svelte")) results.push(full);
  }
  return results;
}

// vite-plugin-svelte has a race on startup: if a virtual CSS module is requested
// before the parent .svelte is compiled, it returns undefined and Vite serves the
// raw .svelte file as CSS, breaking PostCSS. Pre-warming all .svelte files on
// server start ensures the cache is populated before any CSS request arrives.
const svelteCssCacheMissGuard = {
  name: "svelte-css-cache-miss-guard",
  enforce: "pre",
  configureServer(server) {
    server.httpServer?.once("listening", async () => {
      const files = findSvelteFiles(path.join(server.config.root, "src"));
      await Promise.all(files.map((f) => server.transformRequest(f).catch(() => {})));
    });
  },
  transform(code, id) {
    if (id.includes("?svelte&type=style") && code.trimStart().startsWith("<")) {
      return { code: "", map: null };
    }
  },
};

export default defineConfig(async () => ({
  plugins: [sveltekit(), svelteCssCacheMissGuard],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || "127.0.0.1",
    proxy: {
      "/vault/assets": "http://127.0.0.1:8765",
      "^/notes/.+/view(\\?.*)?$": { target: "http://127.0.0.1:8765", changeOrigin: true },
    },
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
}));
