// SPDX-License-Identifier: AGPL-3.0-or-later

// What the page shows and what the in-memory core holds, read the same way so a test can compare
// them. The dev build puts the core on `window.drusiteMemoryCore` (see `src/main.ts`).

import { type Locator, type Page, expect } from "@playwright/test";

import type { MemoryCore } from "../src/core/memory";
import type { BlockKind } from "../src/core/types";

export interface ShownBlock {
  kind: BlockKind;
  indent: number;
  text: string;
  /** A to-do's checkbox as the page shows it, null for other kinds. */
  box: boolean | null;
  /** Whether the text is drawn struck through. */
  struck: boolean;
}

export interface TreeNode {
  title: string;
  children: TreeNode[];
}

declare global {
  interface Window {
    drusiteMemoryCore?: MemoryCore;
  }
}

/** Opens the app on a fresh in-memory core, with the cursor in the body of its one page. */
export async function openApp(page: Page): Promise<void> {
  await page.goto("/");
  await expect(page.locator(".editor")).toBeVisible();
  // A new page focuses its (empty) title; Enter there goes to the body.
  await expect(page.getByLabel("Page title")).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.locator(".editor")).toBeFocused();
}

/** The blocks of the open page as the browser draws them. */
function shownBlocks(page: Page): Promise<(ShownBlock & { id: string | null; hidden: boolean })[]> {
  return page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>(".editor > [data-kind]")].map((el) => {
      const text = el.querySelector<HTMLElement>(":scope > .block-text, :scope > code") ?? el;
      const box = el.querySelector<HTMLInputElement>(":scope > input.todo-box");
      return {
        id: el.getAttribute("data-block-id"),
        kind: JSON.parse(el.getAttribute("data-kind")!) as BlockKind,
        indent: Number(el.getAttribute("data-indent")),
        text: text.textContent ?? "",
        box: box ? box.checked : null,
        struck: getComputedStyle(text).textDecorationLine.includes("line-through"),
        hidden: !el.checkVisibility(),
      };
    }),
  );
}

/** The blocks of the open page as the core holds them, or a reason it can't say yet. */
function heldBlocks(page: Page): Promise<(ShownBlock & { id: string })[] | string> {
  return page.evaluate(() => {
    const core = window.drusiteMemoryCore;
    if (!core) return "no in-memory core on the page";
    const open = core.openPages();
    if (open.length !== 1) return `${open.length} pages open`;
    return core.blocks(open[0]!).map((b) => {
      const checked = b.kind.type === "todo" && b.kind.checked;
      return {
        id: b.id,
        kind: b.kind,
        indent: b.indent,
        text: b.spans.map((s) => s.text).join(""),
        box: b.kind.type === "todo" ? b.kind.checked : null,
        struck: checked,
      };
    });
  });
}

/**
 * Waits until the page and the core agree block for block (ID, kind, indent, text, and for
 * to-dos the checkbox and the strike-through) and expects that to be `expected`.
 */
export async function expectBlocks(page: Page, expected: Partial<ShownBlock>[]): Promise<void> {
  await expect
    .poll(async () => {
      const [shown, held] = [await shownBlocks(page), await heldBlocks(page)];
      const page_ = shown.map(({ hidden: _, ...b }) => b);
      if (typeof held === "string" || JSON.stringify(page_) !== JSON.stringify(held)) return { page: page_, core: held };
      return held.map(({ id: _, ...b }) => b);
    })
    .toEqual(expected.map((b) => expect.objectContaining(b)));
}

/** The text of the blocks the page shows, leaving out those folded away. */
export async function visibleTexts(page: Page): Promise<string[]> {
  return (await shownBlocks(page)).filter((b) => !b.hidden).map((b) => b.text);
}

/** The page tree as the sidebar shows it. */
function shownTree(page: Page): Promise<TreeNode[]> {
  return page.evaluate(() => {
    const walk = (ul: Element | null): TreeNode[] =>
      ul === null
        ? []
        : [...ul.querySelectorAll(":scope > li")].map((li) => ({
            title: li.querySelector(":scope > .tree-row .tree-title")!.textContent ?? "",
            children: walk(li.querySelector(":scope > ul[role=group]")),
          }));
    return walk(document.querySelector("nav.tree > ul[role=tree]"));
  });
}

/** The page tree as the core holds it, titled as the sidebar titles it. */
function heldTree(page: Page): Promise<TreeNode[] | string> {
  return page.evaluate(async () => {
    const core = window.drusiteMemoryCore;
    if (!core) return "no in-memory core on the page";
    const res = await core.handle({ type: "listPages" });
    if (res.type !== "pages") return `listPages answered ${res.type}`;
    const byId = new Map(res.pages.map((p) => [p.id, p]));
    const node = (id: string): TreeNode => {
      const p = byId.get(id)!;
      return { title: p.title || "Untitled", children: p.children.map(node) };
    };
    return res.pages.filter((p) => p.parent === null).map((p) => node(p.id));
  });
}

/**
 * Waits until the sidebar and the core agree on the whole tree (so every page with children must
 * be unfolded) and expects that to be `expected`.
 */
export async function expectTree(page: Page, expected: TreeNode[]): Promise<void> {
  await expect
    .poll(async () => {
      const [shown, held] = [await shownTree(page), await heldTree(page)];
      if (JSON.stringify(shown) !== JSON.stringify(held)) return { sidebar: shown, core: held };
      return held;
    })
    .toEqual(expected);
}

/** The title of the page the core has open, once exactly one is. */
export async function openTitle(page: Page): Promise<string | null> {
  return page.evaluate(async () => {
    const core = window.drusiteMemoryCore!;
    const open = core.openPages();
    if (open.length !== 1) return null;
    const res = await core.handle({ type: "listPages" });
    return res.type === "pages" ? (res.pages.find((p) => p.id === open[0])?.title ?? null) : null;
  });
}

export const leaf = (title: string): TreeNode => ({ title, children: [] });

/**
 * Moves the cursor to the start or end of the line. WebKit on a Mac does what Safari does there:
 * Home and End scroll the page rather than move the cursor, so it takes Cmd-Left and Cmd-Right.
 */
export async function lineKey(page: Page, to: "start" | "end"): Promise<void> {
  const mac = await page.evaluate(() => /Mac/.test(navigator.platform));
  const key = to === "start" ? (mac ? "Meta+ArrowLeft" : "Home") : mac ? "Meta+ArrowRight" : "End";
  await page.keyboard.press(key);
}

/** Picks `option` in one of the app's selects (`Select.svelte`): opens it, clicks the option. */
export async function choose(page: Page, select: Locator, option: string) {
  await select.click();
  await page.getByRole("listbox").getByRole("option", { name: option, exact: true }).click();
}

/** Opens Settings at `section` (its name in the left-hand list). */
export async function openSettings(page: Page, section: string) {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const sheet = page.getByRole("dialog", { name: "Settings" });
  await sheet.getByRole("navigation", { name: "Settings sections" }).getByRole("button", { name: section }).click();
  return sheet;
}

/** Moves a page to the trash from its row's menu in the sidebar. */
export async function trashFromTree(row: Locator) {
  await row.getByRole("button", { name: "Page options" }).click();
  await row.page().getByRole("menuitem", { name: "Move to Trash" }).click();
}
