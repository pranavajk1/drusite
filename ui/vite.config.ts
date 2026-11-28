// SPDX-License-Identifier: AGPL-3.0-or-later

import { readFileSync } from "node:fs";

import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vitest/config";

/** The app's version, from the workspace's Cargo.toml, for Settings to show. */
function appVersion(): string {
  const toml = readFileSync(new URL("../Cargo.toml", import.meta.url), "utf8");
  const found = /\[workspace\.package\][^[]*?\nversion\s*=\s*"([^"]+)"/.exec(toml);
  if (!found) throw new Error("no [workspace.package] version in Cargo.toml");
  return found[1]!;
}

export default defineConfig(({ mode }) => ({
  plugins: [svelte()],
  define: { __APP_VERSION__: JSON.stringify(appVersion()) },
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: {
    target: "safari17",
    outDir: "dist",
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
