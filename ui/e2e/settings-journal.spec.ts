// SPDX-License-Identifier: AGPL-3.0-or-later

// Settings and the journal. A switch's description says what it does and where it applies, never
// whether it's on (the switch shows that), so it reads the same either way. New journal entries
// start from an empty page or an ordinary page, named with the pages it's inside, never the
// journal page or an entry. The sidebar's "Journals" isn't drawn like the "Journal" page in the
// tree.

import { type Locator, type Page, expect, test } from "@playwright/test";

import { makePage, openApp, openSettings, wasm } from "./helpers";

const STATE = /^\s*(on|off|enabled|disabled)\b/i;

/** The description next to a switch, before and after flipping it: the same, with no state word. */
async function sameEitherWay(row: Locator, desc: Locator) {
  const toggle = row.getByRole("switch");
  const before = (await desc.textContent())!.trim();
  expect(before).not.toMatch(STATE);
  const was = await toggle.getAttribute("aria-checked");
  await toggle.click();
  await expect(toggle).not.toHaveAttribute("aria-checked", was!);
  expect((await desc.textContent())!.trim()).toBe(before);
}

test("switches in Settings and Share say what they do, not whether they're on", async ({ page }) => {
  // Publishing needs a server; the in-memory core plays one.
  test.skip(wasm(), "publishing needs a server");
  await openApp(page);
  await page.getByLabel("Page title").fill("Recipes");
  const sheet = await openSettings(page, "Journal");
  const row = sheet.locator(".set-row").filter({ has: page.getByRole("switch", { name: "Ask your notes reads the journal" }) });
  await expect(row.locator(".set-desc")).toHaveText("Lets answers use journal entries. For this account, on every device.");
  await sameEitherWay(row, row.locator(".set-desc"));
  await page.keyboard.press("Escape");
  await expect(sheet).toBeHidden();

  await page.getByRole("button", { name: "Share", exact: true }).click();
  const pop = page.getByRole("dialog", { name: "Share" });
  await pop.getByRole("switch", { name: "Publish to web" }).click();
  for (const name of ["Include subpages", "Search engines", "Link expires", "Password"]) {
    const option = pop.locator(".share-row").filter({ has: page.getByRole("switch", { name }) });
    if (name === "Password") {
      // Turning it on asks for the password first; the words above don't change meanwhile.
      const desc = option.locator(".share-desc");
      const before = await desc.textContent();
      expect(before).not.toMatch(STATE);
      await option.getByRole("switch").click();
      await expect(desc).toHaveText(before!);
    } else {
      await sameEitherWay(option, option.locator(".share-desc"));
    }
  }
});

/** The options of the template picker, as listed. */
async function templateOptions(page: Page, sheet: Locator): Promise<string[]> {
  await sheet.getByLabel("New entries start from").click();
  const options = await page.getByRole("listbox").getByRole("option").allTextContents();
  await page.keyboard.press("Escape");
  return options.map((o) => o.trim());
}

test("new entries start from an empty page or an ordinary one, never the journal or an entry", async ({ page }) => {
  await openApp(page);
  await page.getByLabel("Page title").fill("Templates");
  // A page inside it, and a journal entry (which makes the Journal page).
  await page.getByRole("treeitem", { name: "Templates" }).hover();
  await makePage(page, page.getByRole("treeitem", { name: "Templates" }).getByRole("button", { name: "Add a page inside" }));
  await page.getByLabel("Page title").fill("Daily");
  await page.keyboard.press("Enter");
  await page.keyboard.type("Grateful for {{weekday}}");
  await page.getByRole("button", { name: "Today", exact: true }).click();
  await expect(page.getByRole("radio", { name: "Okay" })).toBeVisible();

  const sheet = await openSettings(page, "Journal");
  await expect(sheet.getByLabel("New entries start from")).toHaveText("Empty page");
  const options = await templateOptions(page, sheet);
  expect(options).toEqual(["Empty page", "Templates", "Templates / Daily"]);

  await sheet.getByLabel("New entries start from").click();
  await page.getByRole("listbox").getByRole("option", { name: "Templates / Daily", exact: true }).click();
  await expect(sheet.getByLabel("New entries start from")).toHaveText("Templates / Daily");

  // The core holds to the same rule: the journal page itself is refused.
  const refused = await page.evaluate(async () => {
    const core = (window.drusiteMemoryCore ?? window.drusiteWebCore)!;
    const journal = (await core.handle({ type: "settings" })) as { settings: { journalParent: string } };
    try {
      await core.handle({ type: "setSetting", setting: { type: "journalTemplate", value: journal.settings.journalParent } });
      return false;
    } catch {
      return true;
    }
  });
  expect(refused).toBe(true);
});

test("the sidebar's Journals isn't drawn like the Journal page in the tree", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: "Today", exact: true }).click();
  const sidebar = page.getByRole("complementary", { name: "Sidebar" });
  const nav = sidebar.getByRole("button", { name: "Journals", exact: true });
  await expect(nav).toBeVisible();
  await expect(sidebar.getByRole("button", { name: "Journal", exact: true })).toHaveCount(0);
  const tree = sidebar.getByRole("treeitem", { name: "Journal" });
  await expect(tree).toBeVisible();
  const icon = (l: Locator) => l.locator("svg").first().innerHTML();
  expect(await icon(nav)).not.toBe(await icon(tree));

  await nav.click();
  await expect(page.getByRole("region", { name: "Journal" })).toBeVisible();
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  test("the drawer names it Journals too", async ({ page }) => {
    await openApp(page);
    await page.getByRole("navigation", { name: "Main" }).getByRole("button", { name: "Today" }).click();
    await page.getByRole("navigation", { name: "Main" }).getByRole("button", { name: "Pages" }).click();
    const sidebar = page.getByRole("complementary", { name: "Sidebar" });
    await expect(sidebar.getByRole("button", { name: "Journals", exact: true })).toBeVisible();
    await sidebar.getByRole("button", { name: "Journals", exact: true }).click();
    await expect(page.getByRole("region", { name: "Journal" })).toBeVisible();
  });
});
