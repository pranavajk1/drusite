// SPDX-License-Identifier: AGPL-3.0-or-later

// The editor's schema: a page is a flat list of blocks, exactly as the core stores it. Every
// block is the same node type; what kind of block it is (paragraph, heading, to-do, …) is an
// attribute, as in the core, so changing it never replaces the node.

import { type DOMOutputSpec, DOMParser, type Mark, type Node, type ParseRule, Schema, type TagParseRule } from "prosemirror-model";

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

function hasParagraphs(el: HTMLElement): boolean {
  return [...el.children].some((c) => c.tagName === "P");
}

/** A pasted list item: numbered inside `<ol>`, a to-do with a checkbox, indented by how deeply it's nested. */
function listItemAttrs(li: HTMLElement): BlockAttrs {
  if (li.hasAttribute("data-kind")) return blockAttrs({ type: "bulleted" })(li);
  const box = li.querySelector<HTMLInputElement>(":scope > input[type=checkbox], :scope > p > input[type=checkbox]");
  let depth = -1;
  for (let p: HTMLElement | null = li; p; p = p.parentElement) if (p.tagName === "UL" || p.tagName === "OL") depth++;
  const kind: BlockKind = box
    ? { type: "todo", checked: box.checked || box.hasAttribute("checked") }
    : li.parentElement?.tagName === "OL"
      ? { type: "numbered" }
      : { type: "bulleted" };
  return { id: null, kind, indent: Math.max(0, Math.min(depth, MAX_INDENT)) };
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
      // Line breaks (Shift-Enter, and every line of a code block) are text. Without this, when the
      // browser edits a block's DOM directly and ProseMirror reads it back, they turn into spaces.
      whitespace: "pre",
      toDOM: blockDom,
      parseDOM: [
        { tag: "[data-kind]", getAttrs: blockAttrs({ type: "paragraph" }) },
        { tag: "h1", getAttrs: blockAttrs({ type: "heading", level: "h1" }) },
        { tag: "h2", getAttrs: blockAttrs({ type: "heading", level: "h2" }) },
        { tag: "h3", getAttrs: blockAttrs({ type: "heading", level: "h3" }) },
        { tag: "h4", getAttrs: blockAttrs({ type: "heading", level: "h3" }) },
        { tag: "h5", getAttrs: blockAttrs({ type: "heading", level: "h3" }) },
        { tag: "h6", getAttrs: blockAttrs({ type: "heading", level: "h3" }) },
        // A quote or list item holding paragraphs (as many apps paste them) becomes one block per
        // paragraph, each keeping the quote's or the item's kind.
        { tag: "blockquote", getAttrs: (el: HTMLElement) => (hasParagraphs(el) ? false : blockAttrs({ type: "quote" })(el)) },
        { tag: "blockquote > p", getAttrs: blockAttrs({ type: "quote" }) },
        { tag: "pre", getAttrs: blockAttrs({ type: "code", language: "" }), preserveWhitespace: "full" },
        { tag: "li", getAttrs: (el: HTMLElement) => (hasParagraphs(el) ? false : listItemAttrs(el)) },
        { tag: "li > p", getAttrs: (el: HTMLElement) => listItemAttrs(el.parentElement!) },
        { tag: "hr", getAttrs: blockAttrs({ type: "divider" }) },
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

/**
 * For pasted HTML. Blocks keep their line breaks, but HTML from elsewhere wraps its source lines
 * wherever it likes, so outside a `<pre>` or Drusite's own blocks its whitespace collapses as a
 * browser would show it.
 */
const isTag = (rule: ParseRule): rule is TagParseRule => "tag" in rule && typeof rule.tag === "string";
export const clipboardParser = new DOMParser(
  schema,
  DOMParser.fromSchema(schema).rules.map((rule): ParseRule => {
    if (!isTag(rule) || rule.node !== "block" || rule.tag === "pre") return rule;
    return { ...rule, preserveWhitespace: rule.tag === "[data-kind]" ? ("full" as const) : false };
  }),
);

/** The mark keys the core knows, in the order the core reports them. */
export const MARK_KEYS = ["bold", "italic", "underline", "strikethrough", "code", "link"] as const;
export type MarkKey = (typeof MARK_KEYS)[number];

/** Marks that grow when you type at their end, as in the core. */
export const EXPANDS_AFTER: ReadonlySet<MarkKey> = new Set(["bold", "italic", "underline", "strikethrough"]);
