// SPDX-License-Identifier: AGPL-3.0-or-later

// Turning a change ProseMirror made to a few blocks into the core's edits.
//
// The core keeps the meaning of an edit, not just its result: Enter is a split, Backspace at the
// start of a block is a join, dragging a block is a move. Those are what make concurrent edits
// from other devices merge the way people expect, so this module recognises them instead of
// describing every change as "delete this, insert that":
//
// - a block that vanished and whose text now ends the block before it was joined onto it;
// - a new block right after one whose text was cut at exactly that point was split from it;
// - blocks in a new order were moved (as few as possible);
// - everything else is typing, formatting, kind and indent changes on blocks that stayed.
//
// It keeps a small model of what the core will hold after each edit, so it can tell which
// formatting the core already applied (marks grow over typed text in the core too) and only
// sends the rest.

import type { Node } from "prosemirror-model";

import type { BlockKind, Edit } from "../core/types";
import {
  type MarkSet,
  NO_MARKS,
  attrsOf,
  coreMark,
  sameKind,
  sameMarks,
  unitsOfNode,
} from "./convert";
import { EXPANDS_AFTER, MARK_KEYS } from "./schema";

/** An edit, and the local ID of the block it creates (splits and inserted blocks). */
export interface LocalEdit {
  edit: Edit;
  creates?: string;
}

interface ModelBlock {
  id: string;
  kind: BlockKind;
  indent: number;
  text: string;
  marks: MarkSet[];
}

interface Target {
  id: string;
  kind: BlockKind;
  indent: number;
  text: string;
  marks: MarkSet[];
}

function target(node: Node): Target {
  const a = attrsOf(node);
  if (!a.id) throw new Error("block without an ID");
  return { id: a.id, kind: a.kind, indent: a.indent, ...unitsOfNode(node) };
}

function isHigh(code: number): boolean {
  return code >= 0xd800 && code <= 0xdbff;
}

/** The edits that turn `oldBlocks` into `newBlocks`, the same stretch of a page before and after. */
export function diffBlocks(oldBlocks: readonly Node[], newBlocks: readonly Node[]): LocalEdit[] {
  const out: LocalEdit[] = [];
  const model: ModelBlock[] = oldBlocks.map((n) => ({ ...target(n) }));
  const want = newBlocks.map(target);
  const wantById = new Map(want.map((t) => [t.id, t]));
  const at = (id: string) => model.findIndex((b) => b.id === id);

  // 1. Blocks that are gone: joined onto the block before them, or deleted.
  for (const gone of model.filter((b) => !wantById.has(b.id))) {
    const i = at(gone.id);
    const prev = i > 0 ? model[i - 1] : undefined;
    const prevWant = prev && wantById.get(prev.id);
    if (prev && prevWant && prevWant.text === prev.text + gone.text) {
      out.push({ edit: { type: "join", block: gone.id } });
      prev.text += gone.text;
      prev.marks = prev.marks.concat(gone.marks);
    } else {
      out.push({ edit: { type: "deleteBlock", block: gone.id } });
    }
    model.splice(i, 1);
  }

  // 2. Blocks that stayed but changed places: move the fewest, keeping the longest run that is
  // already in order.
  const stayed = want.filter((t) => at(t.id) >= 0).map((t) => t.id);
  const keep = longestInOrder(stayed.map(at));
  stayed.forEach((id, i) => {
    if (keep.has(i)) return;
    const after = i > 0 ? stayed[i - 1]! : null;
    out.push({ edit: { type: "moveBlock", block: id, after } });
    const block = model.splice(at(id), 1)[0]!;
    model.splice(after === null ? 0 : at(after) + 1, 0, block);
  });

  // 3. New blocks: split from the block before them, or inserted.
  want.forEach((t, j) => {
    if (at(t.id) >= 0) return;
    const prevId = j > 0 ? want[j - 1]!.id : null;
    const prev = prevId === null ? undefined : model[at(prevId)];
    const prevWant = prevId === null ? undefined : wantById.get(prevId);
    if (prev && prevWant && prev.text === prevWant.text + t.text && !isHigh(prev.text.charCodeAt(prevWant.text.length - 1))) {
      const pos = prevWant.text.length;
      out.push({ edit: { type: "split", block: prev.id, pos, kind: t.kind }, creates: t.id });
      model.splice(at(prev.id) + 1, 0, {
        id: t.id,
        kind: t.kind,
        indent: prev.indent,
        text: prev.text.slice(pos),
        marks: prev.marks.slice(pos),
      });
      prev.text = prev.text.slice(0, pos);
      prev.marks = prev.marks.slice(0, pos);
    } else {
      out.push({ edit: { type: "insertBlock", after: prevId, kind: t.kind }, creates: t.id });
      model.splice(prevId === null ? 0 : at(prevId) + 1, 0, {
        id: t.id,
        kind: t.kind,
        indent: prev?.indent ?? 0,
        text: "",
        marks: [],
      });
    }
  });

  // 4. Each block's own changes: kind, indent, text, formatting.
  for (const t of want) {
    const b = model[at(t.id)]!;
    if (!sameKind(b.kind, t.kind)) {
      out.push({ edit: { type: "setKind", block: t.id, kind: t.kind } });
      b.kind = t.kind;
    }
    if (b.indent !== t.indent) {
      out.push({ edit: { type: "setIndent", block: t.id, indent: t.indent } });
      b.indent = t.indent;
    }
    diffText(b, t, out);
    diffMarks(b, t, out);
  }
  return out;
}

