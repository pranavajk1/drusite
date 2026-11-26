// SPDX-License-Identifier: AGPL-3.0-or-later

// The binding end to end: a real ProseMirror view, edited the ways a person edits, against the
// in-memory core. After every action the core's page must match what the editor shows, and
// Enter and Backspace must reach the core as splits and joins, not as copies.

import { TextSelection } from "prosemirror-state";
import { Slice, Fragment } from "prosemirror-model";
import { undo, redo } from "prosemirror-history";
import { toggleMark } from "prosemirror-commands";
import { beforeAll, describe, expect, it } from "vitest";

import { CoreClient } from "../core/client";
import { MemoryCore } from "../core/memory";
import type { Block, Edit } from "../core/types";
import { attrsOf, docNode, marksFromCore, unitsOfNode } from "./convert";
import { type Editor, createEditor } from "./editor";
import { schema } from "./schema";

beforeAll(() => {
  // jsdom has no layout; ProseMirror only needs these not to throw.
  Range.prototype.getClientRects = () => [] as unknown as DOMRectList;
  Range.prototype.getBoundingClientRect = () => new DOMRect();
  HTMLElement.prototype.getClientRects = () => [] as unknown as DOMRectList;
});

async function setup(texts: string[] = ["hello world"]) {
  const memory = new MemoryCore();
  const client = new CoreClient(memory.transport);
  const page = await client.createPage(null, "Test");
  let view = await client.openPage(page.id);
  // Seed the page through the core.
  const first = view.blocks[0]!.id;
  const seed: Edit[] = [{ type: "insertText", block: first, pos: 0, text: texts[0]! }];
  const { created } = await client.edit(page.id, seed);
  let last = first;
  for (const t of texts.slice(1)) {
    const out = await client.edit(page.id, [{ type: "insertBlock", after: last, kind: { type: "paragraph" } }]);
    last = out.created[0]!;
    await client.edit(page.id, [{ type: "insertText", block: last, pos: 0, text: t }]);
  }
  expect(created).toEqual([]);
  view = await client.openPage(page.id);
  const mount = document.createElement("div");
  document.body.append(mount);
  const sent: Edit[][] = [];
  const recording = new CoreClient({
    call: (command) => {
      if (command.type === "edit") sent.push(command.edits);
      return memory.transport.call(command);
    },
    subscribe: memory.transport.subscribe,
  });
  const editor = createEditor(mount, recording, view, (e) => {
    throw e;
  });
  return { memory, editor, page: page.id, sent };
}

/** What the editor shows, as the core's blocks. */
function shown(editor: Editor): Block[] {
  const out: Block[] = [];
  editor.view.state.doc.forEach((node) => {
    const a = attrsOf(node);
    const units = unitsOfNode(node);
    const spans: Block["spans"] = [];
    for (let i = 0; i < units.text.length; ) {
      let j = i + 1;
      while (j < units.text.length && units.marks[j] === units.marks[i]) j++;
      const m = units.marks[i]!;
      spans.push({
        text: units.text.slice(i, j),
        marks: {
          bold: m.has("bold"),
          italic: m.has("italic"),
          underline: m.has("underline"),
          strikethrough: m.has("strikethrough"),
          code: m.has("code"),
          link: (m.get("link") as string | undefined) ?? null,
        },
      });
      i = j;
    }
    out.push({ id: a.id!, kind: a.kind, indent: a.indent, spans });
  });
  return out;
}

/** Normalised for comparing: adjacent spans with the same marks merged. */
function norm(blocks: Block[]) {
  return blocks.map((b) => {
    const units = { text: "", marks: [] as string[] };
    for (const s of b.spans) {
      for (let i = 0; i < s.text.length; i++) units.marks.push(JSON.stringify([...marksFromCore(s.marks)].sort()));
      units.text += s.text;
    }
    return { id: b.id, kind: b.kind, indent: b.indent, ...units };
  });
}

async function agree(t: Awaited<ReturnType<typeof setup>>) {
  await t.editor.sync.flush();
  expect(norm(shown(t.editor))).toEqual(norm(t.memory.blocks(t.page)));
}

function cursorAt(editor: Editor, block: number, offset: number) {
  const { state } = editor.view;
  let pos = 0;
  for (let i = 0; i < block; i++) pos += state.doc.child(i).nodeSize;
  editor.view.dispatch(state.tr.setSelection(TextSelection.create(state.doc, pos + 1 + offset)));
}

function type(editor: Editor, text: string) {
  const { view } = editor;
  const { from, to } = view.state.selection;
  // Through the input-rule path, as a keystroke would go.
  const handled = view.someProp("handleTextInput", (f) => f(view, from, to, text, () => view.state.tr.insertText(text, from, to)));
  if (!handled) view.dispatch(view.state.tr.insertText(text, from, to));
}

