// SPDX-License-Identifier: AGPL-3.0-or-later

// Ask your notes, on the in-memory core (which fakes the server: it quotes the best-matching
// block and cites it, and reaches no model). The panel opens beside the page, an answer streams
// in, its citation opens the block, chats are kept and can be renamed and deleted, and journal
// entries stay out until Settings lets them in. On a phone the panel fills the screen.

import { expect, test } from "@playwright/test";

import { openApp, openSettings } from "./helpers";

test("an answer cites its block, and the citation opens it", async ({ page }) => {
  await openApp(page);
  await page.keyboard.type("Shopping list");
  await page.keyboard.press("Enter");
  await page.keyboard.type("The spare key is under the blue flowerpot");
  await page.getByLabel("Page title").fill("House");
  await page.getByRole("button", { name: "New page", exact: true }).click();
  await expect(page.getByLabel("Page title")).toBeFocused();

  await page.keyboard.press("ControlOrMeta+j");
  const panel = page.getByRole("region", { name: "Ask your notes" });
  await expect(panel).toBeVisible();
  await expect(panel.getByLabel("Question")).toBeFocused();
  await expect(panel).toContainText("Journal entries are left out.");
  await page.keyboard.type("Where is the spare key?");
  await page.keyboard.press("Enter");

  await expect(panel.locator(".ask-question")).toHaveText("Where is the spare key?");
  const answer = panel.locator(".ask-answer").last();
  await expect(answer).toContainText("From your notes: The spare key is under the blue flowerpot");
  await expect(panel.locator(".ask-title")).toHaveText("Where is the spare key?");
  // The [1] is a chip, and the sources list names the page.
  await expect(answer).not.toContainText("[1]");
  await expect(panel.getByRole("button", { name: "Source 1: House" })).toBeVisible();
  await expect(panel.locator(".ask-sources")).toContainText("House");

  await panel.getByRole("button", { name: "Source 1: House" }).click();
  await expect(page.getByLabel("Page title")).toHaveValue("House");
  // The cursor is in the cited block.
  await page.keyboard.type("» ");
  await expect(page.locator(".editor .block").nth(1)).toHaveText("» The spare key is under the blue flowerpot");
  // On a desktop the panel stays beside the page.
  await expect(panel).toBeVisible();
});

test("chats are kept, renamed and deleted", async ({ page }) => {
  await openApp(page);
  await page.keyboard.type("Bake sourdough at 230 degrees");
  await page.getByRole("button", { name: "Ask ⌘J" }).click();
  const panel = page.getByRole("region", { name: "Ask your notes" });
  await panel.getByLabel("Question").fill("What temperature for sourdough?");
  await panel.getByRole("button", { name: "Ask", exact: true }).click();
  await expect(panel.locator(".ask-answer").last()).toContainText("230 degrees");

  // A new chat, then back to the first from the list.
  await panel.getByRole("button", { name: "New chat" }).click();
  await expect(panel).toContainText("Ask a question about your notes");
  await panel.getByRole("button", { name: "Chats" }).click();
  const list = panel.getByRole("list", { name: "Chats" });
  await expect(list).toContainText("What temperature for sourdough?");
  await list.getByRole("button", { name: /What temperature/ }).click();
  await expect(panel.locator(".ask-answer").last()).toContainText("230 degrees");

  await panel.getByRole("button", { name: "Chats" }).click();
  await list.locator("li").first().hover();
  await list.getByRole("button", { name: "Chat options" }).click();
  await page.getByRole("menuitem", { name: "Rename" }).click();
  await panel.getByLabel("Chat name").fill("Bread");
  await page.keyboard.press("Enter");
  await expect(list).toContainText("Bread");

  await list.locator("li").first().hover();
  await list.getByRole("button", { name: "Chat options" }).click();
  await page.getByRole("menuitem", { name: "Delete" }).click();
  await list.getByRole("button", { name: "Delete" }).click();
  await expect(panel).toContainText("No chats yet.");

  await page.keyboard.press("Escape");
  await expect(panel).toBeHidden();
});

test("journal entries stay out until Settings lets them in", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: "Today" }).click();
  await page.locator(".editor").click();
  await page.keyboard.type("Saw a heron by the canal");
  await page.getByRole("button", { name: "Ask ⌘J" }).click();
  const panel = page.getByRole("region", { name: "Ask your notes" });
  await panel.getByLabel("Question").fill("Where did I see the heron?");
  await page.keyboard.press("Enter");
  await expect(panel.locator(".ask-answer").last()).toContainText("couldn't find");

  const sheet = await openSettings(page, "Journal");
  const toggle = sheet.getByRole("switch", { name: "Ask your notes reads the journal" });
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-checked", "true");
  await page.keyboard.press("Escape");

  await panel.getByRole("button", { name: "New chat" }).click();
  await expect(panel).toContainText("Journal entries are included.");
  await panel.getByLabel("Question").fill("Where did I see the heron?");
  await page.keyboard.press("Enter");
  await expect(panel.locator(".ask-answer").last()).toContainText("by the canal");
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  test("the panel fills the screen and gives way to a cited block", async ({ page }) => {
    await openApp(page);
    await page.keyboard.type("The wifi password is on the router");
    await page.getByRole("navigation", { name: "Main" }).getByRole("button", { name: "Ask" }).click();
    const panel = page.getByRole("region", { name: "Ask your notes" });
    const box = await panel.boundingBox();
    expect(box?.width).toBe(390);
    expect(box?.height).toBeGreaterThan(800);
    await panel.getByLabel("Question").fill("Where is the wifi password?");
    await panel.getByRole("button", { name: "Ask", exact: true }).click();
    await panel.getByRole("button", { name: /Source 1/ }).click();
    await expect(panel).toBeHidden();
    await page.keyboard.type("» ");
    await expect(page.locator(".editor .block").first()).toHaveText("» The wifi password is on the router");
  });
});
