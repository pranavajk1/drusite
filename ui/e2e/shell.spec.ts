// SPDX-License-Identifier: AGPL-3.0-or-later

// The app's frame: the account menu at the foot of the sidebar, and the sidebar closing and
// resizing.

import { expect, test } from "@playwright/test";

import { fromAccountMenu, openApp } from "./helpers";

test.beforeEach(async ({ page }) => {
  await openApp(page);
});

test("the account menu says who and where, and leads to Settings, the trash and signing in", async ({ page }) => {
  const button = page.getByRole("button", { name: /^Account:/ });
  await button.click();
  const menu = page.getByRole("menu", { name: "Account" });
  await expect(menu.getByText("Not signed in")).toBeVisible();
  await expect(button).toContainText("Not signed in");
  await expect(menu.getByRole("menuitem")).toHaveText([/Settings/, /Trash/, /Sign in to sync/]);
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();

  await fromAccountMenu(page, "Trash");
  await expect(page.getByRole("dialog", { name: "Trash" })).toBeVisible();
  await page.getByRole("dialog", { name: "Trash" }).getByRole("button", { name: "Close" }).click();

  await fromAccountMenu(page, "Sign in to sync");
  await expect(page.getByRole("dialog", { name: "Settings" }).getByRole("heading", { name: "Account" })).toBeVisible();
});

test("the sidebar closes and opens, from its button and with the keyboard", async ({ page }) => {
  const sidebar = page.getByRole("complementary", { name: "Sidebar" });
  await page.getByRole("button", { name: "Close sidebar" }).click();
  await expect(sidebar).toBeHidden();
  await page.getByRole("button", { name: "Open sidebar" }).click();
  await expect(sidebar).toBeVisible();
  await page.keyboard.press("ControlOrMeta+Backslash");
  await expect(sidebar).toBeHidden();
  // It stays closed after a reload.
  await page.reload();
  await expect(page.getByRole("button", { name: "Open sidebar" })).toBeVisible();
  await page.keyboard.press("ControlOrMeta+Backslash");
  await expect(sidebar).toBeVisible();
});

test("dragging the sidebar's edge resizes it, within limits, and the width is kept", async ({ page }) => {
  const sidebar = page.getByRole("complementary", { name: "Sidebar" });
  const edge = page.getByRole("separator", { name: "Resize sidebar" });
  const width = async () => (await sidebar.boundingBox())!.width;
  expect(Math.round(await width())).toBe(248);

  const drag = async (dx: number) => {
    const b = (await edge.boundingBox())!;
    await page.mouse.move(b.x + b.width / 2, b.y + 200);
    await page.mouse.down();
    await page.mouse.move(b.x + b.width / 2 + dx / 2, b.y + 200);
    await page.mouse.move(b.x + b.width / 2 + dx, b.y + 200);
    await page.mouse.up();
  };
  await drag(100);
  await expect.poll(width).toBeCloseTo(348, 0);
  await drag(400);
  await expect.poll(width).toBeCloseTo(480, 0);
  await page.reload();
  await expect.poll(width).toBeCloseTo(480, 0);

  // With the keyboard, too.
  await edge.focus();
  await page.keyboard.press("ArrowLeft");
  await expect.poll(width).toBeCloseTo(464, 0);

  // Dragging well past the narrowest closes it; a click on the edge does too.
  await drag(-450);
  await expect(sidebar).toBeHidden();
  await page.getByRole("button", { name: "Open sidebar" }).click();
  await expect.poll(width).toBeCloseTo(464, 0);
  await edge.click();
  await expect(sidebar).toBeHidden();
});
