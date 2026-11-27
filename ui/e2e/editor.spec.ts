// SPDX-License-Identifier: AGPL-3.0-or-later

// The editor as a person uses it: real clicks and keystrokes in a real browser engine, checked
// after each step against what the core saved.

import { expect, test } from "@playwright/test";

import { expectBlocks, lineKey, openApp, visibleTexts } from "./helpers";

const todo = (checked: boolean) => ({ type: "todo", checked }) as const;
const paragraph = { type: "paragraph" } as const;

test.beforeEach(async ({ page }) => {
  await openApp(page);
});

test("a to-do ticks and unticks with the mouse, again and again", async ({ page }) => {
  await page.keyboard.type("[] Buy milk");
  await expectBlocks(page, [{ kind: todo(false), text: "Buy milk", box: false, struck: false }]);

  const box = page.locator(".todo-box");
  for (const checked of [true, false, true, false, true]) {
    await box.click();
    // expectBlocks checks the box and the strike-through against the saved state too.
    await expectBlocks(page, [{ kind: todo(checked), text: "Buy milk", box: checked, struck: checked }]);
  }
  // The click doesn't take the cursor out of the text: typing carries on where it was.
  await page.keyboard.type("!");
  await expectBlocks(page, [{ kind: todo(true), text: "Buy milk!", box: true, struck: true }]);
});

test("Mod-Enter ticks and unticks the to-dos it's on", async ({ page }) => {
  await page.keyboard.type("[] One");
  await page.keyboard.press("ControlOrMeta+Enter");
  await expectBlocks(page, [{ kind: todo(true), text: "One", box: true, struck: true }]);
  await page.keyboard.press("ControlOrMeta+Enter");
  await expectBlocks(page, [{ kind: todo(false), text: "One", box: false, struck: false }]);

  // Enter after a to-do starts another, unticked; with both selected, both tick, then both untick.
  await page.keyboard.press("ControlOrMeta+Enter");
  await page.keyboard.press("Enter");
  await page.keyboard.type("Two");
  await expectBlocks(page, [
    { kind: todo(true), text: "One", box: true },
    { kind: todo(false), text: "Two", box: false },
  ]);
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.press("ControlOrMeta+Enter");
  await expectBlocks(page, [
    { kind: todo(true), text: "One", box: true, struck: true },
    { kind: todo(true), text: "Two", box: true, struck: true },
  ]);
  await page.keyboard.press("ControlOrMeta+Enter");
  await expectBlocks(page, [
    { kind: todo(false), text: "One", box: false, struck: false },
    { kind: todo(false), text: "Two", box: false, struck: false },
  ]);
});

test("a toggle folds and unfolds what's nested under it", async ({ page }) => {
  await page.keyboard.type("> Parent");
  await page.keyboard.press("Enter");
  await page.keyboard.press("Tab");
  await page.keyboard.type("Child");
  const blocks = [
    { kind: { type: "toggle" } as const, indent: 0, text: "Parent" },
    { kind: paragraph, indent: 1, text: "Child" },
  ];
  await expectBlocks(page, blocks);
  expect(await visibleTexts(page)).toEqual(["Parent", "Child"]);

  const arrow = page.locator(".toggle-arrow");
  await expect(arrow).toHaveAttribute("aria-expanded", "true");
  await arrow.click();
  await expect(arrow).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator(".editor > [data-kind]").nth(1)).toBeHidden();
  // Folding only hides: the core still has the child.
  await expectBlocks(page, blocks);
  expect(await visibleTexts(page)).toEqual(["Parent"]);
  // The cursor was in the child, so it moved to the end of the toggle.
  await page.keyboard.type("!");
  blocks[0]!.text = "Parent!";
  await expectBlocks(page, blocks);

  await arrow.click();
  await expect(arrow).toHaveAttribute("aria-expanded", "true");
  await expect(page.locator(".editor > [data-kind]").nth(1)).toBeVisible();
  expect(await visibleTexts(page)).toEqual(["Parent!", "Child"]);
  await expectBlocks(page, blocks);
});

const shortcuts = [
  { typed: "# ", kind: { type: "heading", level: "h1" }, tag: "H1" },
  { typed: "## ", kind: { type: "heading", level: "h2" }, tag: "H2" },
  { typed: "### ", kind: { type: "heading", level: "h3" }, tag: "H3" },
  { typed: "- ", kind: { type: "bulleted" }, tag: "DIV" },
  { typed: "* ", kind: { type: "bulleted" }, tag: "DIV" },
  { typed: "[] ", kind: { type: "todo", checked: false }, tag: "DIV" },
  { typed: "[x] ", kind: { type: "todo", checked: true }, tag: "DIV" },
  { typed: "1. ", kind: { type: "numbered" }, tag: "DIV" },
] as const;

for (const { typed, kind, tag } of shortcuts) {
  test(`typing "${typed}" at the start of a block makes it ${JSON.stringify(kind)}`, async ({ page }) => {
    await page.keyboard.type(`${typed}Text`);
    await expectBlocks(page, [{ kind, indent: 0, text: "Text" }]);
    expect(await page.locator(".editor > [data-kind]").evaluate((el) => el.tagName)).toBe(tag);
  });
}

test("Markdown shortcuts only work at the start of a block", async ({ page }) => {
  await page.keyboard.type("Not # a heading");
  await expectBlocks(page, [{ kind: paragraph, text: "Not # a heading" }]);
});

