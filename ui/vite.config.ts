// SPDX-License-Identifier: AGPL-3.0-or-later

import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: {
    target: "safari17",
    outDir: "dist",
    emptyOutDir: true,
    // No inline scripts or styles: the app's CSP allows only files it ships.
    assetsInlineLimit: 0,
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
  },
});
