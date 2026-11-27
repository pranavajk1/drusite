// SPDX-License-Identifier: AGPL-3.0-or-later

// Converting between the core's blocks and ProseMirror nodes, and the per-character view of a
// block's formatting that the binding compares.

import type { Mark as PmMark, Node } from "prosemirror-model";

import type { Block, BlockKind, Mark, Marks, Span } from "../core/types";
import { type BlockAttrs, MARK_KEYS, type MarkKey, blockType, schema } from "./schema";

/** Formatting as a map from mark key to value (`true`, or the link's href). */
export type MarkSet = ReadonlyMap<MarkKey, string | true>;

export const NO_MARKS: MarkSet = new Map();

export function marksFromCore(marks: Marks): MarkSet {
  const out = new Map<MarkKey, string | true>();
  if (marks.bold) out.set("bold", true);
  if (marks.italic) out.set("italic", true);
  if (marks.underline) out.set("underline", true);
  if (marks.strikethrough) out.set("strikethrough", true);
  if (marks.code) out.set("code", true);
  if (marks.link !== null) out.set("link", marks.link);
  return out;
}

export function marksFromPm(marks: readonly PmMark[]): MarkSet {
  const out = new Map<MarkKey, string | true>();
  for (const m of marks) {
    const key = m.type.name as MarkKey;
    out.set(key, key === "link" ? String(m.attrs.href) : true);
  }
  return out;
}

export function pmMarks(set: MarkSet): PmMark[] {
  const out: PmMark[] = [];
  for (const key of MARK_KEYS) {
    const value = set.get(key);
    if (value === undefined) continue;
    out.push(key === "link" ? schema.marks.link!.create({ href: value }) : schema.marks[key]!.create());
  }
  return out;
}

export function coreMark(key: MarkKey, value: string | true): Mark {
  return key === "link" ? { type: "link", href: String(value) } : { type: key };
}

export function sameMarks(a: MarkSet, b: MarkSet): boolean {
  if (a.size !== b.size) return false;
  for (const [k, v] of a) if (b.get(k) !== v) return false;
  return true;
}

/** A block's text with the formatting of each UTF-16 code unit. */
export interface Units {
  text: string;
  marks: MarkSet[];
}

export function unitsOfNode(node: Node): Units {
  let text = "";
  const marks: MarkSet[] = [];
  node.forEach((child) => {
    const t = child.text ?? "";
    const set = marksFromPm(child.marks);
    text += t;
    for (let i = 0; i < t.length; i++) marks.push(set);
  });
  return { text, marks };
}

export function unitsOfSpans(spans: Span[]): Units {
  let text = "";
  const marks: MarkSet[] = [];
  for (const span of spans) {
    const set = marksFromCore(span.marks);
    text += span.text;
    for (let i = 0; i < span.text.length; i++) marks.push(set);
  }
  return { text, marks };
}

export function blockNode(block: Block): Node {
  const content = block.spans
    .filter((s) => s.text.length > 0)
    .map((s) => schema.text(s.text, pmMarks(marksFromCore(s.marks))));
  const attrs: BlockAttrs = { id: block.id, kind: block.kind, indent: block.indent };
  return blockType.create(attrs, content);
}

export function docNode(blocks: Block[]): Node {
  const nodes = blocks.length > 0 ? blocks.map(blockNode) : [blockType.create()];
  return schema.node("doc", null, nodes);
}

export function attrsOf(node: Node): BlockAttrs {
  return node.attrs as BlockAttrs;
}

export function sameKind(a: BlockKind, b: BlockKind): boolean {
  switch (a.type) {
    case "heading":
      return b.type === "heading" && a.level === b.level;
    case "todo":
      return b.type === "todo" && a.checked === b.checked;
    case "code":
      return b.type === "code" && a.language === b.language;
    case "callout":
      return b.type === "callout" && a.icon === b.icon;
    case "query":
      return b.type === "query" && a.query === b.query;
    case "database":
      return b.type === "database" && a.database === b.database && a.view === b.view;
    case "attachment":
      return b.type === "attachment" && a.blob === b.blob && a.name === b.name && a.mime === b.mime && a.size === b.size;
    default:
      return a.type === b.type;
  }
}
