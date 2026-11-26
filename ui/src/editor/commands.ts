// SPDX-License-Identifier: AGPL-3.0-or-later

// Keys and Markdown shortcuts. Every command here is an ordinary ProseMirror transaction; the
// binding turns the result into the core's edits, so nothing here talks to the core.

import {
  baseKeymap,
  chainCommands,
  deleteSelection,
  joinBackward,
  joinForward,
  selectNodeBackward,
  splitBlockAs,
  toggleMark,
} from "prosemirror-commands";
import { redo, undo } from "prosemirror-history";
import { InputRule, inputRules } from "prosemirror-inputrules";
import { keymap } from "prosemirror-keymap";
import type { Plugin, Command as PmCommand, EditorState, Transaction } from "prosemirror-state";

import type { BlockKind } from "../core/types";
import { attrsOf } from "./convert";
import { type BlockAttrs, MAX_INDENT, blockType, schema } from "./schema";

const LISTS = new Set<BlockKind["type"]>(["bulleted", "numbered", "todo"]);

/** The kind a new block gets when Enter is pressed at the end of a block of `kind`. */
function nextKind(kind: BlockKind): BlockKind {
  if (kind.type === "todo") return { type: "todo", checked: false };
  if (LISTS.has(kind.type)) return kind;
  return { type: "paragraph" };
}

function currentBlock(state: EditorState): { pos: number; attrs: BlockAttrs; empty: boolean } | null {
  const { $from } = state.selection;
  if ($from.depth < 1) return null;
  const node = $from.node(1);
  return { pos: $from.before(1), attrs: attrsOf(node), empty: node.content.size === 0 };
}

function setAttrs(tr: Transaction, pos: number, attrs: Partial<BlockAttrs>): Transaction {
  for (const [key, value] of Object.entries(attrs)) tr.setNodeAttribute(pos, key, value);
  return tr;
}

/** Enter. An empty list item stops the list (or outdents); otherwise the block splits. */
const enter: PmCommand = (state, dispatch) => {
  const block = currentBlock(state);
  if (!block) return false;
  const { kind, indent } = block.attrs;
  if (kind.type === "code") {
    dispatch?.(state.tr.insertText("\n").scrollIntoView());
    return true;
  }
  if (block.empty && kind.type !== "paragraph") {
    const tr = state.tr;
    if (indent > 0 && LISTS.has(kind.type)) setAttrs(tr, block.pos, { indent: indent - 1 });
    else setAttrs(tr, block.pos, { kind: { type: "paragraph" } });
    dispatch?.(tr);
    return true;
  }
  const atStart = state.selection.empty && state.selection.$from.parentOffset === 0 && !block.empty;
  const split = splitBlockAs((node, atEnd) => {
    const a = attrsOf(node);
    const k = atEnd ? nextKind(a.kind) : a.kind.type === "todo" ? { type: "todo" as const, checked: false } : a.kind;
    return { type: blockType, attrs: { id: null, kind: atStart ? a.kind : k, indent: a.indent } };
  });
  return split(state, (tr) => {
    // Enter at the start of a heading leaves an empty paragraph above it, not an empty heading.
    if (atStart && !LISTS.has(kind.type)) setAttrs(tr, block.pos, { kind: { type: "paragraph" } });
    dispatch?.(tr.scrollIntoView());
  });
};

/** Backspace at the start of a block first turns it into a paragraph, then outdents, then joins. */
const backspaceAtStart: PmCommand = (state, dispatch) => {
  const { selection } = state;
  if (!selection.empty || selection.$from.parentOffset !== 0) return false;
  const block = currentBlock(state);
  if (!block) return false;
  if (block.attrs.kind.type !== "paragraph") {
    dispatch?.(setAttrs(state.tr, block.pos, { kind: { type: "paragraph" } }));
    return true;
  }
  if (block.attrs.indent > 0) {
    dispatch?.(setAttrs(state.tr, block.pos, { indent: block.attrs.indent - 1 }));
    return true;
  }
  return false;
};

/** Tab and Shift-Tab: indent or outdent the selected blocks, never more than one level deeper than the block before. */
function indentBy(delta: number): PmCommand {
  return (state, dispatch) => {
    const { from, to } = state.selection;
    const tr = state.tr;
    let prev = -1;
    let changed = false;
    state.doc.forEach((node, pos) => {
      const a = attrsOf(node);
      const inside = pos + node.nodeSize > from && pos <= to;
      const next = inside ? Math.max(0, Math.min(a.indent + delta, prev + 1, MAX_INDENT)) : a.indent;
      if (next !== a.indent) {
        tr.setNodeAttribute(pos, "indent", next);
        changed = true;
      }
      prev = next;
    });
    if (changed) dispatch?.(tr);
    return true;
  };
}

