// SPDX-License-Identifier: AGPL-3.0-or-later

// The editor's schema: a page is a flat list of blocks, exactly as the core stores it. Every
// block is the same node type; what kind of block it is (paragraph, heading, to-do, …) is an
// attribute, as in the core, so changing it never replaces the node.

import { type DOMOutputSpec, type Mark, type Node, Schema } from "prosemirror-model";

import type { BlockKind } from "../core/types";

export interface BlockAttrs {
  /** The core's ID for the block, a temporary `tmp-…` one until the core has made it, or null. */
  id: string | null;
  kind: BlockKind;
  indent: number;
}

export const MAX_INDENT = 8;

function kindFromDom(el: HTMLElement): BlockKind | null {
  const raw = el.getAttribute("data-kind");
  if (!raw) return null;
  try {
    return JSON.parse(raw) as BlockKind;
  } catch {
    return null;
  }
}

function blockAttrs(kind: BlockKind) {
  return (el: HTMLElement): BlockAttrs => ({
    id: el.getAttribute("data-block-id"),
    kind: kindFromDom(el) ?? kind,
    indent: Number(el.getAttribute("data-indent") ?? 0) || 0,
  });
}

export function blockDom(node: Node): DOMOutputSpec {
  const { id, kind, indent } = node.attrs as BlockAttrs;
  const attrs: Record<string, string> = {
    class: `block block-${kind.type}`,
    "data-kind": JSON.stringify(kind),
    "data-indent": String(indent),
  };
  if (id) attrs["data-block-id"] = id;
  switch (kind.type) {
    case "heading":
      return [kind.level, attrs, 0];
    case "quote":
      return ["blockquote", attrs, 0];
    case "code":
      return ["pre", attrs, ["code", 0]];
    default:
      return ["div", attrs, 0];
  }
}

export const schema = new Schema({
  nodes: {
    doc: { content: "block+" },
    block: {
      content: "text*",
      marks: "_",
      attrs: {
        id: { default: null },
        kind: { default: { type: "paragraph" } },
        indent: { default: 0 },
      },
      defining: true,
      toDOM: blockDom,
      parseDOM: [
        { tag: "[data-kind]", getAttrs: blockAttrs({ type: "paragraph" }) },
        { tag: "h1", getAttrs: blockAttrs({ type: "heading", level: "h1" }) },
        { tag: "h2", getAttrs: blockAttrs({ type: "heading", level: "h2" }) },
        { tag: "h3", getAttrs: blockAttrs({ type: "heading", level: "h3" }) },
        { tag: "h4", getAttrs: blockAttrs({ type: "heading", level: "h3" }) },
        { tag: "blockquote", getAttrs: blockAttrs({ type: "quote" }) },
        { tag: "pre", getAttrs: blockAttrs({ type: "code", language: "" }), preserveWhitespace: "full" },
        { tag: "li", getAttrs: blockAttrs({ type: "bulleted" }) },
        { tag: "p", getAttrs: blockAttrs({ type: "paragraph" }) },
      ],
    },
    text: {},
  },
  // `inclusive` matches how the core's marks grow: typing at the end of bold text continues
  // it; code and links don't grow.
  marks: {
    link: {
      attrs: { href: {} },
      inclusive: false,
      toDOM: (m: Mark) => ["a", { href: String(m.attrs.href), rel: "noopener noreferrer", target: "_blank" }, 0],
      parseDOM: [{ tag: "a[href]", getAttrs: (el: HTMLElement) => ({ href: el.getAttribute("href") }) }],
    },
    bold: {
      toDOM: () => ["strong", 0],
      parseDOM: [{ tag: "strong" }, { tag: "b" }, { style: "font-weight=bold" }, { style: "font-weight=700" }],
    },
    italic: {
      toDOM: () => ["em", 0],
      parseDOM: [{ tag: "em" }, { tag: "i" }, { style: "font-style=italic" }],
    },
    underline: {
      toDOM: () => ["u", 0],
      parseDOM: [{ tag: "u" }, { style: "text-decoration=underline" }],
    },
    strikethrough: {
      toDOM: () => ["s", 0],
      parseDOM: [{ tag: "s" }, { tag: "del" }, { style: "text-decoration=line-through" }],
    },
    code: {
      inclusive: false,
      toDOM: () => ["code", 0],
      parseDOM: [{ tag: "code" }],
    },
  },
});

export const blockType = schema.nodes.block!;

/** The mark keys the core knows, in the order the core reports them. */
export const MARK_KEYS = ["bold", "italic", "underline", "strikethrough", "code", "link"] as const;
export type MarkKey = (typeof MARK_KEYS)[number];

/** Marks that grow when you type at their end, as in the core. */
export const EXPANDS_AFTER: ReadonlySet<MarkKey> = new Set(["bold", "italic", "underline", "strikethrough"]);
