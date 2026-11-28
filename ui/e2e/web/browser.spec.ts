// SPDX-License-Identifier: AGPL-3.0-or-later

// What only the browser version does: the store in OPFS across reloads, several tabs on one
// store, opening with no network. Run by playwright.web.config.ts, on the WebAssembly core.

import type { Page } from "@playwright/test";

import { expectBlocks, openApp } from "../helpers";
import { expect, test } from "./fixtures";

const paragraph = { type: "paragraph" } as const;

// One at a time: Playwright's WebKit keeps one OPFS for all its profiles on some systems.
test.describe.configure({ mode: "serial" });

async function title(page: Page, text: string) {
  const input = page.getByLabel("Page title");
  await input.click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.type(text);
  await expect(input).toHaveValue(text);
}

/** The editor's text, block by block. */
const texts = (page: Page) => page.locator(".editor > [data-kind]").allTextContents();

test("notes are kept in the browser across reloads", async ({ page }) => {
  await openApp(page);
  await page.keyboard.type("Kept in OPFS");
  await expectBlocks(page, [{ kind: paragraph, text: "Kept in OPFS" }]);
  await title(page, "Browser notes");
  // The title is saved a moment after typing stops; the sidebar shows it once it is.
  await expect(page.locator(".tree-row .tree-title", { hasText: "Browser notes" })).toBeVisible();

  await page.reload();
  await expect(page.getByLabel("Page title")).toHaveValue("Browser notes");
  await expectBlocks(page, [{ kind: paragraph, text: "Kept in OPFS" }]);
  await expect(page.locator(".tree-row .tree-title", { hasText: "Browser notes" })).toBeVisible();
});

test("two tabs share the notes, and the second carries on when the first closes", async ({ page, context }) => {
  await openApp(page);
  await title(page, "Shared");
  await page.locator(".editor").click();
  await page.keyboard.type("From the first tab");
  await expectBlocks(page, [{ kind: paragraph, text: "From the first tab" }]);

  // The second tab sends its commands to the first, which holds the store.
  const second = await context.newPage();
  await second.goto("/");
  await second.locator(".tree-row .tree-title", { hasText: /^Shared$/ }).click();
  await expect(second.getByLabel("Page title")).toHaveValue("Shared");
  await expect.poll(() => texts(second)).toEqual(["From the first tab"]);

  // An edit in one tab shows in the other, as one from another device does.
  await second.locator(".editor").click();
  await second.keyboard.press("ControlOrMeta+End");
  await second.keyboard.type(", and the second");
  await expect.poll(() => texts(page)).toEqual(["From the first tab, and the second"]);
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.type("!");
  await expect.poll(() => texts(second)).toEqual(["From the first tab, and the second!"]);

  // A page made in one tab shows in the other's sidebar.
  await page.getByRole("button", { name: "New page" }).first().click();
  await title(page, "Made in the first");
  await expect(second.locator(".tree-row .tree-title", { hasText: "Made in the first" })).toBeVisible();

  // The first tab closes: the second takes the store over and goes on saving. (A command in
  // flight to the closed tab is refused, and the editor shows what was saved.)
  await page.close();
  await expect
    .poll(
      () =>
        second.evaluate(() =>
          window.drusiteWebCore!.handle({ type: "listPages" }).then(
            () => "answered",
            (e: { code?: string }) => e.code ?? "failed",
          ),
        ),
      { timeout: 20_000 },
    )
    .toBe("answered");
  await second.locator(".tree-row .tree-title", { hasText: /^Shared$/ }).click();
  await expect.poll(() => texts(second)).toEqual(["From the first tab, and the second!"]);
  await second.locator(".editor").click();
  await second.keyboard.press("ControlOrMeta+End");
  await second.keyboard.type(" Later.");
  await expectBlocks(second, [{ kind: paragraph, text: "From the first tab, and the second! Later." }]);
  await second.reload();
  await second.locator(".tree-row .tree-title", { hasText: /^Shared$/ }).click();
  await expect.poll(() => texts(second)).toEqual(["From the first tab, and the second! Later."]);
});

test("the app opens with no network, and keeps what's written", async ({ page, context }) => {
  await openApp(page);
  await page.keyboard.type("Written online");
  await expectBlocks(page, [{ kind: paragraph, text: "Written online" }]);
  // The service worker has kept the app's files.
  await page.evaluate(async () => {
    await navigator.serviceWorker.ready;
  });
  await expect.poll(() => page.evaluate(() => !!navigator.serviceWorker.controller)).toBe(true);

  await context.setOffline(true);
  await page.reload();
  await expect(page.locator(".editor")).toBeVisible();
  await expectBlocks(page, [{ kind: paragraph, text: "Written online" }]);
  await page.locator(".editor").click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.type(", then offline");
  await expectBlocks(page, [{ kind: paragraph, text: "Written online, then offline" }]);

  await context.setOffline(false);
  await page.reload();
  await expectBlocks(page, [{ kind: paragraph, text: "Written online, then offline" }]);
});

test("Settings show this browser's account, without the desktop's sections", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: /^Account:/ }).click();
  await page.getByRole("menu", { name: "Account" }).getByRole("menuitem", { name: "Settings" }).click();
  const sheet = page.getByRole("dialog", { name: "Settings" });
  await expect(sheet.getByText("Your notes are in this browser only.")).toBeVisible();
  await expect(sheet.getByLabel("Server address")).toHaveCount(0);
  const sections = sheet.getByRole("navigation", { name: "Settings sections" });
  await expect(sections.getByRole("button", { name: "Markdown folder" })).toHaveCount(0);
  await sections.getByRole("button", { name: "Import" }).click();
  await expect(sheet.getByText("Import a Notion export")).toBeVisible();
  await expect(sheet.getByText("Import from Notion's API")).toHaveCount(0);
});