function key(editor: Editor, name: string) {
  const handled = editor.view.someProp("handleKeyDown", (f) => f(editor.view, keyEvent(name)));
  if (!handled) throw new Error(`no handler for ${name}`);
}

function keyEvent(name: string): KeyboardEvent {
  const parts = name.split("-");
  const k = parts.pop()!;
  return new KeyboardEvent("keydown", {
    key: k,
    shiftKey: parts.includes("Shift"),
    // jsdom isn't a Mac, so ProseMirror's "Mod" is Ctrl.
    ctrlKey: parts.includes("Mod"),
    altKey: parts.includes("Alt"),
  });
}

describe("the binding", () => {
  it("sends typing as text inserted into the block", async () => {
    const t = await setup(["hello"]);
    cursorAt(t.editor, 0, 5);
    type(t.editor, " world");
    await agree(t);
    expect(t.sent.at(-1)).toEqual([{ type: "insertText", block: expect.any(String), pos: 5, text: " world" }]);
  });

  it("sends Enter as a split and Backspace at the start as a join", async () => {
    const t = await setup(["abcdef"]);
    cursorAt(t.editor, 0, 3);
    key(t.editor, "Enter");
    await agree(t);
    expect(t.sent.at(-1)!.map((e) => e.type)).toEqual(["split"]);
    expect(shown(t.editor).map((b) => b.spans.map((s) => s.text).join(""))).toEqual(["abc", "def"]);
    // The new block has the core's ID, not a temporary one.
    expect(attrsOf(t.editor.view.state.doc.child(1)).id).not.toMatch(/^tmp-/);
    cursorAt(t.editor, 1, 0);
    key(t.editor, "Backspace");
    await agree(t);
    expect(t.sent.at(-1)!.map((e) => e.type)).toEqual(["join"]);
  });

  it("types into a block created a moment ago before its ID is known", async () => {
    const t = await setup(["one"]);
    cursorAt(t.editor, 0, 3);
    key(t.editor, "Enter");
    type(t.editor, "t");
    type(t.editor, "wo");
    key(t.editor, "Enter");
    type(t.editor, "three");
    await agree(t);
    expect(shown(t.editor).map((b) => b.spans.map((s) => s.text).join(""))).toEqual(["one", "two", "three"]);
  });

  it("turns Markdown shortcuts into block kinds", async () => {
    const t = await setup([""]);
    cursorAt(t.editor, 0, 0);
    for (const ch of "## ") type(t.editor, ch);
    type(t.editor, "Title");
    key(t.editor, "Enter");
    for (const ch of "- ") type(t.editor, ch);
    type(t.editor, "item");
    key(t.editor, "Enter");
    type(t.editor, "next");
    key(t.editor, "Enter");
    key(t.editor, "Enter"); // an empty list item ends the list
    for (const ch of "[] ") type(t.editor, ch);
    type(t.editor, "task");
    await agree(t);
    expect(shown(t.editor).map((b) => b.kind)).toEqual([
      { type: "heading", level: "h2" },
      { type: "bulleted" },
      { type: "bulleted" },
      { type: "todo", checked: false },
    ]);
  });

  it("sends formatting as format edits, and typing at the end of bold text stays bold", async () => {
    const t = await setup(["make this bold"]);
    const { view } = t.editor;
    view.dispatch(view.state.tr.setSelection(TextSelection.create(view.state.doc, 6, 10)));
    toggleMark(schema.marks.bold!)(view.state, view.dispatch);
    await agree(t);
    expect(t.sent.at(-1)).toEqual([
      { type: "format", block: expect.any(String), start: 5, end: 9, mark: { type: "bold" }, on: true },
    ]);
    cursorAt(t.editor, 0, 9);
    type(t.editor, "!");
    await agree(t);
    expect(t.sent.at(-1)!.map((e) => e.type)).toEqual(["insertText"]);
  });

  it("indents with Tab, never more than one level deeper than the block before", async () => {
    const t = await setup(["a", "b", "c"]);
    cursorAt(t.editor, 1, 0);
    key(t.editor, "Tab");
    key(t.editor, "Tab");
    await agree(t);
    expect(shown(t.editor).map((b) => b.indent)).toEqual([0, 1, 0]);
    cursorAt(t.editor, 0, 0);
    key(t.editor, "Tab"); // the first block can't indent
    await agree(t);
    expect(shown(t.editor).map((b) => b.indent)).toEqual([0, 1, 0]);
  });

  it("handles pasting several blocks, cutting across blocks, undo and redo", async () => {
    const t = await setup(["first", "second", "third"]);
    const { view } = t.editor;
    // Paste two paragraphs in the middle of "second".
    cursorAt(t.editor, 1, 3);
    const pasted = docNode([
      { id: "x", kind: { type: "quote" }, indent: 0, spans: [{ text: "P1", marks: marksOf() }] },
      { id: "y", kind: { type: "paragraph" }, indent: 0, spans: [{ text: "P2", marks: marksOf({ italic: true }) }] },
    ]);
    view.dispatch(view.state.tr.replaceSelection(new Slice(Fragment.from(pasted.content), 1, 1)));
    await agree(t);
    // Select from inside "first" to inside "third" and delete.
    view.dispatch(view.state.tr.setSelection(TextSelection.create(view.state.doc, 3, view.state.doc.content.size - 3)));
    key(t.editor, "Backspace");
    await agree(t);
    undo(view.state, view.dispatch);
    await agree(t);
    redo(view.state, view.dispatch);
    await agree(t);
    undo(view.state, view.dispatch);
    undo(view.state, view.dispatch);
    await agree(t);
  });

  it("moves a block when ProseMirror moves its node", async () => {
    const t = await setup(["a", "b", "c", "d"]);
    const { view } = t.editor;
    const doc = view.state.doc;
    const b = doc.child(1);
    const bStart = doc.child(0).nodeSize;
    const tr = view.state.tr.delete(bStart, bStart + b.nodeSize);
    tr.insert(tr.doc.content.size, b);
    view.dispatch(tr);
    await agree(t);
    expect(t.sent.at(-1)).toEqual([{ type: "moveBlock", block: attrsOf(b).id, after: expect.any(String) }]);
  });

  it("stays in step through 1,000 random edits", async () => {
    const t = await setup(["The quick brown fox", "jumps over", "the lazy dog"]);
    const { view } = t.editor;
    // mulberry32: small, with well-mixed low bits.
    let seed = 42;
    const rnd = (n: number) => {
      seed = (seed + 0x6d2b79f5) | 0;
      let x = Math.imul(seed ^ (seed >>> 15), seed | 1);
      x ^= x + Math.imul(x ^ (x >>> 7), x | 61);
      return ((x ^ (x >>> 14)) >>> 0) % n;
    };
    const keys = ["Enter", "Backspace", "Delete", "Tab", "Shift-Tab", "Mod-b", "Mod-i", "Mod-z", "Mod-Shift-z"];
    const words = ["a", "bc", " ", "é", "日本", "🙂", "- ", "1. "];
    for (let step = 0; step < 1000; step++) {
      const doc = view.state.doc;
      const blockIndex = rnd(doc.childCount);
      let start = 0;
      for (let i = 0; i < blockIndex; i++) start += doc.child(i).nodeSize;
      const node = doc.child(blockIndex);
      const text = unitsOfNode(node).text;
      const offsets = [0];
      for (let i = 0; i < text.length; i++) {
        const c = text.charCodeAt(i);
        if (!(c >= 0xd800 && c <= 0xdbff)) offsets.push(i + 1);
      }
      const a = start + 1 + offsets[rnd(offsets.length)]!;
      const b = rnd(4) === 0 ? start + 1 + offsets[rnd(offsets.length)]! : a;
      view.dispatch(view.state.tr.setSelection(TextSelection.create(doc, Math.min(a, b), Math.max(a, b))));
      if (rnd(2) === 0) type(t.editor, words[rnd(words.length)]!);
      else {
        const k = keys[rnd(keys.length)]!;
        const handled = view.someProp("handleKeyDown", (f) => f(view, keyEvent(k)));
        if (!handled && (k === "Mod-z" || k === "Mod-Shift-z")) (k === "Mod-z" ? undo : redo)(view.state, view.dispatch);
      }
      if (rnd(5) === 0) await agree(t);
    }
    await agree(t);
    const kinds = new Set(t.sent.flat().map((e) => e.type));
    console.log(`random edits: ${t.editor.sync.sent} sent, kinds ${[...kinds].sort().join(" ")}, ${view.state.doc.childCount} blocks`);
    expect(t.editor.sync.sent).toBeGreaterThan(500);
    for (const kind of ["insertText", "deleteText", "split", "join", "format", "setIndent", "setKind"]) expect(kinds).toContain(kind);
  });
});

function marksOf(on: Partial<Record<"bold" | "italic", boolean>> = {}) {
  return { bold: !!on.bold, italic: !!on.italic, underline: false, strikethrough: false, code: false, link: null };
}
