// SPDX-License-Identifier: AGPL-3.0-or-later

// The slash menu: `/` at the start of a block, or after a space, lists the kinds of block, and
// typing narrows the list. Picking one turns an empty block into that kind, or adds a new block
// of that kind below one with text. It's another way to the same blocks the Markdown shortcuts
// make. Also here: the "Press / for blocks" hint on the empty block the cursor is in.

import { Plugin, PluginKey, type EditorState, TextSelection, type Transaction } from "prosemirror-state";
import { Decoration, DecorationSet, type EditorView } from "prosemirror-view";

import type { BlockKind } from "../core/types";
import { type IconName, iconSvg } from "../lib/icons/icons";
import { visibleBottom } from "../lib/ui/floating";
import { PICK_FILES } from "./attachments";
import { attrsOf } from "./convert";
import { defaultQueryJson } from "./query";
import { blockType } from "./schema";

interface Item {
  label: string;
  icon: IconName;
  /** The Markdown shortcut that makes the same block, shown on the right. */
  hint?: string;
  keywords: string;
  /** The block's new kind, or what else picking it does. */
  kind: BlockKind | "divider" | "filter" | "file";
}

export const SLASH_ITEMS: Item[] = [
  { label: "Text", icon: "type", keywords: "paragraph plain", kind: { type: "paragraph" } },
  { label: "Heading 1", icon: "heading-1", hint: "#", keywords: "h1 title", kind: { type: "heading", level: "h1" } },
  { label: "Heading 2", icon: "heading-2", hint: "##", keywords: "h2 subtitle", kind: { type: "heading", level: "h2" } },
  { label: "Heading 3", icon: "heading-3", hint: "###", keywords: "h3", kind: { type: "heading", level: "h3" } },
  { label: "Bulleted list", icon: "list", hint: "-", keywords: "bullet unordered ul", kind: { type: "bulleted" } },
  { label: "Numbered list", icon: "list-ordered", hint: "1.", keywords: "ordered ol number", kind: { type: "numbered" } },
  { label: "To-do list", icon: "square-check", hint: "[]", keywords: "todo task checkbox check", kind: { type: "todo", checked: false } },
  { label: "Toggle", icon: "list-collapse", hint: ">", keywords: "fold collapse disclosure", kind: { type: "toggle" } },
  { label: "Quote", icon: "text-quote", hint: '"', keywords: "blockquote citation", kind: { type: "quote" } },
  { label: "Callout", icon: "message-square-text", hint: "!", keywords: "note tip info aside", kind: { type: "callout", icon: "💡" } },
  { label: "Code", icon: "code", hint: "```", keywords: "snippet pre monospace", kind: { type: "code", language: "" } },
  { label: "Divider", icon: "minus", hint: "---", keywords: "rule line hr separator", kind: "divider" },
  { label: "Filter", icon: "list-filter", keywords: "query journal database view", kind: "filter" },
  { label: "File or image", icon: "paperclip", keywords: "attachment upload image picture photo", kind: "file" },
];

interface SlashState {
  /** Where the `/` is, while the menu is open. */
  from: number | null;
  query: string;
  index: number;
  /** A `/` the user closed the menu on with Escape: it stays closed for that one. */
  dismissed: number | null;
}

const key = new PluginKey<SlashState>("slash");
const CLOSED: SlashState = { from: null, query: "", index: 0, dismissed: null };

/** The `/` before the cursor that opens the menu, and what's typed after it. */
function trigger(state: EditorState): { from: number; query: string } | null {
  const { $from, empty } = state.selection;
  if (!empty || $from.parent.type !== blockType) return null;
  const kind = attrsOf($from.parent).kind.type;
  if (kind === "code" || kind === "divider" || kind === "query" || kind === "attachment") return null;
  const before = $from.parent.textBetween(Math.max(0, $from.parentOffset - 22), $from.parentOffset, undefined, "\ufffc");
  const m = /\/([^\s/]{0,20})$/.exec(before);
  if (!m) return null;
  const query = m[1]!;
  const slash = $from.parentOffset - query.length - 1;
  // Only at the block's start, or after a space, so a URL's or a path's slashes don't open it.
  if (slash > 0 && !/\s/.test($from.parent.textBetween(slash - 1, slash, undefined, "\ufffc"))) return null;
  return { from: $from.pos - query.length - 1, query };
}

export function matches(query: string): Item[] {
  const q = query.toLowerCase();
  if (q === "") return SLASH_ITEMS;
  return SLASH_ITEMS.filter(
    (i) => i.label.toLowerCase().includes(q) || i.keywords.includes(q) || i.label.toLowerCase().replace(/[^a-z0-9]/g, "").startsWith(q),
  );
}

/** Removes the `/query` and makes the block (or a new one below it) `item`. */
function apply(view: EditorView, from: number, item: Item) {
  const { state } = view;
  const to = state.selection.from;
  const tr: Transaction = state.tr.delete(from, to);
  const $at = tr.doc.resolve(from);
  let pos = $at.before(1);
  const block = tr.doc.nodeAt(pos)!;
  const indent = attrsOf(block).indent;
  if (block.content.size > 0) {
    // The block has text: the new kind goes in a new block below it.
    const below = pos + block.nodeSize;
    tr.insert(below, blockType.create({ id: null, kind: { type: "paragraph" }, indent }));
    pos = below;
  }
  tr.setSelection(TextSelection.create(tr.doc, pos + 1));
  const paragraphAfter = () => {
    const at = pos + tr.doc.nodeAt(pos)!.nodeSize;
    tr.insert(at, blockType.create({ id: null, kind: { type: "paragraph" }, indent }));
    tr.setSelection(TextSelection.create(tr.doc, at + 1));
  };
  if (item.kind === "divider") {
    tr.setNodeAttribute(pos, "kind", { type: "divider" });
    paragraphAfter();
  } else if (item.kind === "filter") {
    tr.setNodeAttribute(pos, "kind", { type: "query", query: defaultQueryJson() });
    paragraphAfter();
  } else if (item.kind === "file") {
    tr.setMeta(PICK_FILES, true);
  } else {
    tr.setNodeAttribute(pos, "kind", item.kind);
  }
  view.dispatch(tr.scrollIntoView());
  view.focus();
}

