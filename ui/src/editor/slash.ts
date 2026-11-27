// SPDX-License-Identifier: AGPL-3.0-or-later

// The slash menu: `/` at the start of a block, or after a space, lists the kinds of block, and
// typing narrows the list. Picking one turns an empty block into that kind, or adds a new block
// of that kind below one with text. It's another way to the same blocks the Markdown shortcuts
// make. Also here: the "Press / for blocks" hint on the empty block the cursor is in.

import { Plugin, PluginKey, type EditorState, TextSelection, type Transaction } from "prosemirror-state";
import { Decoration, DecorationSet, type EditorView } from "prosemirror-view";

import type { BlockKind } from "../core/types";
import { type IconName, iconSvg } from "../lib/icons/icons";
import { visibleBottom, visibleTop } from "../lib/ui/floating";
import { PICK_FILES } from "./attachments";
import { attrsOf } from "./convert";
import { MAKE_DATABASE } from "./database.svelte";
import { defaultQueryJson } from "./query";
import { blockType } from "./schema";

interface Item {
  label: string;
  icon: IconName;
  /** The Markdown shortcut that makes the same block, shown on the right. */
  hint?: string;
  keywords: string;
  /** The block's new kind, or what else picking it does. */
  kind: BlockKind | "divider" | "filter" | "file" | "database" | "tasks";
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
  { label: "Filter", icon: "list-filter", keywords: "query journal view", kind: "filter" },
  { label: "Database", icon: "table", keywords: "table inline database view rows", kind: "database" },
  { label: "Task board", icon: "square-kanban", keywords: "tasks kanban board todo status", kind: "tasks" },
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
  } else if (item.kind === "database" || item.kind === "tasks") {
    tr.setMeta(MAKE_DATABASE, item.kind === "tasks" ? "tasks" : "empty");
    paragraphAfter();
    tr.setSelection(TextSelection.create(tr.doc, pos + 1));
  } else {
    tr.setNodeAttribute(pos, "kind", item.kind);
  }
  view.dispatch(tr.scrollIntoView());
  view.focus();
}

/** Notion's is about ten rows: enough to choose from, without hiding the page. */
const MAX_HEIGHT = 340;

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
    // A touch on the menu can take the focus from the editor before its click arrives, which
    // would hide the menu under the finger: while it's pressed, the menu stays.
    this.dom.addEventListener("pointerdown", () => (this.pressed = true));
    const release = () => setTimeout(() => (this.pressed = false), 400);
    this.dom.addEventListener("pointerup", release);
    this.dom.addEventListener("pointercancel", release);
    this.dom.addEventListener("click", (e) => {
      this.pressed = false;
      const row = (e.target as Element).closest<HTMLElement>("[data-index], .slash-close");
      if (!row) return;
      if (row.classList.contains("slash-close")) this.dismiss();
      else pick(this.view, Number(row.dataset.index));
    });
    document.body.append(this.dom);
    // Only while the editor has the focus: not over a dialog, and not for a `/` another device typed.
    this.onFocus = () => this.update(this.view);
    this.onBlur = () => {
      if (!this.pressed) this.dom.hidden = true;
    };
    view.dom.addEventListener("focus", this.onFocus);
    view.dom.addEventListener("blur", this.onBlur);
    // The page scrolls the window, and the keyboard resizes the visual viewport: follow the line.
    this.onMove = () => {
      if (!this.dom.hidden) this.place();
    };
    window.addEventListener("scroll", this.onMove, true);
    window.addEventListener("resize", this.onMove);
    window.visualViewport?.addEventListener("resize", this.onMove);
    // The phone's formatting bar can show after the menu opened (once the keyboard is up): make
    // room for it then.
    this.barWatch = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(this.onMove);
  }

  private barWatch: ResizeObserver | null;
  private watched: Element | null = null;

  private onMove: () => void;

  private onFocus: () => void;
  private pressed = false;

  private dismiss() {
    const st = key.getState(this.view.state)!;
    this.view.dispatch(this.view.state.tr.setMeta(key, { ...CLOSED, dismissed: st.from }));
    this.view.focus();
  }
  private onBlur: () => void;

  update(view: EditorView) {
    const s = key.getState(view.state)!;
    if (s.from === null || !view.hasFocus()) {
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
    this.place();
  }

  /**
   * Below the line being typed on, or above it when there's no room below (the keyboard, the
   * window's edge). It never covers that line: when neither side fits it, the list gets shorter
   * and scrolls.
   */
  private place() {
    const bar = document.querySelector(".phone-toolbar");
    if (bar && bar !== this.watched && this.barWatch) {
      this.barWatch.disconnect();
      this.barWatch.observe(bar);
      this.watched = bar;
    }
    const caret = this.view.coordsAtPos(this.view.state.selection.from);
    const top = visibleTop() + 8;
    const bottom = visibleBottom() - 8;
    const below = caret.bottom + 4;
    const above = caret.top - 4;
    this.dom.style.maxHeight = `${MAX_HEIGHT}px`;
    // offsetHeight, not the bounding box: the opening animation scales the menu.
    const h = this.dom.offsetHeight;
    const roomBelow = bottom - below;
    const roomAbove = above - top;
    if (h <= roomBelow || (h > roomAbove && roomBelow >= roomAbove)) {
      this.dom.style.maxHeight = `${Math.min(MAX_HEIGHT, Math.max(96, roomBelow))}px`;
      this.dom.style.top = `${below}px`;
    } else {
      const fit = Math.min(h, roomAbove);
      this.dom.style.maxHeight = `${fit}px`;
      this.dom.style.top = `${above - fit}px`;
    }
    const w = this.dom.offsetWidth;
    this.dom.style.left = `${Math.max(8, Math.min(caret.left - 12, window.innerWidth - 8 - w))}px`;
    this.dom.querySelector(".slash-list .active")?.scrollIntoView({ block: "nearest" });
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
      row.dataset.index = String(i);
      return row;
    });
    const list = document.createElement("div");
    list.className = "slash-list";
    list.append(head, ...rows);
    const close = document.createElement("div");
    close.className = "menu-item slash-close";
    close.innerHTML = `<span class="menu-item-label">Close menu</span><kbd>esc</kbd>`;
    this.dom.replaceChildren(list, close);
  }

  destroy() {
    this.view.dom.removeEventListener("focus", this.onFocus);
    this.view.dom.removeEventListener("blur", this.onBlur);
    window.removeEventListener("scroll", this.onMove, true);
    window.removeEventListener("resize", this.onMove);
    window.visualViewport?.removeEventListener("resize", this.onMove);
    this.barWatch?.disconnect();
    this.dom.remove();
  }
}

function filterHint(): HTMLElement {
  const hint = document.createElement("span");
  hint.className = "slash-hint";
  hint.textContent = "Type to filter";
  hint.setAttribute("aria-hidden", "true");
  hint.contentEditable = "false";
  return hint;
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
        if (s.from === null || !view.hasFocus()) return false;
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
        // "Type to filter" after a `/` with nothing typed after it yet, as in Notion.
        const s = key.getState(state)!;
        if (s.from !== null && s.query === "" && state.selection.from === s.from + 1)
          return DecorationSet.create(state.doc, [
            Decoration.widget(s.from + 1, filterHint, { side: 1, key: "slash-hint", ignoreSelection: true }),
          ]);
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
