// SPDX-License-Identifier: AGPL-3.0-or-later

import { readFileSync } from "node:fs";

import { svelte } from "@sveltejs/vite-plugin-svelte";
import type { Plugin } from "vite";
import { defineConfig } from "vitest/config";

/** The app's version, from the workspace's Cargo.toml, for Settings to show. */
function appVersion(): string {
  const toml = readFileSync(new URL("../Cargo.toml", import.meta.url), "utf8");
  const found = /\[workspace\.package\][^[]*?\nversion\s*=\s*"([^"]+)"/.exec(toml);
  if (!found) throw new Error("no [workspace.package] version in Cargo.toml");
  return found[1]!;
}

/**
 * The web build's service worker, written last so it lists every file the build made: it keeps
 * them, so the app opens with no network. See src/core/web/sw.js for what it does with them.
 */
function serviceWorker(): Plugin {
  return {
    name: "drusite-service-worker",
    apply: "build",
    generateBundle(_options, bundle) {
      const files = Object.keys(bundle)
        .filter((f) => !f.endsWith(".map") && f !== "index.html")
        .map((f) => `/${f}`);
      const source = readFileSync(new URL("./src/core/web/sw.js", import.meta.url), "utf8");
      this.emitFile({
        type: "asset",
        fileName: "sw.js",
        source: source
          .replace("self.__FILES__", JSON.stringify(["/", ...files]))
          .replace("self.__VERSION__", JSON.stringify(`${appVersion()}-${hash(files.join(","))}`)),
      });
    },
  };
}

function hash(text: string): string {
  let h = 0;
  for (let i = 0; i < text.length; i++) h = (Math.imul(31, h) + text.charCodeAt(i)) | 0;
  return (h >>> 0).toString(36);
}

export default defineConfig(({ mode }) => ({
  plugins: mode === "web" ? [svelte(), serviceWorker()] : [svelte()],
  define: { __APP_VERSION__: JSON.stringify(appVersion()) },
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  // The web build is served by drusite-server at `/`, its files under `/app/` (see
  // crates/drusite-server/src/web.rs); the app's build goes inside the Tauri bundle.
  base: mode === "web" ? "/" : undefined,
  worker: { format: "es" },
  build: {
    target: "safari17",
    outDir: mode === "web" ? "dist-web" : "dist",
    assetsDir: mode === "web" ? "app" : "assets",
    emptyOutDir: true,
    // No inline scripts or styles: the app's CSP allows only files it ships.
    assetsInlineLimit: 0,
  },
  // Svelte's browser build in tests too, so components can be mounted.
  resolve: mode === "test" ? { conditions: ["browser"] } : undefined,
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
  },
}));
