// SPDX-License-Identifier: AGPL-3.0-or-later

// How blocks look: list numbers, folded toggles, and the parts of a block that aren't text (a
// to-do's checkbox, a toggle's arrow, a callout's icon, a divider's rule).

import type { Node } from "prosemirror-model";
import { type EditorState, Plugin, PluginKey, TextSelection, type Transaction } from "prosemirror-state";
import { ReplaceStep } from "prosemirror-transform";
import { Decoration, DecorationSet, type EditorView, type NodeView } from "prosemirror-view";

import { iconSvg } from "../lib/icons/icons";
import { attachmentView } from "./attachments";
import { databaseView } from "./database.svelte";
import { queryView } from "./query";

import { attrsOf } from "./convert";
import { blockDom, blockType, schema } from "./schema";

interface Display {
  /** Toggles that are folded, by block ID (view state only, never saved). */
  folded: ReadonlySet<string>;
  decorations: DecorationSet;
}

const key = new PluginKey<Display>("drusite-display");
const FOLD = "drusite-fold";

/** Numbers for numbered lists and hiding for folded toggles' children. */
function decorate(doc: Node, folded: ReadonlySet<string>): DecorationSet {
  const decos: Decoration[] = [];
  // counters[indent] = the number of the last numbered block at that indent in the current run.
  const counters: number[] = [];
  let hideDeeperThan: number | null = null;
  doc.forEach((node, pos) => {
    const { id, kind, indent } = attrsOf(node);
    if (hideDeeperThan !== null && indent <= hideDeeperThan) hideDeeperThan = null;
    const hidden = hideDeeperThan !== null;
    counters.length = Math.min(counters.length, indent + 1);
    let attrs: Record<string, string> | null = null;
    if (kind.type === "numbered") {
      const n = (counters[indent] ?? 0) + 1;
      counters[indent] = n;
      attrs = { "data-number": `${n}.` };
    } else {
      counters[indent] = 0;
    }
    if (hidden) attrs = { ...attrs, class: "folded-away" };
    if (attrs) decos.push(Decoration.node(pos, pos + node.nodeSize, attrs));
    if (!hidden && kind.type === "toggle" && id !== null && folded.has(id)) hideDeeperThan = indent;
  });
  const only = doc.childCount === 1 ? doc.firstChild : null;
  if (only && only.content.size === 0 && attrsOf(only).kind.type === "paragraph") {
    decos.push(Decoration.node(0, only.nodeSize, { class: "empty-hint" }));
  }
  return DecorationSet.create(doc, decos);
}

/** Whether a transaction only typed inside blocks, which can't change numbering or folding. */
function onlyText(tr: Transaction): boolean {
  if (tr.before.childCount !== tr.doc.childCount || tr.doc.childCount === 1) return false;
  return tr.steps.every((step) => {
    if (!(step instanceof ReplaceStep)) return false;
    const { from, to, slice } = step as unknown as { from: number; to: number; slice: { openStart: number; content: { firstChild: Node | null } } };
    const $from = tr.before.resolve(from);
    const $to = tr.before.resolve(to);
    return $from.depth === 1 && $to.depth === 1 && $from.index(0) === $to.index(0) && (slice.content.firstChild?.isText ?? true);
  });
}

export const displayPlugin = new Plugin<Display>({
  key,
  state: {
    init: (_, state) => ({ folded: new Set(), decorations: decorate(state.doc, new Set()) }),
    apply(tr, value) {
      const fold = tr.getMeta(FOLD) as string | undefined;
      let folded = value.folded;
      if (fold !== undefined) {
        const next = new Set(folded);
        if (next.has(fold)) next.delete(fold);
        else next.add(fold);
        folded = next;
      }
      if (fold === undefined && (!tr.docChanged || onlyText(tr))) {
        return { folded, decorations: value.decorations.map(tr.mapping, tr.doc) };
      }
      return { folded, decorations: decorate(tr.doc, folded) };
    },
  },
  props: {
    decorations: (state) => key.getState(state)?.decorations ?? DecorationSet.empty,
    // A divider's text isn't shown, so typing with the cursor on one starts a new line below it.
    handleTextInput(view, from, to, text) {
      const $from = view.state.doc.resolve(from);
      if ($from.depth !== 1 || attrsOf($from.parent).kind.type !== "divider" || to !== from) return false;
      const after = $from.after(1);
      const para = blockType.create({ id: null, kind: { type: "paragraph" }, indent: attrsOf($from.parent).indent }, schema.text(text));
      const tr = view.state.tr.insert(after, para);
      view.dispatch(tr.setSelection(TextSelection.create(tr.doc, after + 1 + text.length)).scrollIntoView());
      return true;
    },
  },
});

/** Folds or unfolds a toggle. A cursor inside what folds away moves to the end of the toggle. */
export function toggleFold(view: EditorView, id: string): void {
  const { state } = view;
  const tr = state.tr.setMeta(FOLD, id).setMeta("addToHistory", false);
  const at = blockPos(state.doc, id);
  if (at !== null && !isFolded(state, id)) {
    const toggle = state.doc.nodeAt(at)!;
    const end = at + toggle.nodeSize;
    const hiddenTo = childrenEnd(state.doc, at);
    const { from, to } = state.selection;
    if (hiddenTo > end && to > end && from < hiddenTo) tr.setSelection(TextSelection.create(tr.doc, end - 1));
  }
  view.dispatch(tr);
}

/** Where block `id` starts, or null. */
function blockPos(doc: Node, id: string): number | null {
  let found: number | null = null;
  doc.forEach((node, pos) => {
    if (found === null && attrsOf(node).id === id) found = pos;
  });
  return found;
}

