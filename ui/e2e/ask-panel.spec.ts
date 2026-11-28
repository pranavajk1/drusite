// SPDX-License-Identifier: AGPL-3.0-or-later

// Where Ask opens. From a page's own button: a panel beside that page, which asks about it,
// resizes from its edge and keeps its width. From the sidebar or ⌘J: over the page, and it can
// fill the page for a long chat. On phones both fill the screen. The question box keeps its
// placeholder centred, and names the server's model only as a small label beside it.

import { type Page, expect, test } from "@playwright/test";

import { openApp } from "./helpers";

/** Two pages, House and Office, each with a key in it; Office is open. */
async function twoPages(page: Page) {
  await openApp(page);
  await page.keyboard.type("The spare key is under the blue flowerpot");
  await page.getByLabel("Page title").fill("House");
  await page.getByRole("button", { name: "New page", exact: true }).click();
  await expect(page.getByLabel("Page title")).toBeFocused();
  await page.keyboard.press("Enter");
  await page.keyboard.type("The office badge is in the top drawer");
  await page.getByLabel("Page title").fill("Office");
}

/** Asks once the answer before has finished; the new answer, as it streams in. */
async function ask(page: Page, question: string) {
  const panel = page.getByRole("region", { name: "Ask your notes" });
  await expect(panel.getByRole("button", { name: "Stop" })).toHaveCount(0);
  await panel.getByLabel("Question").fill(question);
  await page.keyboard.press("Enter");
  await expect(panel.locator(".ask-question").last()).toHaveText(question);
  return panel.locator(".ask-answer").last();
}

const box = async (page: Page, selector: string) => (await page.locator(selector).first().boundingBox())!;

test("from a page, Ask opens beside it and asks about that page", async ({ page }) => {
  await twoPages(page);
  const main = await box(page, ".main");
  await page.getByRole("button", { name: "Ask about this page" }).click();
  const panel = page.getByRole("region", { name: "Ask your notes" });
  await expect(panel).toBeVisible();
  await expect(panel.getByLabel("Question")).toBeFocused();

  // Beside the page, not over it: the page gets narrower and ends where the panel starts.
  const side = await box(page, ".right-panel");
  const squeezed = await box(page, ".main");
  expect(squeezed.width).toBeLessThan(main.width - 200);
  expect(squeezed.x + squeezed.width).toBeLessThanOrEqual(side.x + 1);
  await expect(panel.getByRole("button", { name: "Expand to full page" })).toHaveCount(0);

  // Kept to this page.
  await expect(panel.locator(".ask-scope")).toHaveText("Office");
  await expect(panel).toContainText("Answers come from this page and the pages inside it.");
  await expect(await ask(page, "Where is the office badge?")).toContainText("top drawer");
  await expect(await ask(page, "Where is the spare key?")).toContainText("couldn't find");

  // Asked about everything again, the other page answers.
  await panel.getByRole("button", { name: "Ask all your notes" }).click();
  await expect(panel.locator(".ask-scope")).toHaveCount(0);
  await expect(await ask(page, "Where is the spare key?")).toContainText("blue flowerpot");

  // Another page: the panel stays, and keeps to that page instead.
  await page.getByRole("treeitem", { name: "House" }).click();
  await expect(page.getByLabel("Page title")).toHaveValue("House");
  await expect(panel.locator(".ask-scope")).toHaveText("House");

  // The page's button closes it again.
  await page.getByRole("button", { name: "Ask about this page" }).click();
  await expect(panel).toBeHidden();
});

test("the panel beside the page resizes from its edge and keeps its width", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: "Ask about this page" }).click();
  const handle = page.getByRole("separator", { name: "Resize Ask panel" });
  const before = await box(page, ".right-panel");
  const at = (await handle.boundingBox())!;
  const x = at.x + at.width / 2;
  await page.mouse.move(x, at.y + 200);
  await page.mouse.down();
  await page.mouse.move(x - 60, at.y + 210, { steps: 5 });
  await page.mouse.move(x - 120, at.y + 220, { steps: 5 });
  await page.mouse.up();
  const after = await box(page, ".right-panel");
  expect(Math.round(after.width - before.width)).toBe(120);

  // From the keyboard too.
  await handle.focus();
  await page.keyboard.press("ArrowRight");
  expect(Math.round((await box(page, ".right-panel")).width)).toBe(Math.round(after.width) - 16);

  // Still open, and as wide, after the app starts again.
  await page.reload();
  await expect(page.getByRole("region", { name: "Ask your notes" })).toBeVisible();
  expect(Math.round((await box(page, ".right-panel")).width)).toBe(Math.round(after.width) - 16);
});

