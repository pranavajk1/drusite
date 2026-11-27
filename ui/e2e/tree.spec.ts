// SPDX-License-Identifier: AGPL-3.0-or-later

// The sidebar's page tree as a person uses it, checked after each step against the core's tree.

import { type Page, expect, test } from "@playwright/test";

import { expectBlocks, expectTree, leaf, openApp, openTitle, trashFromTree } from "./helpers";

/** The sidebar row of the page titled `title`. */
const row = (page: Page, title: string) =>
  page.locator(".tree-row").filter({ has: page.locator(".tree-title", { hasText: new RegExp(`^${title}$`) }) });

async function typeTitle(page: Page, title: string): Promise<void> {
  const input = page.getByLabel("Page title");
  await input.click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.type(title);
  await expect(input).toHaveValue(title);
}

async function expectOpen(page: Page, title: string): Promise<void> {
  await expect(page.getByLabel("Page title")).toHaveValue(title);
  await expect(page.locator(".tree-row.current .tree-title")).toHaveText(title || "Untitled");
  await expect.poll(() => openTitle(page)).toBe(title);
}

test.beforeEach(async ({ page }) => {
  await openApp(page);
});

test("pages are created, renamed, nested and deleted from the sidebar", async ({ page }) => {
  await expectTree(page, [leaf("Untitled")]);

  // Rename the first page by typing its title.
  await typeTitle(page, "Home");
  await expectTree(page, [leaf("Home")]);
  await expectOpen(page, "Home");

  // A new top-level page opens with its title ready for typing.
  await page.getByRole("button", { name: "New page", exact: true }).click();
  await expect(page.getByLabel("Page title")).toBeFocused();
  await expectTree(page, [leaf("Home"), leaf("Untitled")]);
  await page.keyboard.type("Projects");
  await expectTree(page, [leaf("Home"), leaf("Projects")]);
  await expectOpen(page, "Projects");

  // A page inside another.
  await row(page, "Projects").hover();
  await row(page, "Projects").getByRole("button", { name: "Add a page inside" }).click();
  await expect(page.getByLabel("Page title")).toBeFocused();
  await page.keyboard.type("Drusite");
  await expectTree(page, [leaf("Home"), { title: "Projects", children: [leaf("Drusite")] }]);
  await expectOpen(page, "Drusite");
  await expect(row(page, "Drusite")).toHaveAttribute("style", /--depth: 1/);

  // And one inside that.
  await row(page, "Drusite").hover();
  await row(page, "Drusite").getByRole("button", { name: "Add a page inside" }).click();
  await page.keyboard.type("Ideas");
  await expectTree(page, [
    leaf("Home"),
    { title: "Projects", children: [{ title: "Drusite", children: [leaf("Ideas")] }] },
  ]);

  // Renaming a page from elsewhere in the tree.
  await row(page, "Home").click();
  await expectOpen(page, "Home");
  await typeTitle(page, "Inbox");
  await expectTree(page, [
    leaf("Inbox"),
    { title: "Projects", children: [{ title: "Drusite", children: [leaf("Ideas")] }] },
  ]);

  // Folding a page in the sidebar hides its children; the core keeps them.
  const caret = row(page, "Projects").locator(".tree-caret");
  await expect(caret).toHaveAttribute("aria-expanded", "true");
  await caret.click();
  await expect(caret).toHaveAttribute("aria-expanded", "false");
  await expect(row(page, "Drusite")).toHaveCount(0);
  await expectOpen(page, "Inbox");
  await caret.click();
  await expect(row(page, "Drusite")).toBeVisible();

  // Delete moves the page, and everything inside it, to the trash; Undo brings it back.
  await row(page, "Drusite").hover();
  await trashFromTree(row(page, "Drusite"));
  await expectTree(page, [leaf("Inbox"), leaf("Projects")]);
  await expectOpen(page, "Inbox");
  await page.getByRole("status").getByRole("button", { name: "Undo" }).click();
  await expectOpen(page, "Drusite");
  await row(page, "Drusite").locator(".tree-caret").click();
  await expectTree(page, [
    leaf("Inbox"),
    { title: "Projects", children: [{ title: "Drusite", children: [leaf("Ideas")] }] },
  ]);

  // Deleting the open page opens another.
  await row(page, "Projects").click();
  await expectOpen(page, "Projects");
  await trashFromTree(row(page, "Projects"));
  await expectTree(page, [leaf("Inbox")]);
  await expectOpen(page, "Inbox");

  // The trash restores it where it was, or deletes it for good.
  await page.getByRole("button", { name: "Trash", exact: true }).click();
  const trash = page.getByRole("dialog", { name: "Trash" });
  await expect(trash.getByText("with 2 subpages")).toBeVisible();
  await trash.getByRole("button", { name: "Restore" }).click();
  await expect(row(page, "Projects")).toBeVisible();
  await expect(trash.getByText("The trash is empty.")).toBeVisible();
  await trash.getByRole("button", { name: "Close" }).click();
  await row(page, "Projects").hover();
  await trashFromTree(row(page, "Projects"));
  await page.getByRole("button", { name: "Trash", exact: true }).click();
  await trash.getByRole("button", { name: "Empty trash" }).click();
  await trash.getByRole("button", { name: "Yes, empty the trash" }).click();
  await expect(trash.getByText("The trash is empty.")).toBeVisible();
  await trash.getByRole("button", { name: "Close" }).click();
  await expectTree(page, [leaf("Inbox")]);
});

test("dragging a page onto another nests it there", async ({ page }) => {
  await typeTitle(page, "A");
  await page.getByRole("button", { name: "New page", exact: true }).click();
  await page.keyboard.type("B");
  await expectTree(page, [leaf("A"), leaf("B")]);

  await row(page, "B").dragTo(row(page, "A"));
  await expectTree(page, [{ title: "A", children: [leaf("B")] }]);

  // Onto the space below the tree: back to the top level, at the end.
  await row(page, "B").dragTo(page.locator(".tree-root-drop"), { targetPosition: { x: 10, y: 4 } });
  await expectTree(page, [leaf("A"), leaf("B")]);
});

test("what's typed on a page is still there after opening another and coming back", async ({ page }) => {
  await page.keyboard.type("[] Remember me");
  await page.locator(".todo-box").click();
  await expectBlocks(page, [{ kind: { type: "todo", checked: true }, text: "Remember me", box: true, struck: true }]);

  await page.getByRole("button", { name: "New page", exact: true }).click();
  await page.keyboard.type("Other");
  await expectOpen(page, "Other");
  await expectBlocks(page, [{ kind: { type: "paragraph" }, text: "" }]);

  await row(page, "Untitled").click();
  await expectOpen(page, "");
  await expectBlocks(page, [{ kind: { type: "todo", checked: true }, text: "Remember me", box: true, struck: true }]);
});