test("a numbered list counts, and Enter on an empty item ends it", async ({ page }) => {
  await page.keyboard.type("1. One");
  await page.keyboard.press("Enter");
  await page.keyboard.type("Two");
  await expectBlocks(page, [
    { kind: { type: "numbered" }, text: "One" },
    { kind: { type: "numbered" }, text: "Two" },
  ]);
  const items = page.locator(".editor > [data-kind]");
  await expect(items.nth(0)).toHaveAttribute("data-number", "1.");
  await expect(items.nth(1)).toHaveAttribute("data-number", "2.");

  await page.keyboard.press("Enter");
  await page.keyboard.press("Enter");
  await page.keyboard.type("After");
  await expectBlocks(page, [
    { kind: { type: "numbered" }, text: "One" },
    { kind: { type: "numbered" }, text: "Two" },
    { kind: paragraph, text: "After" },
  ]);
});

test("Enter splits a block and Backspace joins it back", async ({ page }) => {
  await page.keyboard.type("hello world");
  for (let i = 0; i < " world".length; i++) await page.keyboard.press("ArrowLeft");
  await page.keyboard.press("Enter");
  await expectBlocks(page, [
    { kind: paragraph, text: "hello" },
    { kind: paragraph, text: " world" },
  ]);
  // The cursor is at the start of the new block.
  await page.keyboard.press("Backspace");
  await expectBlocks(page, [{ kind: paragraph, text: "hello world" }]);

  // Enter at the end makes an empty block; Backspace in it removes it.
  await lineKey(page, "end");
  await page.keyboard.press("Enter");
  await expectBlocks(page, [
    { kind: paragraph, text: "hello world" },
    { kind: paragraph, text: "" },
  ]);
  await page.keyboard.press("Backspace");
  await expectBlocks(page, [{ kind: paragraph, text: "hello world" }]);
  await page.keyboard.type("!");
  await expectBlocks(page, [{ kind: paragraph, text: "hello world!" }]);
});

test("Backspace at the start of a list item makes it a paragraph before joining", async ({ page }) => {
  await page.keyboard.type("First");
  await page.keyboard.press("Enter");
  await page.keyboard.type("- Second");
  await lineKey(page, "start");
  await page.keyboard.press("Backspace");
  await expectBlocks(page, [
    { kind: paragraph, text: "First" },
    { kind: paragraph, text: "Second" },
  ]);
  await page.keyboard.press("Backspace");
  await expectBlocks(page, [{ kind: paragraph, text: "FirstSecond" }]);
});

test("Tab and Shift-Tab indent and outdent, one level past the block before at most", async ({ page }) => {
  await page.keyboard.type("- One");
  // The first block can't indent.
  await page.keyboard.press("Tab");
  await expectBlocks(page, [{ kind: { type: "bulleted" }, indent: 0, text: "One" }]);

  await page.keyboard.press("Enter");
  await page.keyboard.type("Two");
  await page.keyboard.press("Tab");
  await page.keyboard.press("Tab");
  await expectBlocks(page, [
    { kind: { type: "bulleted" }, indent: 0, text: "One" },
    { kind: { type: "bulleted" }, indent: 1, text: "Two" },
  ]);

  await page.keyboard.press("Enter");
  await page.keyboard.type("Three");
  await page.keyboard.press("Tab");
  await expectBlocks(page, [
    { kind: { type: "bulleted" }, indent: 0, text: "One" },
    { kind: { type: "bulleted" }, indent: 1, text: "Two" },
    { kind: { type: "bulleted" }, indent: 2, text: "Three" },
  ]);

  await page.keyboard.press("Shift+Tab");
  await page.keyboard.press("Shift+Tab");
  await page.keyboard.press("Shift+Tab");
  await expectBlocks(page, [
    { kind: { type: "bulleted" }, indent: 0, text: "One" },
    { kind: { type: "bulleted" }, indent: 1, text: "Two" },
    { kind: { type: "bulleted" }, indent: 0, text: "Three" },
  ]);
  // Tab keeps the focus in the editor.
  await expect(page.locator(".editor")).toBeFocused();
});

test("the slash menu turns an empty block into another kind, and filters as you type", async ({ page }) => {
  await page.keyboard.type("/");
  const menu = page.getByRole("listbox", { name: "Blocks" });
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("option")).toHaveCount(14);
  await page.keyboard.type("todo");
  await expect(menu.getByRole("option")).toHaveText(["To-do list[]"]);
  await page.keyboard.press("Enter");
  await expect(menu).toBeHidden();
  await page.keyboard.type("Buy milk");
  await expectBlocks(page, [{ kind: { type: "todo", checked: false }, text: "Buy milk" }]);
});

test("on a block with text, the slash menu's choice goes in a new block below", async ({ page }) => {
  await page.keyboard.type("Intro /");
  const menu = page.getByRole("listbox", { name: "Blocks" });
  await menu.getByRole("option", { name: "Heading 2" }).click();
  await page.keyboard.type("Next");
  await expectBlocks(page, [
    { kind: paragraph, text: "Intro " },
    { kind: { type: "heading", level: "h2" }, text: "Next" },
  ]);
});

test("Escape closes the slash menu and leaves the slash as text", async ({ page }) => {
  await page.keyboard.type("/");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("listbox", { name: "Blocks" })).toBeHidden();
  await page.keyboard.type("x");
  await expectBlocks(page, [{ kind: paragraph, text: "/x" }]);
});
