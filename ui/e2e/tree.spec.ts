// SPDX-License-Identifier: AGPL-3.0-or-later

// The sidebar's page tree as a person uses it, checked after each step against the core's tree.

import { type Page, expect, test } from "@playwright/test";

import { expectBlocks, expectTree, leaf, openApp, openTitle } from "./helpers";

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
  await page.getByRole("button", { name: "+ New page" }).click();
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

  // Delete asks once, then deletes the page and everything inside it.
  await row(page, "Drusite").hover();
  const del = row(page, "Drusite").getByRole("button", { name: "Delete page" });
  await del.click();
  await expect(del).toHaveText("Delete?");
  await expectTree(page, [
    leaf("Inbox"),
    { title: "Projects", children: [{ title: "Drusite", children: [leaf("Ideas")] }] },
  ]);
  await del.click();
  await expectTree(page, [leaf("Inbox"), leaf("Projects")]);
  await expectOpen(page, "Inbox");

  // Deleting the open page opens another.
  await row(page, "Projects").click();
  await expectOpen(page, "Projects");
  const delProjects = row(page, "Projects").getByRole("button", { name: "Delete page" });
  await delProjects.click();
  await expect(delProjects).toHaveText("Delete?");
  await delProjects.click();
  await expectTree(page, [leaf("Inbox")]);
  await expectOpen(page, "Inbox");
});

test("dragging a page onto another nests it there", async ({ page }) => {
  await typeTitle(page, "A");
  await page.getByRole("button", { name: "+ New page" }).click();
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

  await page.getByRole("button", { name: "+ New page" }).click();
  await page.keyboard.type("Other");
  await expectOpen(page, "Other");
  await expectBlocks(page, [{ kind: { type: "paragraph" }, text: "" }]);

  await row(page, "Untitled").click();
  await expectOpen(page, "");
  await expectBlocks(page, [{ kind: { type: "todo", checked: true }, text: "Remember me", box: true, struck: true }]);
});
