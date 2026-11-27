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
import type { MarkType } from "prosemirror-model";
import { keymap } from "prosemirror-keymap";
import { type Plugin, type Command as PmCommand, type EditorState, TextSelection, type Transaction } from "prosemirror-state";

import type { BlockKind } from "../core/types";
import { childrenEnd, isFolded } from "./blocks";
import { attrsOf } from "./convert";
import { PICK_FILES } from "./attachments";
import { defaultQueryJson } from "./query";
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
  // At the end of a folded toggle, the new line goes after what the toggle hides, not into it.
  const node = state.doc.nodeAt(block.pos)!;
  const id = block.attrs.id;
  if (kind.type === "toggle" && id && isFolded(state, id) && state.selection.empty && state.selection.$from.parentOffset === node.content.size) {
    const at = childrenEnd(state.doc, block.pos);
    if (dispatch) {
      const tr = state.tr.insert(at, blockType.create({ id: null, kind: { type: "paragraph" }, indent }));
      dispatch(tr.setSelection(TextSelection.create(tr.doc, at + 1)).scrollIntoView());
    }
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
  // Backspace just after a divider removes the divider.
  const $pos = state.doc.resolve(block.pos);
  const prev = $pos.nodeBefore;
  if (prev && attrsOf(prev).kind.type === "divider") {
    dispatch?.(state.tr.delete(block.pos - prev.nodeSize, block.pos));
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

/** Mod-Enter in a code block: leaves it for a new paragraph below. */
const exitCode: PmCommand = (state, dispatch) => {
  const block = currentBlock(state);
  if (!block || block.attrs.kind.type !== "code") return false;
  if (dispatch) {
    const after = block.pos + state.doc.nodeAt(block.pos)!.nodeSize;
    const tr = state.tr.insert(after, blockType.create({ id: null, kind: { type: "paragraph" }, indent: block.attrs.indent }));
    dispatch(tr.setSelection(TextSelection.create(tr.doc, after + 1)).scrollIntoView());
  }
  return true;
};

/** Puts the cursor at the end of the page, on an empty paragraph, adding one if the last block isn't. */
export const focusEnd: PmCommand = (state, dispatch) => {
  const last = state.doc.lastChild!;
  const a = attrsOf(last);
  const tr = state.tr;
  if (last.content.size > 0 || a.kind.type !== "paragraph") {
    tr.insert(state.doc.content.size, blockType.create({ id: null, kind: { type: "paragraph" }, indent: 0 }));
  }
  dispatch?.(tr.setSelection(TextSelection.atEnd(tr.doc)).scrollIntoView());
  return true;
};

const insertNewline: PmCommand = (state, dispatch) => {
  dispatch?.(state.tr.insertText("\n").scrollIntoView());
  return true;
};

const marks = schema.marks;

/**
 * Like toggleMark, for a mark that doesn't grow when you type at its end (inline code). Typing
 * there is already plain, but the mark looks on, so the shortcut must turn it off, not on again.
 */
function toggleClosedMark(type: MarkType): PmCommand {
  const toggle = toggleMark(type);
  return (state, dispatch) => {
    const { empty, $cursor } = state.selection as TextSelection;
    if (empty && $cursor && !state.storedMarks && $cursor.nodeBefore && type.isInSet($cursor.nodeBefore.marks) && !type.isInSet($cursor.marks())) {
      dispatch?.(state.tr.setStoredMarks($cursor.marks()));
      return true;
    }
    return toggle(state, dispatch);
  };
}

export function editorKeymap(): Plugin {
  return keymap({
    Enter: enter,
    "Shift-Enter": insertNewline,
    "Mod-Enter": chainCommands(toggleTodo, exitCode),
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
    "Mod-e": toggleClosedMark(marks.code!),
    "Mod-Alt-0": setKind({ type: "paragraph" }),
    "Mod-Alt-1": setKind({ type: "heading", level: "h1" }),
    "Mod-Alt-2": setKind({ type: "heading", level: "h2" }),
    "Mod-Alt-3": setKind({ type: "heading", level: "h3" }),
  });
}

/** What the phone toolbar does: the same commands the keyboard shortcuts run. */
export const toolbar = {
  indent: indentBy(1),
  outdent: indentBy(-1),
  bold: toggleMark(marks.bold!),
  italic: toggleMark(marks.italic!),
  undo,
  redo,
  /** Paragraph → H1 → H2 → H3 → paragraph, by the kind of the block the cursor is in. */
  heading: ((state, dispatch) => {
    const block = currentBlock(state);
    const kind = block?.attrs.kind;
    const order = ["h1", "h2", "h3"] as const;
    const next: BlockKind =
      kind?.type !== "heading"
        ? { type: "heading", level: "h1" }
        : kind.level === "h3"
          ? { type: "paragraph" }
          : { type: "heading", level: order[order.indexOf(kind.level) + 1]! };
    return setKind(next)(state, dispatch);
  }) as PmCommand,
  /** Turns the selected blocks into `kind`, or back into paragraphs if they all are already. */
  toggleKind(type: "bulleted" | "todo"): PmCommand {
    return (state, dispatch) => {
      const kind = currentBlock(state)?.attrs.kind;
      if (kind?.type === type) return setKind({ type: "paragraph" })(state, dispatch);
      return setKind(type === "todo" ? { type: "todo", checked: false } : { type: "bulleted" })(
        state,
        dispatch,
      );
    };
  },
};

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
      new InputRule(/^---$/, (state, _match, start, end) => {
        // A divider holds no text you can see, so the cursor goes on to a new line below it.
        const $start = state.doc.resolve(start);
        if ($start.parentOffset !== 0) return null;
        const pos = $start.before(1);
        const block = state.doc.nodeAt(pos)!;
        const tr = state.tr.delete(start, end);
        tr.setNodeAttribute(pos, "kind", { type: "divider" });
        const rest = tr.doc.nodeAt(pos)!;
        // Text after the `---` moves to the new line rather than hiding inside the divider.
        const text = rest.content;
        if (text.size > 0) tr.delete(pos + 1, pos + 1 + text.size);
        const para = blockType.create({ id: null, kind: { type: "paragraph" }, indent: attrsOf(block).indent }, text);
        const at = pos + tr.doc.nodeAt(pos)!.nodeSize;
        tr.insert(at, para);
        return tr.setSelection(TextSelection.create(tr.doc, at + 1));
      }),
      rule(/^!\s$/, () => ({ type: "callout", icon: "💡" })),
      // `/filter` and a space makes a filter block, with a paragraph after it to carry on in.
      new InputRule(/^\/filter\s$/, (state, _match, start, end) => {
        const $start = state.doc.resolve(start);
        if ($start.parentOffset !== 0 || $start.parent.content.size !== end - start) return null;
        const pos = $start.before(1);
        const block = state.doc.nodeAt(pos)!;
        const tr = state.tr.delete(start, end);
        tr.setNodeAttribute(pos, "kind", { type: "query", query: defaultQueryJson() });
        const at = pos + tr.doc.nodeAt(pos)!.nodeSize;
        tr.insert(at, blockType.create({ id: null, kind: { type: "paragraph" }, indent: attrsOf(block).indent }));
        return tr.setSelection(TextSelection.create(tr.doc, at + 1));
      }),
      rule(/^>\s$/, () => ({ type: "toggle" })),
      // `/file` and a space opens the file picker; what's chosen goes on the page.
      new InputRule(/^\/file\s$/, (state, _match, start, end) => {
        const $start = state.doc.resolve(start);
        if ($start.parentOffset !== 0 || $start.parent.content.size !== end - start) return null;
        return state.tr.delete(start, end).setMeta(PICK_FILES, true);
      }),
    ],
  });
}
