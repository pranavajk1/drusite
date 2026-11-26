// SPDX-License-Identifier: AGPL-3.0-or-later

// Browser tests: the UI in a real browser engine, on the in-memory core, driven by real mouse and
// keyboard input. WebKit is the engine the Mac app runs in; Chromium is the one on Windows.

import { defineConfig, devices } from "@playwright/test";

// The UI's types leave out Node's (its code runs in a browser); this is the one Node global used here.
declare const process: { env: Record<string, string | undefined> };

const PORT = 5199;
const ci = !!process.env.CI;

export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  forbidOnly: ci,
  retries: 0,
  reporter: ci ? [["list"], ["github"]] : "list",
  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: "retain-on-failure",
  },
  projects: [
    { name: "webkit", use: { ...devices["Desktop Safari"] } },
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
  ],
  webServer: {
    command: `npx vite --port ${PORT} --strictPort`,
    url: `http://localhost:${PORT}`,
    reuseExistingServer: false,
  },
});