/** The end of the blocks nested under the block at `pos` (those after it, indented deeper). */
export function childrenEnd(doc: Node, pos: number): number {
  const indent = attrsOf(doc.nodeAt(pos)!).indent;
  let at = pos + doc.nodeAt(pos)!.nodeSize;
  for (let next = doc.nodeAt(at); next && attrsOf(next).indent > indent; next = doc.nodeAt(at)) at += next.nodeSize;
  return at;
}

export function isFolded(state: EditorState, id: string): boolean {
  return key.getState(state)?.folded.has(id) ?? false;
}

function control(tag: string, className: string): HTMLElement {
  const el = document.createElement(tag);
  el.className = className;
  el.contentEditable = "false";
  return el;
}

/** Kinds whose view has more than text. */
const CUSTOM: ReadonlySet<string> = new Set(["todo", "toggle", "callout", "divider", "query", "attachment", "database"]);

/** One view for every block: the text, plus whatever its kind shows next to the text. */
export function blockView(node: Node, view: EditorView, getPos: () => number | undefined): NodeView {
  const kind = attrsOf(node).kind;
  if (!CUSTOM.has(kind.type)) {
    // Plain blocks: just the schema's DOM, with attributes kept up to date.
    const spec = blockDom(node) as [string, Record<string, string>, ...unknown[]];
    const dom = document.createElement(spec[0]);
    for (const [k, v] of Object.entries(spec[1])) dom.setAttribute(k, v);
    const contentDOM = kind.type === "code" ? dom.appendChild(document.createElement("code")) : dom;
    return {
      dom,
      contentDOM,
      update(next) {
        if (next.type !== node.type || CUSTOM.has(attrsOf(next).kind.type)) return false;
        // A different element (heading level, code, quote) means a fresh view.
        const fresh = blockDom(next) as [string, Record<string, string>];
        if (fresh[0].toUpperCase() !== dom.tagName) return false;
        for (const [k, v] of Object.entries(fresh[1])) dom.setAttribute(k, v);
        if (!attrsOf(next).id) dom.removeAttribute("data-block-id");
        return true;
      },
    };
  }

  const dom = document.createElement("div");
  const contentDOM = document.createElement("div");
  contentDOM.className = "block-text";
  const setAttrs = (n: Node) => {
    const spec = blockDom(n) as [string, Record<string, string>];
    for (const [k, v] of Object.entries(spec[1])) dom.setAttribute(k, v);
  };
  setAttrs(node);
  const id = () => attrsOf(node).id;

  if (kind.type === "todo") {
    const box = control("input", "todo-box checkbox") as HTMLInputElement;
    box.type = "checkbox";
    box.checked = kind.checked;
    box.addEventListener("mousedown", (e) => e.preventDefault());
    // No preventDefault on click: for a checkbox it makes the browser restore the old state after
    // the handler runs, undoing what update() just showed. The model sets the state either way.
    box.addEventListener("click", () => {
      const pos = getPos();
      if (pos === undefined) return;
      const current = attrsOf(view.state.doc.nodeAt(pos)!).kind;
      if (current.type !== "todo") return;
      view.dispatch(view.state.tr.setNodeAttribute(pos, "kind", { type: "todo", checked: !current.checked }));
    });
    dom.append(box, contentDOM);
    return {
      dom,
      contentDOM,
      update(next) {
        const k = attrsOf(next).kind;
        if (k.type !== "todo") return false;
        node = next;
        box.checked = k.checked;
        setAttrs(next);
        return true;
      },
      stopEvent: (e) => e.target === box,
      ignoreMutation: (m) => m.target === box,
    };
  }

  if (kind.type === "query") return queryView(node, view, getPos, setAttrs);
  if (kind.type === "database") return databaseView(node, view, getPos);
  if (kind.type === "attachment") return attachmentView(node, setAttrs, dom, contentDOM);

  if (kind.type === "toggle") {
    const arrow = control("button", "toggle-arrow");
    arrow.setAttribute("aria-label", "Fold");
    arrow.innerHTML = iconSvg("chevron-right");
    const sync = () => {
      const folded = id() !== null && isFolded(view.state, id()!);
      arrow.setAttribute("aria-expanded", String(!folded));
    };
    sync();
    arrow.addEventListener("mousedown", (e) => e.preventDefault());
    arrow.addEventListener("click", () => {
      const current = id();
      if (current) toggleFold(view, current);
      sync();
    });
    dom.append(arrow, contentDOM);
    return {
      dom,
      contentDOM,
      update(next) {
        if (attrsOf(next).kind.type !== "toggle") return false;
        node = next;
        setAttrs(next);
        sync();
        return true;
      },
      stopEvent: (e) => e.target === arrow,
      ignoreMutation: (m) => m.target === arrow || arrow.contains(m.target as globalThis.Node),
    };
  }

  if (kind.type === "callout") {
    const icon = control("span", "callout-icon");
    icon.textContent = kind.icon || "💡";
    dom.append(icon, contentDOM);
    return {
      dom,
      contentDOM,
      update(next) {
        const k = attrsOf(next).kind;
        if (k.type !== "callout") return false;
        node = next;
        icon.textContent = k.icon || "💡";
        setAttrs(next);
        return true;
      },
      ignoreMutation: (m) => icon.contains(m.target as globalThis.Node),
    };
  }

  // Divider: a rule. Any text it holds (only possible through a concurrent edit) is kept but
  // not shown.
  dom.append(control("hr", "divider-rule"), contentDOM);
  contentDOM.classList.add("divider-text");
  return {
    dom,
    contentDOM,
    update(next) {
      if (attrsOf(next).kind.type !== "divider") return false;
      node = next;
      setAttrs(next);
      return true;
    },
  };
}
