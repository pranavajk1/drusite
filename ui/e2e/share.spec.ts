// SPDX-License-Identifier: AGPL-3.0-or-later

// Share to web, on the in-memory core (which keeps share state as the server would). The top
// bar's Share opens a popover: publish, copy the link, each option, a new link, unpublish. The
// page tree marks a published page. On a phone it's a sheet from the page actions.

import { expect, test } from "@playwright/test";

import { openApp } from "./helpers";

test("a page is published, its options set, its link renewed and taken down", async ({ page, context, browserName }) => {
  if (browserName === "chromium") await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await openApp(page);
  await page.getByLabel("Page title").fill("Recipes");

  const share = page.getByRole("button", { name: "Share", exact: true });
  await share.click();
  const pop = page.getByRole("dialog", { name: "Share" });
  await expect(pop).toBeVisible();
  await expect(pop).toContainText("Share a read-only link anyone can open.");
  await expect(pop.getByRole("textbox", { name: "Public link" })).toHaveCount(0);

  await pop.getByRole("switch", { name: "Publish to web" }).click();
  await expect(pop.getByRole("switch", { name: "Publish to web" })).toHaveAttribute("aria-checked", "true");
  const link = pop.getByRole("textbox", { name: "Public link" });
  await expect(link).toHaveValue(/^https:\/\/pages\.example\.test\/p\/[A-Za-z0-9_-]{43}$/);
  const first = await link.inputValue();
  await expect(pop).toContainText("Anyone with the link can read “Recipes”.");

  // The tree and the top bar show it.
  const row = page.getByRole("treeitem").filter({ hasText: "Recipes" });
  await expect(row.getByRole("img", { name: "Published to web" })).toBeVisible();
  await expect(share).toHaveClass(/published/);

  await pop.getByRole("button", { name: "Copy link" }).click();
  await expect(pop.getByRole("button", { name: "Copied" })).toBeVisible();
  if (browserName === "chromium") expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(first);

  // Every option is off until turned on.
  for (const name of ["Include subpages", "Search engines", "Link expires", "Password"]) {
    await expect(pop.getByRole("switch", { name })).toHaveAttribute("aria-checked", "false");
  }
  await pop.getByRole("switch", { name: "Include subpages" }).click();
  await expect(pop.getByRole("switch", { name: "Include subpages" })).toHaveAttribute("aria-checked", "true");
  await pop.getByRole("switch", { name: "Search engines" }).click();
  await expect(pop.getByRole("switch", { name: "Search engines" })).toHaveAttribute("aria-checked", "true");

  await pop.getByRole("switch", { name: "Link expires" }).click();
  const date = pop.getByLabel("Last day");
  await expect(date).toBeVisible();
  await pop.getByRole("switch", { name: "Link expires" }).click();
  await expect(date).toHaveCount(0);
  await expect(pop).toContainText("The link stops working after a day you pick.");

  await pop.getByRole("switch", { name: "Password" }).click();
  const pw = pop.getByLabel("Password", { exact: true }).and(pop.locator("input"));
  await expect(pw).toBeFocused();
  await pw.fill("abc");
  await pop.getByRole("button", { name: "Save" }).click();
  await expect(pop).toContainText("A password needs at least 4 characters.");
  await pw.fill("open sesame");
  await pop.getByRole("button", { name: "Save" }).click();
  await expect(pop).toContainText("Password set");
  await pop.getByRole("switch", { name: "Password" }).click();
  await expect(pop).toContainText("Readers need a password to open it.");

  // A new link, confirmed: the old one is replaced.
  await pop.getByRole("button", { name: "New link" }).click();
  await expect(pop).toContainText("The current link will stop working.");
  await pop.getByRole("button", { name: "Make a new link" }).click();
  await expect(link).not.toHaveValue(first);
  await expect(link).toHaveValue(/\/p\/[A-Za-z0-9_-]{43}$/);

  // Escape closes it and gives the focus back; it opens again as it was.
  await page.keyboard.press("Escape");
  await expect(pop).toHaveCount(0);
  await expect(share).toBeFocused();
  await share.click();
  await expect(page.getByRole("dialog", { name: "Share" }).getByRole("switch", { name: "Include subpages" })).toHaveAttribute(
    "aria-checked",
    "true",
  );

  // Unpublish takes it down; the mark goes.
  await page.getByRole("dialog", { name: "Share" }).getByRole("button", { name: "Unpublish" }).click();
  await expect(page.getByRole("dialog", { name: "Share" }).getByRole("switch", { name: "Publish to web" })).toHaveAttribute(
    "aria-checked",
    "false",
  );
  await expect(row.getByRole("img", { name: "Published to web" })).toHaveCount(0);

  // A click outside closes it.
  await page.getByLabel("Page title").click();
  await expect(page.getByRole("dialog", { name: "Share" })).toHaveCount(0);
});

test("on a phone, Share is a sheet from the page actions", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openApp(page);
  await page.getByLabel("Page title").fill("Trip");
  await expect(page.getByRole("button", { name: "Share", exact: true })).toBeHidden();
  await page.getByRole("button", { name: "Page actions" }).click();
  await page.getByRole("menuitem", { name: "Share to web" }).click();
  const sheet = page.getByRole("dialog", { name: "Share" });
  await expect(sheet).toBeVisible();
  await expect(sheet).toHaveClass(/phone-sheet/);
  await sheet.getByRole("switch", { name: "Publish to web" }).click();
  await expect(sheet.getByRole("textbox", { name: "Public link" })).toHaveValue(/\/p\//);
  const box = await sheet.boundingBox();
  expect(box!.width).toBeLessThanOrEqual(390);
  await sheet.getByRole("button", { name: "Close" }).click();
  await expect(sheet).toHaveCount(0);
  await page.getByRole("button", { name: "Page actions" }).click();
  await expect(page.getByRole("menuitem", { name: "Published to web" })).toBeVisible();
});
