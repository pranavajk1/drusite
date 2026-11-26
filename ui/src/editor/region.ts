// SPDX-License-Identifier: AGPL-3.0-or-later

// Which blocks a set of transactions touched. The binding looks at those blocks and nothing
// else, so a keystroke costs the same in a 10-block page and a 10,000-block one.

import type { Node } from "prosemirror-model";
import { Mapping, type Step } from "prosemirror-transform";

/** The same stretch of blocks before and after: `old[start..oldEnd)` became `new[start..newEnd)`. */
export interface Region {
  start: number;
  oldEnd: number;
  newEnd: number;
}

/** Positions a step touched, in the document as it was before that step. */
function stepPositions(step: Step): number[] {
  const out: number[] = [];
  step.getMap().forEach((oldStart, oldEnd) => out.push(oldStart, oldEnd));
  // Mark and attribute steps change nothing's size, so their map is empty.
  const json = step.toJSON() as { from?: unknown; to?: unknown; pos?: unknown };
  for (const p of [json.from, json.to, json.pos]) if (typeof p === "number") out.push(p);
  return out;
}

/**
 * The blocks `steps` (taking `oldDoc` to `newDoc`) touched, with one unchanged block either side
 * so joins and splits at the edges are seen whole. `null` if nothing changed.
 */
export function changedRegion(oldDoc: Node, newDoc: Node, steps: readonly Step[]): Region | null {
  if (steps.length === 0) return null;
  const mapping = new Mapping(steps.map((s) => s.getMap()));
  let oldLo = Infinity;
  let oldHi = -Infinity;
  let newLo = Infinity;
  let newHi = -Infinity;
  steps.forEach((step, i) => {
    const toOld = mapping.slice(0, i).invert();
    const toNew = mapping.slice(i);
    for (const p of stepPositions(step)) {
      const o = toOld.map(p, -1);
      const o2 = toOld.map(p, 1);
      const n = toNew.map(p, -1);
      const n2 = toNew.map(p, 1);
      oldLo = Math.min(oldLo, o, o2);
      oldHi = Math.max(oldHi, o, o2);
      newLo = Math.min(newLo, n, n2);
      newHi = Math.max(newHi, n, n2);
    }
  });
  if (!Number.isFinite(oldLo)) return null;

  const index = (doc: Node, pos: number) => doc.resolve(Math.max(0, Math.min(pos, doc.content.size))).index(0);
  const oldCount = oldDoc.childCount;
  const newCount = newDoc.childCount;
  // Blocks before the first change are the same in both documents, and so are blocks after the
  // last one, counted from the end.
  const before = Math.max(0, Math.min(index(oldDoc, oldLo), index(newDoc, newLo)) - 1);
  const oldLast = Math.min(oldCount - 1, index(oldDoc, oldHi));
  const newLast = Math.min(newCount - 1, index(newDoc, newHi));
  const after = Math.max(0, Math.min(oldCount - 1 - oldLast, newCount - 1 - newLast) - 1);
  return { start: before, oldEnd: oldCount - after, newEnd: newCount - after };
}

/** The top-level blocks from `from` to `to`. */
export function blocksIn(doc: Node, from: number, to: number): Node[] {
  const out: Node[] = [];
  for (let i = from; i < to; i++) out.push(doc.child(i));
  return out;
}
