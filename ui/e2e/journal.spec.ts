// SPDX-License-Identifier: AGPL-3.0-or-later

// The journal: today's entry, its mood, the journal page's views, the filter block and the
// settings, on the in-memory core.

import { expect, test } from "@playwright/test";

import { dayTitle, today } from "../src/journal/dates";
import { choose, openApp, openSettings } from "./helpers";

test("Today opens one entry per day, under the Journal page", async ({ page }) => {
  await openApp(page);
  const title = dayTitle(today());
  await page.getByRole("button", { name: "Today", exact: true }).click();
  await expect(page.getByLabel("Page title")).toHaveValue(title);
  const tree = page.locator(".sidebar .tree");
  await expect(tree.getByText("Journal", { exact: true })).toBeVisible();
  // A second press opens the same entry instead of making another.
  await page.getByRole("button", { name: "Today", exact: true }).click();
  await expect(tree.getByText(title)).toHaveCount(1);
});

test("a mood is picked, changed and cleared", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: "Today", exact: true }).click();
  const good = page.getByRole("radio", { name: "Good" });
  await good.click();
  await expect(good).toHaveAttribute("aria-checked", "true");
  await page.getByRole("radio", { name: "Great" }).click();
  await expect(good).toHaveAttribute("aria-checked", "false");
  await expect(page.getByRole("radio", { name: "Great" })).toHaveAttribute("aria-checked", "true");
  const mood = () => page.evaluate(() => window.drusiteMemoryCore!.handle({ type: "listPages" }));
  await expect
    .poll(async () => {
      const r = await mood();
      return r.type === "pages" ? r.pages.flatMap((p) => p.properties.filter((x) => x.name === "Mood").map((x) => x.value)) : [];
    })
    .toEqual([{ type: "number", value: 5 }]);
  await page.getByRole("radio", { name: "Great" }).click();
  await expect(page.getByRole("radio", { name: "Great" })).toHaveAttribute("aria-checked", "false");
});

test("the Journal page shows the calendar, and a day there opens its entry", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: "Today", exact: true }).click();
  await page.getByRole("radio", { name: "Okay" }).click();
  await page.getByRole("complementary", { name: "Sidebar" }).getByRole("button", { name: "Journal", exact: true }).click();
  const views = page.getByRole("region", { name: "Journal" });
  await expect(views).toBeVisible();
  await expect(views.getByRole("button", { name: /has an entry, Okay/ })).toBeVisible();
  await expect(views.getByText("1 day with a mood · average 3.0 of 5")).toBeVisible();
  await views.getByRole("button", { name: /has an entry/ }).click();
  await expect(page.getByLabel("Page title")).toHaveValue(dayTitle(today()));
});

test("a filter block lists matching pages and keeps its query", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: "Today", exact: true }).click();
  await page.getByRole("radio", { name: "Great" }).click();
  await page.locator(".editor").click();
  await page.keyboard.type("Notes");
  await page.keyboard.press("Enter");
  await page.keyboard.type("/filter ");
  const widget = page.locator(".query-widget");
  await expect(widget).toBeVisible();
  await expect(widget.getByRole("button", { name: `${dayTitle(today())} Great` })).toBeVisible();
  // A condition nothing passes empties the list.
  await widget.getByRole("button", { name: "Edit filter" }).click();
  await widget.getByRole("button", { name: "Add a condition" }).click();
  const value = widget.getByLabel("Value");
  await value.fill("1");
  await widget.getByLabel("Comparison").selectOption("lessOrEqual");
  await expect(widget.getByText("No pages match.")).toBeVisible();
  // The query is saved in the block.
  await expect
    .poll(() =>
      page.evaluate(() => {
        const core = window.drusiteMemoryCore!;
        return core
          .openPages()
          .flatMap((id) => core.blocks(id))
          .filter((b) => b.kind.type === "query")
          .map((b) => (b.kind.type === "query" ? JSON.parse(b.kind.query).conditions : null));
      }),
    )
    .toEqual([[{ property: "Mood", comparison: "lessOrEqual", value: { type: "number", value: 1 } }]]);
});

test("settings: a template fills new entries, and weeks can start on Sunday", async ({ page }) => {
  await openApp(page);
  await page.keyboard.type("How was {{weekday}}?");
  await page.getByLabel("Page title").fill("Template");
  const sheet = await openSettings(page, "Journal");
  await choose(page, sheet.getByLabel("New entries start from"), "Template");
  await choose(page, sheet.getByLabel("Weeks start on"), "Sunday");
  await sheet.getByRole("button", { name: "Close" }).click();
  await page.getByRole("button", { name: "Today", exact: true }).click();
  const weekday = dayTitle(today()).split(",")[0];
  await expect(page.locator(".editor")).toContainText(`How was ${weekday}?`);
  await page.getByRole("complementary", { name: "Sidebar" }).getByRole("button", { name: "Journal", exact: true }).click();
  await expect(page.locator(".jv-calendar th").first()).toHaveText("Sun");
});