/** Changes the kind of every selected block. */
export function setKind(kind: BlockKind): PmCommand {
  return (state, dispatch) => {
    const { from, to } = state.selection;
    const tr = state.tr;
    state.doc.nodesBetween(from, to, (node, pos) => {
      if (node.type === blockType) tr.setNodeAttribute(pos, "kind", kind);
      return false;
    });
    dispatch?.(tr);
    return true;
  };
}

/** Mod-Enter: ticks or unticks the selected to-dos. All end up ticked unless all already are. */
const toggleTodo: PmCommand = (state, dispatch) => {
  const { from, to } = state.selection;
  const todos: { pos: number; checked: boolean }[] = [];
  state.doc.nodesBetween(from, to, (node, pos) => {
    const k = attrsOf(node).kind;
    if (node.type === blockType && k.type === "todo") todos.push({ pos, checked: k.checked });
    return false;
  });
  if (todos.length === 0) return false;
  const checked = !todos.every((t) => t.checked);
  const tr = state.tr;
  for (const t of todos) tr.setNodeAttribute(t.pos, "kind", { type: "todo", checked });
  dispatch?.(tr);
  return true;
};

const insertNewline: PmCommand = (state, dispatch) => {
  dispatch?.(state.tr.insertText("\n").scrollIntoView());
  return true;
};

const marks = schema.marks;

export function editorKeymap(): Plugin {
  return keymap({
    Enter: enter,
    "Shift-Enter": insertNewline,
    "Mod-Enter": toggleTodo,
    Backspace: chainCommands(deleteSelection, backspaceAtStart, joinBackward, selectNodeBackward),
    Delete: chainCommands(deleteSelection, joinForward),
    Tab: indentBy(1),
    "Shift-Tab": indentBy(-1),
    "Mod-z": undo,
    "Mod-Shift-z": redo,
    "Mod-y": redo,
    "Mod-b": toggleMark(marks.bold!),
    "Mod-i": toggleMark(marks.italic!),
    "Mod-u": toggleMark(marks.underline!),
    "Mod-Shift-x": toggleMark(marks.strikethrough!),
    "Mod-e": toggleMark(marks.code!),
    "Mod-Alt-0": setKind({ type: "paragraph" }),
    "Mod-Alt-1": setKind({ type: "heading", level: "h1" }),
    "Mod-Alt-2": setKind({ type: "heading", level: "h2" }),
    "Mod-Alt-3": setKind({ type: "heading", level: "h3" }),
  });
}

export function baseKeys(): Plugin {
  return keymap(baseKeymap);
}

/** Markdown shortcuts at the start of a block, as in Notion: `# `, `- `, `1. `, `[] `, `" ` (quote), `> ` (toggle), ` ``` `, `---`. */
export function markdownShortcuts(): Plugin {
  const rule = (pattern: RegExp, kind: (m: RegExpMatchArray) => BlockKind) =>
    new InputRule(pattern, (state, match, start, end) => {
      const $start = state.doc.resolve(start);
      if ($start.parentOffset !== 0) return null;
      const tr = state.tr.delete(start, end);
      tr.setNodeAttribute($start.before(1), "kind", kind(match));
      return tr;
    });
  return inputRules({
    rules: [
      rule(/^(#{1,3})\s$/, (m) => ({ type: "heading", level: `h${m[1]!.length}` as "h1" | "h2" | "h3" })),
      rule(/^[-*]\s$/, () => ({ type: "bulleted" })),
      rule(/^1[.)]\s$/, () => ({ type: "numbered" })),
      rule(/^\[( |x)?\]\s$/, (m) => ({ type: "todo", checked: m[1] === "x" })),
      rule(/^"\s$/, () => ({ type: "quote" })),
      rule(/^```$/, () => ({ type: "code", language: "" })),
      rule(/^---$/, () => ({ type: "divider" })),
      rule(/^!\s$/, () => ({ type: "callout", icon: "💡" })),
      rule(/^>\s$/, () => ({ type: "toggle" })),
    ],
  });
}
