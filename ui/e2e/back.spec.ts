// SPDX-License-Identifier: AGPL-3.0-or-later

// The device's Back (Android's button or gesture; here the browser's, which is the same history)
// steps back through the app: what's open over the page first, then the pages before.

import { type Page, expect, test } from "@playwright/test";

import { openApp } from "./helpers";

const title = (page: Page) => page.getByLabel("Page title");

test.beforeEach(async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openApp(page);
});

test("on a phone, Back closes what's open, then goes back a page", async ({ page }) => {
  await title(page).fill("Alpha");
  await page.getByRole("button", { name: "New page", exact: true }).click();
  await expect(title(page)).toHaveValue("");
  await title(page).fill("Beta");

  // The pages drawer.
  await page.getByRole("button", { name: "Pages", exact: true }).last().click();
  const drawer = page.locator(".app.drawer-open");
  await expect(drawer).toHaveCount(1);
  await page.goBack();
  await expect(drawer).toHaveCount(0);
  await expect(title(page)).toHaveValue("Beta");

  // Search.
  await page.getByRole("button", { name: "Search", exact: true }).last().click();
  await expect(page.getByRole("dialog", { name: "Search" })).toBeVisible();
  await page.goBack();
  await expect(page.getByRole("dialog", { name: "Search" })).toHaveCount(0);
  await expect(title(page)).toHaveValue("Beta");

  // A menu.
  await page.getByRole("button", { name: "Page actions" }).click();
  await expect(page.getByRole("menu", { name: "Page actions" })).toBeVisible();
  await page.goBack();
  await expect(page.getByRole("menu", { name: "Page actions" })).toHaveCount(0);

  // Settings: a section goes back to the list, and the list to the page.
  await page.locator(".topbar-sync").click();
  const settings = page.getByRole("dialog", { name: "Settings" });
  await expect(settings).toHaveClass(/in-section/);
  await page.goBack();
  await expect(settings).toHaveClass(/at-home/);
  await page.goBack();
  await expect(settings).toHaveCount(0);
  await expect(title(page)).toHaveValue("Beta");

  // Then the page before.
  await page.goBack();
  await expect(title(page)).toHaveValue("Alpha");
});

test("what was closed with its own button doesn't take a Back", async ({ page }) => {
  await title(page).fill("Alpha");
  await page.getByRole("button", { name: "New page", exact: true }).click();
  await title(page).fill("Beta");

  await page.getByRole("button", { name: "Search", exact: true }).last().click();
  await page.getByRole("button", { name: "Cancel" }).click();
  await expect(page.getByRole("dialog", { name: "Search" })).toHaveCount(0);
  await page.getByRole("button", { name: "Page actions" }).click();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu", { name: "Page actions" })).toHaveCount(0);

  await page.goBack();
  await expect(title(page)).toHaveValue("Alpha");
});

test("a page picked from the drawer goes back to the one before, not to the drawer", async ({ page }) => {
  await title(page).fill("Alpha");
  await page.getByRole("button", { name: "New page", exact: true }).click();
  await title(page).fill("Beta");

  await page.getByRole("button", { name: "Pages", exact: true }).last().click();
  await page.locator(".tree-row .tree-title", { hasText: /^Alpha$/ }).click();
  await expect(title(page)).toHaveValue("Alpha");
  await expect(page.locator(".app.drawer-open")).toHaveCount(0);

  await page.goBack();
  await expect(title(page)).toHaveValue("Beta");
  await expect(page.locator(".app.drawer-open")).toHaveCount(0);
  await page.goBack();
  await expect(title(page)).toHaveValue("Alpha");
});
