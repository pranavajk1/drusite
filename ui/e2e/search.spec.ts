// SPDX-License-Identifier: AGPL-3.0-or-later

// Quick find: ⌘K / Ctrl-K, results as you type, and a hit opens its page at the block.

import { expect, test } from "@playwright/test";

import { openApp } from "./helpers";

test("search finds a block on another page and opens it there", async ({ page }) => {
  await openApp(page);
  await page.keyboard.type("Groceries: eggs, oat milk");
  await page.keyboard.press("Enter");
  await page.keyboard.type("Call the plumber about the heron-shaped tap");
  await page.getByLabel("Page title").fill("Errands");
  await page.getByRole("button", { name: "+ New page" }).click();
  await expect(page.getByLabel("Page title")).toBeFocused();

  await page.keyboard.press("ControlOrMeta+k");
  const box = page.getByRole("dialog", { name: "Search" });
  await expect(box.getByLabel("Search your pages")).toBeFocused();
  await page.keyboard.type("plumb");
  const hit = box.getByRole("option").first();
  await expect(hit).toContainText("Errands");
  await expect(hit).toContainText("Call the plumber");
  await page.keyboard.press("Enter");
  await expect(box).toBeHidden();
  await expect(page.getByLabel("Page title")).toHaveValue("Errands");
  // The cursor is in the block that matched.
  await page.keyboard.type("» ");
  await expect(page.locator(".editor .block").nth(1)).toHaveText("» Call the plumber about the heron-shaped tap");
});

test("search says when nothing matches, and Escape closes it", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: "🔍 Search" }).click();
  const box = page.getByRole("dialog", { name: "Search" });
  await page.keyboard.type("zebra");
  await expect(box.getByText("Nothing matches “zebra”.")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(box).toBeHidden();
});
