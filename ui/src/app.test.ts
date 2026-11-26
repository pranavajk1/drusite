// SPDX-License-Identifier: AGPL-3.0-or-later

// The whole app against the in-memory core: pages, switching between them, and the sidebar.

import { flushSync, mount, unmount } from "svelte";
import { beforeAll, beforeEach, expect, it } from "vitest";

import App from "./App.svelte";
import { CoreClient } from "./core/client";
import { MemoryCore } from "./core/memory";
import type { PageId } from "./core/types";

beforeAll(() => {
  Range.prototype.getClientRects = () => [] as unknown as DOMRectList;
  Range.prototype.getBoundingClientRect = () => new DOMRect();
  HTMLElement.prototype.getClientRects = () => [] as unknown as DOMRectList;
});

beforeEach(() => localStorage.clear());

const settle = () => new Promise((r) => setTimeout(r, 0)).then(() => flushSync());
async function until(check: () => boolean, what: string) {
  for (let i = 0; i < 200; i++) {
    if (check()) return;
    await settle();
  }
  throw new Error(`timed out waiting for ${what}`);
}

async function start(memory = new MemoryCore()) {
  const target = document.createElement("div");
  document.body.append(target);
  const errors: string[] = [];
  const client = new CoreClient(memory.transport);
  const app = mount(App, { target, props: { client } });
  const q = <T extends Element>(sel: string) => target.querySelector<T>(sel);
  const rows = () => [...target.querySelectorAll<HTMLElement>(".tree-row")];
  const title = () => q<HTMLInputElement>(".page-title")?.value;
  const text = () => q(".ProseMirror")?.textContent ?? "";
  const ready = (t: string) => until(() => title() === t && q(".ProseMirror") !== null, `page "${t}"`);
  await until(() => q(".ProseMirror") !== null, "the first page");
  return {
    memory,
    client,
    target,
    errors: () => target.querySelector(".error")?.textContent ?? null,
    rows,
    title,
    text,
    ready,
    rename(t: string) {
      const input = q<HTMLInputElement>(".page-title")!;
      input.value = t;
      input.dispatchEvent(new Event("input", { bubbles: true }));
      flushSync();
    },
    close: () => unmount(app),
  };
}

it("keeps each page's title, and keeps the open page open, when switching quickly", async () => {
  const t = await start();
  t.rename("Alpha");
  await t.ready("Alpha");
  // A subpage, renamed and switched away from before its rename has gone through.
  t.rows()[0]!.querySelector<HTMLButtonElement>("[aria-label='Add a page inside']")!.click();
  await until(() => t.rows().length === 2 && t.title() === "", "the new subpage");
  t.rename("Beta");
  t.rows()[0]!.click();
  await t.ready("Alpha");
  // Back and forth at once: Beta's first editor is still closing when the last one opens it.
  t.rows()[1]!.click();
  t.rows()[0]!.click();
  t.rows()[1]!.click();
  await t.ready("Beta");
  await until(() => t.rows()[1]?.textContent?.includes("Beta") ?? false, "Beta in the tree");
  for (let i = 0; i < 20; i++) await settle();
  expect(t.rows().map((r) => r.querySelector(".tree-title")!.textContent)).toEqual(["Alpha", "Beta"]);
  // The page on screen still takes edits.
  const beta = (await t.client.listPages()).find((p) => p.title === "Beta")!.id as PageId;
  const view = await t.client.openPage(beta);
  await t.client.closePage(beta);
  await t.client.edit(beta, [{ type: "insertText", block: view.blocks[0]!.id, pos: 0, text: "hi" }]);
  expect(t.errors()).toBeNull();
  t.close();
});

it("remembers which pages are unfolded in the sidebar across restarts", async () => {
  const memory = new MemoryCore();
  const client = new CoreClient(memory.transport);
  const a = await client.createPage(null, "A");
  const b = await client.createPage(a.id, "B");
  await client.createPage(b.id, "C");
  await client.createPage(null, "D");
  let t = await start(memory);
  const titles = () => t.rows().map((r) => r.querySelector(".tree-title")!.textContent);
  expect(titles()).toEqual(["A", "D"]);
  const caret = (name: string) =>
    t.rows().find((r) => r.querySelector(".tree-title")!.textContent === name)!.querySelector<HTMLButtonElement>(".tree-caret")!;
  caret("A").click();
  flushSync();
  caret("B").click();
  flushSync();
  expect(titles()).toEqual(["A", "B", "C", "D"]);
  t.close();
  t = await start(memory);
  expect(titles()).toEqual(["A", "B", "C", "D"]);
  // Folding is remembered too, and a deleted page is forgotten.
  caret("B").click();
  flushSync();
  await client.deletePage(b.id);
  await until(() => titles().length === 2, "B gone");
  t.close();
  t = await start(memory);
  expect(titles()).toEqual(["A", "D"]);
  expect(JSON.parse(localStorage.getItem("drusite.expanded")!)).toEqual([a.id]);
  t.close();
});

it("moves to another page when the open page's parent is deleted", async () => {
  const memory = new MemoryCore();
  const client = new CoreClient(memory.transport);
  const a = await client.createPage(null, "A");
  const b = await client.createPage(a.id, "B");
  await client.createPage(null, "C");
  localStorage.setItem("drusite.lastPage", b.id);
  const t = await start(memory);
  await t.ready("B");
  const row = t.rows().find((r) => r.querySelector(".tree-title")!.textContent === "A")!;
  const del = () => row.querySelector<HTMLButtonElement>("[aria-label='Delete page']")!;
  del().click();
  flushSync();
  del().click();
  await t.ready("C");
  expect(t.errors()).toBeNull();
  t.close();
});
