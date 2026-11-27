// SPDX-License-Identifier: AGPL-3.0-or-later

// Databases on the in-memory core: a task board made from the sidebar, cards added, dragged
// between columns and opened; a property added; the table, calendar and list views; a filter;
// and an inline database from the slash menu.

import { type Page, expect, test } from "@playwright/test";

import { openApp } from "./helpers";

async function newBoard(page: Page) {
  await openApp(page);
  await page.getByRole("button", { name: "New task board" }).click();
  await expect(page.getByRole("tab", { name: "Board" })).toHaveAttribute("aria-selected", "true");
}

function column(page: Page, name: string) {
  return page.locator(".board-col", { has: page.locator(".board-col-head", { hasText: name }) });
}

async function addCard(page: Page, col: string, title: string) {
  await column(page, col).getByRole("button", { name: "New" }).click();
  await expect(page.getByLabel("Page title")).toBeFocused();
  await page.keyboard.type(title);
  // Back to the board, through the breadcrumb.
  await page.locator(".breadcrumb .crumb").first().click();
  await expect(column(page, col).locator(".card", { hasText: title })).toBeVisible();
}

/** Drags with pointer events, a step at a time, as a hand would. */
async function drag(page: Page, from: string, toColumn: string) {
  const card = page.locator(".card", { hasText: from });
  const target = column(page, toColumn);
  const a = (await card.boundingBox())!;
  const b = (await target.boundingBox())!;
  await page.mouse.move(a.x + 20, a.y + 10);
  await page.mouse.down();
  for (let i = 1; i <= 8; i++) await page.mouse.move(a.x + 20 + ((b.x + 40 - a.x - 20) * i) / 8, a.y + 10 + ((b.y + 60 - a.y - 10) * i) / 8);
  await page.mouse.up();
}

test("a task board: cards are added to columns and dragged between them", async ({ page }) => {
  await newBoard(page);
  await addCard(page, "Not started", "Sow the seeds");
  await addCard(page, "In progress", "Water the beds");
  await drag(page, "Sow the seeds", "Done");
  await expect(column(page, "Done").locator(".card", { hasText: "Sow the seeds" })).toBeVisible();
  await expect(column(page, "Not started").locator(".card")).toHaveCount(0);
  // The core holds the new status.
  const statuses = await page.evaluate(async () => {
    const core = window.drusiteMemoryCore!;
    const pages = await core.handle({ type: "listPages" });
    if (pages.type !== "pages") return [];
    const db = pages.pages.find((p) => p.kind === "database")!;
    const r = await core.handle({ type: "queryView", database: db.id, view: null, clock: { now: "2026-11-27T12:00", utcOffset: 0 } });
    return r.type === "view" ? r.result.groups.filter((g) => g.rows.length).map((g) => `${g.label}: ${g.rows.map((x) => x.title).join(", ")}`) : [];
  });
  expect(statuses).toEqual(["In progress: Water the beds", "Done: Sow the seeds"]);
});

test("on a phone-width screen, holding a card at the edge scrolls the board to the next column", async ({ page }) => {
  await newBoard(page);
  await addCard(page, "Not started", "Sow the seeds");
  await page.setViewportSize({ width: 390, height: 844 });
  const view = page.viewportSize()!;
  const done = column(page, "Done");
  const card = page.locator(".card", { hasText: "Sow the seeds" });
  // The phone layout (with its tab bar) takes over first, and redraws the board.
  await expect(page.getByRole("navigation", { name: "Main" })).toBeVisible();
  // Shrunk from a desktop window, the pages drawer may have come along open.
  const menu = page.locator(".topbar-menu");
  if ((await menu.getAttribute("aria-expanded")) === "true") await menu.click();
  await expect(menu).toHaveAttribute("aria-expanded", "false");
  // Out of reach at first: a column fills most of a phone's width.
  expect((await done.boundingBox())!.x).toBeGreaterThan(view.width);
  const a = (await card.boundingBox())!;
  // Nothing (a drawer sliding away) is still over the card where the finger goes down.
  await expect
    .poll(() => page.evaluate(({ x, y }) => !!document.elementFromPoint(x, y)?.closest(".card"), { x: a.x + 20, y: a.y + 10 }))
    .toBe(true);
  await page.mouse.move(a.x + 20, a.y + 10);
  await page.mouse.down();
  for (let i = 1; i <= 8; i++) await page.mouse.move(a.x + 20 + ((view.width - 6 - a.x - 20) * i) / 8, a.y + 10);
  // Held at the edge, the board scrolls until Done comes into view.
  await expect.poll(async () => (await done.boundingBox())!.x, { timeout: 5000 }).toBeLessThan(view.width - 120);
  const b = (await done.boundingBox())!;
  await page.mouse.move(b.x + 40, b.y + 60, { steps: 4 });
  await page.mouse.up();
  await expect(done.locator(".card", { hasText: "Sow the seeds" })).toHaveCount(1);
  await expect(column(page, "Not started").locator(".card")).toHaveCount(0);
});