class MenuView {
  private dom: HTMLDivElement;
  private items: Item[] = [];

  constructor(private view: EditorView) {
    this.dom = document.createElement("div");
    this.dom.className = "popover slash-menu";
    this.dom.setAttribute("role", "listbox");
    this.dom.setAttribute("aria-label", "Blocks");
    this.dom.hidden = true;
    // Keep the editor's focus and selection while the menu is used with a pointer.
    this.dom.addEventListener("mousedown", (e) => e.preventDefault());
    document.body.append(this.dom);
  }

  update(view: EditorView) {
    const s = key.getState(view.state)!;
    if (s.from === null) {
      this.dom.hidden = true;
      return;
    }
    this.items = matches(s.query);
    if (this.items.length === 0) {
      this.dom.hidden = true;
      return;
    }
    this.render(s);
    this.dom.hidden = false;
    const at = view.coordsAtPos(s.from);
    this.dom.style.maxHeight = "";
    const r = this.dom.getBoundingClientRect();
    // Below the caret, or above it when the keyboard or the window's edge leaves no room. When
    // neither fits, the menu shrinks to the room above (it scrolls).
    const bottom = visibleBottom();
    const below = at.bottom + 6;
    const roomBelow = bottom - 8 - below;
    const roomAbove = at.top - 6 - 8;
    if (r.height <= roomBelow || roomBelow >= roomAbove) {
      this.dom.style.maxHeight = `${Math.max(120, roomBelow)}px`;
      this.dom.style.top = `${below}px`;
    } else {
      const h = Math.min(r.height, roomAbove);
      this.dom.style.maxHeight = `${roomAbove}px`;
      this.dom.style.top = `${at.top - 6 - h}px`;
    }
    this.dom.style.left = `${Math.max(8, Math.min(at.left - 8, window.innerWidth - 8 - r.width))}px`;
    this.dom.querySelector(".active")?.scrollIntoView({ block: "nearest" });
  }

  private render(s: SlashState) {
    const head = document.createElement("div");
    head.className = "menu-label";
    head.textContent = s.query ? "Blocks" : "Basic blocks";
    const rows = this.items.map((item, i) => {
      const row = document.createElement("div");
      row.className = "menu-item" + (i === s.index ? " active" : "");
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(i === s.index));
      row.innerHTML = `<span class="slash-icon">${iconSvg(item.icon, 18)}</span>`;
      const label = document.createElement("span");
      label.className = "menu-item-label";
      label.textContent = item.label;
      row.append(label);
      if (item.hint) {
        const kbd = document.createElement("kbd");
        kbd.textContent = item.hint;
        row.append(kbd);
      }
      row.addEventListener("pointerenter", () => {
        const st = key.getState(this.view.state)!;
        if (st.index !== i) this.view.dispatch(this.view.state.tr.setMeta(key, { ...st, index: i }));
      });
      row.addEventListener("click", () => pick(this.view, i));
      return row;
    });
    this.dom.replaceChildren(head, ...rows);
  }

  destroy() {
    this.dom.remove();
  }
}

function pick(view: EditorView, index: number) {
  const s = key.getState(view.state)!;
  const item = matches(s.query)[index];
  if (s.from === null || !item) return;
  apply(view, s.from, item);
}

export function slashMenu(): Plugin<SlashState> {
  return new Plugin<SlashState>({
    key,
    state: {
      init: () => CLOSED,
      apply(tr, prev, _old, state) {
        const meta = tr.getMeta(key) as SlashState | undefined;
        if (meta) return meta;
        const t = trigger(state);
        const dismissed = prev.dismissed !== null && t && tr.mapping.map(prev.dismissed) === t.from ? t.from : null;
        if (!t || dismissed !== null) return { ...CLOSED, dismissed };
        const index = t.query === prev.query && prev.from !== null ? prev.index : 0;
        return { from: t.from, query: t.query, index, dismissed: null };
      },
    },
    view: (view) => new MenuView(view),
    props: {
      handleKeyDown(view, e) {
        const s = key.getState(view.state)!;
        if (s.from === null) return false;
        const n = matches(s.query).length;
        if (n === 0) return false;
        const set = (index: number) => view.dispatch(view.state.tr.setMeta(key, { ...s, index }));
        if (e.key === "ArrowDown") set((s.index + 1) % n);
        else if (e.key === "ArrowUp") set((s.index - 1 + n) % n);
        else if (e.key === "Enter" || e.key === "Tab") pick(view, s.index);
        else if (e.key === "Escape") view.dispatch(view.state.tr.setMeta(key, { ...CLOSED, dismissed: s.from }));
        else return false;
        return true;
      },
      decorations(state) {
        // "Press / for blocks" on the empty block the cursor is in, unless it's the page's only one
        // (that one has the longer hint).
        const { $from, empty } = state.selection;
        if (!empty || $from.depth < 1 || state.doc.childCount === 1) return null;
        const block = $from.node(1);
        if (block.content.size !== 0) return null;
        const pos = $from.before(1);
        return DecorationSet.create(state.doc, [Decoration.node(pos, pos + block.nodeSize, { class: "is-empty-current" })]);
      },
    },
  });
}