/** One delete and one insert covering what differs between the model's text and the target's. */
function diffText(b: ModelBlock, t: Target, out: LocalEdit[]): void {
  if (b.text === t.text) return;
  const a = b.text;
  const c = t.text;
  let pre = 0;
  while (pre < a.length && pre < c.length && a.charCodeAt(pre) === c.charCodeAt(pre)) pre++;
  // Never cut between the halves of a surrogate pair.
  if (pre > 0 && isHigh(a.charCodeAt(pre - 1))) pre--;
  let suf = 0;
  while (suf < a.length - pre && suf < c.length - pre && a.charCodeAt(a.length - 1 - suf) === c.charCodeAt(c.length - 1 - suf)) suf++;
  if (suf > 0 && suf < a.length - pre && isHigh(a.charCodeAt(a.length - suf - 1))) suf--;

  const del = a.length - pre - suf;
  if (del > 0) {
    out.push({ edit: { type: "deleteText", block: b.id, pos: pre, len: del } });
    b.text = a.slice(0, pre) + a.slice(pre + del);
    b.marks.splice(pre, del);
  }
  const ins = c.slice(pre, c.length - suf);
  if (ins.length === 0) return;
  out.push({ edit: { type: "insertText", block: b.id, pos: pre, text: ins } });

  // What the core gives the typed text: marks inside a range always cover it, and marks that
  // grow (bold, italic, …) cover text typed at their end. At the start of a block nothing is
  // inherited (the core keeps block markers unformatted). Whatever differs from what ProseMirror
  // shows is corrected by the formatting diff that follows.
  const before = pre > 0 ? b.marks[pre - 1]! : null;
  const after = pre < b.marks.length ? b.marks[pre]! : null;
  const inherited = new Map<(typeof MARK_KEYS)[number], string | true>();
  for (const key of MARK_KEYS) {
    const v = before?.get(key);
    if (v === undefined) continue;
    if (after?.get(key) === v || EXPANDS_AFTER.has(key)) inherited.set(key, v);
  }
  b.text = b.text.slice(0, pre) + ins + b.text.slice(pre);
  b.marks.splice(pre, 0, ...Array.from({ length: ins.length }, () => inherited));
}

/** Format edits for every run where the model's formatting differs from the target's. */
function diffMarks(b: ModelBlock, t: Target, out: LocalEdit[]): void {
  for (const key of MARK_KEYS) {
    let i = 0;
    while (i < t.marks.length) {
      const want = t.marks[i]!.get(key);
      const have = b.marks[i]?.get(key);
      if (want === have) {
        i++;
        continue;
      }
      let j = i + 1;
      while (j < t.marks.length && t.marks[j]!.get(key) === want && b.marks[j]?.get(key) !== want) j++;
      out.push({
        edit: {
          type: "format",
          block: b.id,
          start: i,
          end: j,
          mark: coreMark(key, want ?? true),
          on: want !== undefined,
        },
      });
      for (let k = i; k < j; k++) {
        const next = new Map(b.marks[k] ?? NO_MARKS);
        if (want === undefined) next.delete(key);
        else next.set(key, want);
        b.marks[k] = next;
      }
      i = j;
    }
  }
  if (b.marks.some((m, i) => !sameMarks(m, t.marks[i] ?? NO_MARKS))) {
    throw new Error("formatting diff didn't converge");
  }
}

/** Indices of a longest strictly increasing subsequence of `xs`. */
function longestInOrder(xs: readonly number[]): Set<number> {
  const tails: number[] = [];
  const prev: number[] = new Array<number>(xs.length).fill(-1);
  xs.forEach((x, i) => {
    let lo = 0;
    let hi = tails.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (xs[tails[mid]!]! < x) lo = mid + 1;
      else hi = mid;
    }
    if (lo > 0) prev[i] = tails[lo - 1]!;
    tails[lo] = i;
  });
  const keep = new Set<number>();
  let k = tails.length > 0 ? tails[tails.length - 1]! : -1;
  while (k >= 0) {
    keep.add(k);
    k = prev[k]!;
  }
  return keep;
}
