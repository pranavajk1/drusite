// SPDX-License-Identifier: AGPL-3.0-or-later

// The browser version's specs run in a browser profile kept on disk, as a person's is:
// Playwright's usual throwaway contexts are like private windows, and WebKit gives those no
// OPFS (the app then keeps notes in memory only, which these specs are about not doing).

import { test as base } from "@playwright/test";

export { expect } from "@playwright/test";

export const test = base.extend({
  context: async ({ playwright, browserName }, use, testInfo) => {
    const context = await playwright[browserName].launchPersistentContext(testInfo.outputPath("profile"), {
      ...testInfo.project.use,
      baseURL: testInfo.project.use.baseURL ?? (testInfo.config.projects[0]?.use.baseURL as string | undefined),
    });
    await use(context);
    await context.close();
  },
  page: async ({ context }, use) => {
    const page = context.pages()[0] ?? (await context.newPage());
    // WebKit may hand every profile the same OPFS: start each test from an empty one.
    await page.goto("/sw.js");
    await page.evaluate(async () => {
      const root = await navigator.storage.getDirectory();
      await root.removeEntry(".drusite", { recursive: true }).catch(() => {});
    });
    await use(page);
  },
});
