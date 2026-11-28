// SPDX-License-Identifier: AGPL-3.0-or-later

// The browser version's memory budget: the tab, idle with a 1,000-page workspace and one page
// open, as the desktop's budget is measured. What the browser counts for the page and its worker
// (JavaScript heaps, the DOM: performance.measureUserAgentSpecificMemory, which only Chromium has
// and only for cross-origin isolated pages, as vite preview's headers make this one), plus the
// core's WebAssembly memory, which that leaves out.

import { expect, test } from "@playwright/test";

/** Megabytes the tab may use: set from the first measurement (36 MB), about a third above. */
const BUDGET_MB = 48;
const PAGES = 1000;

// Full Chromium (its headless shell has no memory measurement), measuring at once.
test.use({ channel: "chromium", launchOptions: { args: ["--enable-blink-features=ForceEagerMeasureMemory"] } });

test("a tab with 1,000 pages and one open stays in the memory budget", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "only Chromium measures a tab's memory");
  test.setTimeout(300_000);
  await page.goto("/");
  await expect(page.locator(".editor")).toBeVisible();

  // Pages with a few blocks each, made through the core as the UI would.
  await page.evaluate(async (count) => {
    const core = window.drusiteWebCore!;
    for (let i = 0; i < count; i++) {
      const made = await core.handle({ type: "createPage", parent: null, title: `Page ${i}` });
      if (made.type !== "pageCreated") throw new Error(made.type);
      const opened = await core.handle({ type: "openPage", page: made.page.id });
      if (opened.type !== "opened") throw new Error(opened.type);
      const block = opened.view.blocks[0]!.id;
      const text = `Notes for page ${i}: ${"the quick brown fox jumps over the lazy dog ".repeat(4)}`;
      await core.handle({
        type: "edit",
        page: made.page.id,
        edits: [
          { type: "insertText", block, pos: 0, text },
          { type: "split", block, pos: text.length, kind: { type: "paragraph" } },
        ],
      });
      await core.handle({ type: "closePage", page: made.page.id });
    }
  }, PAGES);

  // As someone opens it: a fresh load, one page open, idle.
  await page.reload();
  await expect(page.locator(".editor")).toBeVisible();
  await page.locator(".tree-row .tree-title", { hasText: /^Page 500$/ }).click();
  await expect(page.getByLabel("Page title")).toHaveValue("Page 500");
  await page.locator(".editor").click();
  await page.waitForTimeout(2000);

  const [heaps, wasm] = await page.evaluate(async () => {
    const measure = (performance as unknown as { measureUserAgentSpecificMemory(): Promise<{ bytes: number }> })
      .measureUserAgentSpecificMemory;
    if (!crossOriginIsolated || !measure) throw new Error("this page can't measure its memory");
    return [(await measure.call(performance)).bytes, await window.drusiteWebCore!.wasmMemory()];
  });
  expect(wasm).toBeGreaterThan(0);
  const mb = (heaps + wasm) / 1024 / 1024;
  console.log(`tab memory: ${mb.toFixed(1)} MB of ${BUDGET_MB} MB (WebAssembly ${(wasm / 1024 / 1024).toFixed(1)} MB)`);
  test.info().annotations.push({ type: "memory", description: `${mb.toFixed(1)} MB` });
  expect(mb).toBeLessThan(BUDGET_MB);
});