test("from the sidebar, Ask floats over the page and can fill it", async ({ page }) => {
  await twoPages(page);
  const main = await box(page, ".main");
  await page.getByRole("button", { name: "Ask ⌘J" }).click();
  const panel = page.getByRole("region", { name: "Ask your notes" });
  await expect(panel).toBeVisible();
  // Over the page: it keeps its width. Everything is asked, with no page named.
  expect((await box(page, ".main")).width).toBe(main.width);
  await expect(panel.locator(".ask-scope")).toHaveCount(0);
  await expect(panel).toContainText("Answers come from your pages.");
  await expect(await ask(page, "Where is the spare key?")).toContainText("blue flowerpot");

  // Filling the page, the chat carries on.
  await panel.getByRole("button", { name: "Expand to full page" }).click();
  const full = await box(page, ".ask-float");
  const sidebar = await box(page, ".sidebar");
  expect(Math.round(full.x)).toBe(Math.round(sidebar.width));
  expect(Math.round(full.x + full.width)).toBe(page.viewportSize()!.width);
  await expect(panel.locator(".ask-answer").last()).toContainText("blue flowerpot");

  // A citation shows its page, so the chat shrinks back to a panel over it.
  await panel.getByRole("button", { name: "Source 1: House" }).click();
  await expect(page.getByLabel("Page title")).toHaveValue("House");
  await expect(panel.getByRole("button", { name: "Expand to full page" })).toBeVisible();

  await panel.getByRole("button", { name: "Expand to full page" }).click();
  await panel.getByRole("button", { name: "Shrink to a panel" }).click();
  expect((await box(page, ".ask-float")).width).toBeLessThan(600);

  // ⌘J closes it.
  await page.keyboard.press("ControlOrMeta+j");
  await expect(panel).toBeHidden();
});

test("the server's model is a small label by the question box, never in the text", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+j");
  const panel = page.getByRole("region", { name: "Ask your notes" });
  // The in-memory core stands in for the server, and calls its model "Memory".
  await expect(panel.locator(".ask-compose .ask-model")).toHaveText("Memory");
  await expect(panel.locator(".ask-empty")).not.toContainText("Memory");
  await expect(panel.locator(".ask-empty")).not.toContainText("written by");
  await expect(panel.locator(".ask-empty-text").first()).toHaveText("Answers come from your pages.");
  await expect(panel.locator(".ask-empty-text").nth(1)).toContainText("Journal entries are left out.");
});

/** The question box's text sits in the middle of it: as much room above the line as below. */
async function placeholderCentred(page: Page) {
  const m = await page.getByLabel("Question").evaluate((el) => {
    const s = getComputedStyle(el);
    return {
      top: parseFloat(s.paddingTop),
      bottom: parseFloat(s.paddingBottom),
      line: parseFloat(s.lineHeight),
      height: el.getBoundingClientRect().height,
    };
  });
  expect(m.top).toBe(m.bottom);
  expect(m.height).toBeCloseTo(m.line + m.top + m.bottom, 0);
}

test("the question box's placeholder is centred", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+j");
  await placeholderCentred(page);
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  test("both ways in fill the screen, and the placeholder is centred", async ({ page }) => {
    await twoPages(page);
    const panel = page.getByRole("region", { name: "Ask your notes" });

    // From the page's own button: the whole screen, about this page.
    await page.getByRole("button", { name: "Ask about this page" }).click();
    let b = (await panel.boundingBox())!;
    expect(b.width).toBe(390);
    expect(b.height).toBeGreaterThan(800);
    await expect(panel.locator(".ask-scope")).toHaveText("Office");
    await expect(panel.getByRole("separator")).toHaveCount(0);
    await placeholderCentred(page);
    await panel.getByRole("button", { name: "Close Ask" }).click();
    await expect(panel).toBeHidden();

    // From the tab bar: the same, about everything, with nothing to expand.
    await page.getByRole("navigation", { name: "Main" }).getByRole("button", { name: "Ask" }).click();
    b = (await panel.boundingBox())!;
    expect(b.width).toBe(390);
    expect(b.height).toBeGreaterThan(800);
    await expect(panel.locator(".ask-scope")).toHaveCount(0);
    await expect(panel.getByRole("button", { name: "Expand to full page" })).toBeHidden();
    await placeholderCentred(page);
  });
});
