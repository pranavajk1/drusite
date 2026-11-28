// SPDX-License-Identifier: AGPL-3.0-or-later

// A workspace can have no pages: deleting the last one leaves nothing open, not a new blank page.

import { type Page, expect, test } from "@playwright/test";

import { expectTree, fromAccountMenu, leaf, openApp, trashFromTree } from "./helpers";

const empty = (page: Page) => page.locator(".no-pages");

async function trashOnlyPage(page: Page): Promise<void> {
  const row = page.locator(".tree-row");
  await row.hover();
  await trashFromTree(row);
}

/** The trash holds exactly these titles. */
async function expectTrash(page: Page, titles: string[]): Promise<void> {
  // On a phone, the account menu is at the foot of the pages drawer.
  if (await page.locator(".tabbar").isVisible()) await page.getByRole("navigation", { name: "Main" }).getByRole("button", { name: "Pages" }).click();
  await fromAccountMenu(page, "Trash");
  const trash = page.getByRole("dialog", { name: "Trash" });
  await expect(trash.locator(".trash-title > span")).toHaveText(titles);
  await trash.getByRole("button", { name: "Close" }).click();
  await expect(trash).toHaveCount(0);
}

test.beforeEach(async ({ page }) => {
  await openApp(page);
});

test("deleting the last page leaves no pages, and New page starts again", async ({ page }) => {
  await expectTree(page, [leaf("Untitled")]);
  await trashOnlyPage(page);

  await expect(empty(page)).toBeVisible();
  await expect(empty(page)).toContainText("No pages yet");
  await expectTree(page, []);
  await expect(page.locator(".editor")).toHaveCount(0);
  // Without a page, the top bar has nothing of a page's.
  await expect(page.getByRole("navigation", { name: "Breadcrumb" })).toHaveCount(0);
  await expect(page.locator(".topbar-share")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Page actions" })).toHaveCount(0);
  await expect(page.locator("[role=status]").getByRole("button", { name: "Undo" })).toBeVisible();

  // Only the one page went to the trash; nothing new was made.
  await expectTrash(page, ["Untitled"]);
  await expectTree(page, []);

  // The buttons' icons take their button's colour; only the large one above is greyed.
  for (const name of ["New page", "Today"]) {
    const button = empty(page).getByRole("button", { name });
    const [text, icon] = await button.evaluate((b) => [getComputedStyle(b).color, getComputedStyle(b.querySelector("svg")!).color]);
    expect(icon, name).toBe(text);
  }

  await empty(page).getByRole("button", { name: "New page" }).click();
  await expect(page.getByLabel("Page title")).toBeFocused();
  await page.keyboard.type("Fresh");
  await expectTree(page, [leaf("Fresh")]);
  await expect(empty(page)).toHaveCount(0);
  await expect(page.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Fresh");
});

test("Undo after deleting the last page opens it again", async ({ page }) => {
  await trashOnlyPage(page);
  await expect(empty(page)).toBeVisible();
  await page.getByRole("status").getByRole("button", { name: "Undo" }).click();
  await expect(page.locator(".editor")).toBeVisible();
  await expectTree(page, [leaf("Untitled")]);
  await expectTrash(page, []);
});

test("Today from no pages opens today's entry", async ({ page }) => {
  await trashOnlyPage(page);
  await empty(page).getByRole("button", { name: "Today" }).click();
  await expect(page.locator(".editor")).toBeVisible();
  await expect(empty(page)).toHaveCount(0);
  await expect(page.locator(".tree-row.current")).toHaveCount(1);
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  test("deleting the last page leaves no pages, and the tab bar's New page works", async ({ page }) => {
    await page.getByRole("button", { name: "Page actions" }).click();
    await page.getByRole("menu", { name: "Page actions" }).getByRole("menuitem", { name: "Move to Trash" }).click();

    await expect(empty(page)).toBeVisible();
    await expect(empty(page).getByRole("button", { name: "New page" })).toBeInViewport();
    await expect(page.getByRole("button", { name: "Page actions" })).toHaveCount(0);
    await expectTree(page, []);

    await expectTrash(page, ["Untitled"]);
    await expect(empty(page)).toBeVisible();

    await page.getByRole("navigation", { name: "Main" }).getByRole("button", { name: "New page" }).click();
    await expect(page.getByLabel("Page title")).toBeFocused();
    await page.keyboard.type("Phone");
    await expectTree(page, [leaf("Phone")]);
    await expect(empty(page)).toHaveCount(0);
  });

  test("the tab bar's Today works with no pages", async ({ page }) => {
    await page.getByRole("button", { name: "Page actions" }).click();
    await page.getByRole("menu", { name: "Page actions" }).getByRole("menuitem", { name: "Move to Trash" }).click();
    await expect(empty(page)).toBeVisible();
    await page.getByRole("navigation", { name: "Main" }).getByRole("button", { name: "Today" }).click();
    await expect(page.locator(".editor")).toBeVisible();
    await expect(empty(page)).toHaveCount(0);
  });
});