test("a card opens its page, whose properties edit in place", async ({ page }) => {
  await newBoard(page);
  await addCard(page, "Not started", "Order compost");
  await page.locator(".card", { hasText: "Order compost" }).click();
  await expect(page.getByLabel("Page title")).toHaveValue("Order compost");
  await page.getByRole("button", { name: /^Priority:/ }).click();
  await page.getByRole("option", { name: "High" }).click();
  await expect(page.locator(".row-props .chip", { hasText: "High" })).toBeVisible();
  // A new option, made by typing.
  await page.getByRole("button", { name: /^Priority:/ }).click();
  await page.getByPlaceholder("Search for an option…").fill("Someday");
  await page.keyboard.press("Enter");
  await expect(page.locator(".row-props .chip", { hasText: "Someday" })).toBeVisible();
});

test("a property is added, and the table, list and calendar show the rows", async ({ page }) => {
  await newBoard(page);
  await addCard(page, "Not started", "Mulch");
  await page.getByRole("tab", { name: "All tasks" }).click();
  await expect(page.locator(".db-table")).toBeVisible();
  await page.getByRole("button", { name: "Add a property" }).last().click();
  await page.getByLabel("Property name").fill("Points");
  await page.getByRole("button", { name: "Type" }).click();
  await page.getByRole("option", { name: "Number" }).click();
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await expect(page.locator(".db-table th", { hasText: "Points" })).toBeVisible();
  await page.getByRole("button", { name: "Points: Empty" }).click();
  await page.keyboard.type("5");
  await page.keyboard.press("Enter");
  await expect(page.locator(".db-table td", { hasText: "5" })).toBeVisible();

  // A filter that hides it, then a list view.
  await page.getByRole("button", { name: "Filter" }).click();
  await page.getByRole("button", { name: "Add a filter" }).click();
  await expect(page.locator(".db-foot")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Add a view" }).click();
  await page.getByRole("button", { name: "List" }).click();
  await expect(page.locator(".db-list-row", { hasText: "Mulch" })).toBeVisible();
  await page.getByRole("tab", { name: "Calendar" }).click();
  await expect(page.locator(".cal-grid")).toBeVisible();
});

test("a property chip goes into a formula where the cursor is, and typing carries on there", async ({ page }) => {
  await newBoard(page);
  await page.getByRole("tab", { name: "All tasks" }).click();
  await page.getByRole("button", { name: "Add a property" }).last().click();
  await page.getByLabel("Property name").fill("Double");
  await page.getByRole("button", { name: "Type" }).click();
  await page.getByRole("option", { name: "Formula" }).click();
  const formula = page.getByLabel("Formula");
  await formula.click();
  await page.keyboard.type("1 + ");
  await page.locator(".formula-props .chip", { hasText: "Status" }).click();
  await expect(formula).toBeFocused();
  await page.keyboard.type(" == 1");
  await expect(formula).toHaveValue('1 + prop("Status") == 1');
});

test("on a phone-width screen, the table's title column scrolls away with the others", async ({ page }) => {
  await newBoard(page);
  await addCard(page, "Not started", "Sow the seeds");
  await page.getByRole("tab", { name: "All tasks" }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole("navigation", { name: "Main" })).toBeVisible();
  const title = page.locator(".db-table th").first();
  const before = (await title.boundingBox())!.x;
  await page.locator(".table-scroll").evaluate((el) => (el.scrollLeft = 200));
  await expect.poll(async () => (await title.boundingBox())!.x).toBeLessThan(before - 150);
});

test("on a touch phone, a row's title opens it, with no Open button over it", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });
  const page = await context.newPage();
  await page.goto("/");
  await expect(page.locator(".editor")).toBeVisible();
  expect(await page.evaluate(() => matchMedia("(hover: none)").matches)).toBe(true);
  await page.getByRole("button", { name: "Pages" }).first().click();
  await page.getByRole("button", { name: "New task board" }).click();
  await page.getByRole("tab", { name: "All tasks" }).click();
  await page.getByRole("button", { name: "New", exact: true }).first().click();
  await expect(page.getByLabel("Page title")).toBeFocused();
  await page.keyboard.type("A rather long title for a narrow phone screen");
  // Back to the table through the pages drawer (a phone has no breadcrumb).
  await page.getByRole("button", { name: "Pages" }).first().click();
  await page.locator(".tree").getByText("Tasks", { exact: true }).click();
  const title = page.locator(".db-table .title-cell .cell-btn", { hasText: "A rather long title" });
  await expect(title).toBeVisible();
  await expect(page.locator(".db-table .open-row").first()).toBeHidden();
  await title.tap();
  await expect(page.getByLabel("Page title")).toHaveValue("A rather long title for a narrow phone screen");
  // Opened to read: no cursor in its text, so no keyboard comes up over it.
  await expect(page.locator(".editor")).not.toBeFocused();
  await expect(page.getByLabel("Page title")).not.toBeFocused();
  await context.close();
});

test("a task board goes inline in a page from the slash menu", async ({ page }) => {
  await openApp(page);
  await page.keyboard.type("/kanban");
  await page.keyboard.press("Enter");
  await expect(page.locator(".block-database .board")).toBeVisible();
  await expect(page.locator(".block-database .db-inline-title")).toHaveText("Tasks");
  // The database is a subpage, and its block says so.
  const kinds = await page.evaluate(() => [...document.querySelectorAll(".editor > [data-kind]")].map((e) => JSON.parse(e.getAttribute("data-kind")!).type));
  expect(kinds).toContain("database");
});
