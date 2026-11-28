// SPDX-License-Identifier: AGPL-3.0-or-later

// The browser version's tests: the web build (`vite build --mode web`), with the real core as
// WebAssembly and its store in OPFS, in Chromium, WebKit and Firefox. The same specs as the
// in-memory core's (playwright.config.ts) where they don't reach into that core's internals,
// plus the browser version's own in `e2e/web/`. Build the core first: `npm run wasm`.

import { defineConfig, devices } from "@playwright/test";

declare const process: { env: Record<string, string | undefined> };

const PORT = 5198;
const ci = !!process.env.CI;
process.env.DRUSITE_CORE = "wasm";

export default defineConfig({
  testDir: "e2e",
  // Specs that read the in-memory core's own state, or fake a server with it, stay with it.
  testIgnore: ["agents.spec.ts", "ai.spec.ts", "ask.spec.ts", "ask-panel.spec.ts", "share.spec.ts", "versions.spec.ts"],
  fullyParallel: true,
  forbidOnly: ci,
  retries: ci ? 1 : 0,
  reporter: ci ? [["list"], ["github"]] : "list",
  timeout: 60_000,
  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: "retain-on-failure",
  },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    { name: "webkit", use: { ...devices["Desktop Safari"] } },
    { name: "firefox", use: { ...devices["Desktop Firefox"] } },
  ],
  webServer: {
    command: `VITE_DRUSITE_TEST=1 npx vite build --mode web && npx vite preview --mode web --port ${PORT} --strictPort`,
    url: `http://localhost:${PORT}`,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
