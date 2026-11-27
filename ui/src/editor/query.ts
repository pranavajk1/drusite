// SPDX-License-Identifier: AGPL-3.0-or-later

// The filter block: a block whose kind holds a page query, shown as the live list of pages it
// matches, with a small form to change the query. Its text (only possible through a concurrent
// edit) is kept but not shown, as a divider's is.

import type { Node } from "prosemirror-model";
import type { EditorView, NodeView } from "prosemirror-view";

import type { CoreClient } from "../core/client";
import type { Comparison, Condition, PageId, PageInfo, PageQuery, PropertyValue } from "../core/types";
import { moodOf } from "../journal/mood";
import { DEFAULT_QUERY, parseQuery } from "../journal/query";
import { attrsOf } from "./convert";

/** What a filter block needs from the app: the core, and a way to open a page. */
export interface QueryHost {
  client: CoreClient;
  open: (id: PageId) => void;
  pages: () => PageInfo[];
}

let host: QueryHost | null = null;

export function setQueryHost(h: QueryHost | null): void {
  host = h;
}

export function defaultQueryJson(): string {
  return JSON.stringify(DEFAULT_QUERY);
}

const COMPARISONS: [Comparison, string][] = [
  ["equals", "is"],
  ["notEquals", "is not"],
  ["contains", "contains"],
  ["greater", ">"],
  ["greaterOrEqual", "≥"],
  ["less", "<"],
  ["lessOrEqual", "≤"],
  ["exists", "is set"],
  ["missing", "is not set"],
];

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Record<string, string | number | boolean | null> = {},
  ...kids: (string | globalThis.Node)[]
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  Object.assign(e, props);
  e.append(...kids);
  return e;
}

function describe(q: PageQuery): string {
  const parts: string[] = [q.kind === "journal" ? "Journal entries" : q.kind === "page" ? "Pages" : "All pages"];
  for (const c of q.conditions) {
    const cmp = COMPARISONS.find(([k]) => k === c.comparison)?.[1] ?? c.comparison;
    parts.push(`${c.property} ${cmp}${c.value ? ` ${showValue(c.value)}` : ""}`);
  }
  if (q.sort) parts.push(`by ${q.sort.by}${q.sort.descending ? " (newest first)" : ""}`);
  if (q.limit != null) parts.push(`first ${q.limit}`);
  return parts.join(" · ");
}

function showValue(v: PropertyValue): string {
  return v.type === "checkbox" ? (v.value ? "yes" : "no") : String(v.value ?? "");
}

/** Reads a typed value: a number, a YYYY-MM-DD day, yes/no, or text. */
function readValue(raw: string): PropertyValue {
  const t = raw.trim();
  if (/^\d{4}-\d{2}-\d{2}$/.test(t)) return { type: "date", value: t };
  if (t !== "" && Number.isFinite(Number(t))) return { type: "number", value: Number(t) };
  if (/^(yes|true)$/i.test(t)) return { type: "checkbox", value: true };
  if (/^(no|false)$/i.test(t)) return { type: "checkbox", value: false };
  return { type: "text", value: raw };
}

export function queryView(node: Node, view: EditorView, getPos: () => number | undefined, setAttrs: (n: Node) => void): NodeView {
  const dom = document.createElement("div");
  setAttrs(node);
  const contentDOM = el("div", { className: "block-text query-text" });
  const summary = el("span", { className: "query-summary" });
  const edit = el("button", { className: "query-edit", type: "button", textContent: "Edit filter" });
  const form = el("div", { className: "query-form", hidden: true });
  const list = el("ul", { className: "query-results" });
  const widget = el("div", { className: "query-widget", contentEditable: "false" }, el("div", { className: "query-head" }, summary, edit), form, list);
  dom.append(widget, contentDOM);

  let query = parseQuery(attrsOf(node).kind.type === "query" ? (attrsOf(node).kind as { query: string }).query : "");
  let seq = 0;

  const save = (q: PageQuery) => {
    const pos = getPos();
    if (pos === undefined) return;
    view.dispatch(view.state.tr.setNodeAttribute(pos, "kind", { type: "query", query: JSON.stringify(q) }));
  };

  async function refresh() {
    summary.textContent = describe(query);
    if (!host) return;
    const mine = ++seq;
    let pages: PageInfo[];
    try {
      pages = await host.client.queryPages(query);
    } catch {
      return;
    }
    if (mine !== seq) return;
    list.replaceChildren(
      ...(pages.length === 0
        ? [el("li", { className: "query-empty", textContent: "No pages match." })]
        : pages.map((p) => {
            const mood = moodOf(p);
            const b = el("button", { type: "button", textContent: p.title || "Untitled" });
            if (mood) b.append(el("span", { className: "query-mood", title: mood.label, textContent: ` ${mood.emoji}` }));
            b.addEventListener("click", () => host?.open(p.id));
            return el("li", {}, b);
          })),
    );
  }

  function renderForm() {
    const kind = el("select", { ariaLabel: "Which pages" });
    for (const [v, label] of [["", "All pages"], ["journal", "Journal entries"], ["page", "Other pages"]]) {
      kind.append(el("option", { value: v!, textContent: label!, selected: (query.kind ?? "") === v }));
    }
    kind.addEventListener("change", () => save({ ...query, kind: kind.value === "" ? null : (kind.value as PageQuery["kind"]) }));

    const rows = query.conditions.map((c, i) => {
      const prop = el("input", { value: c.property, placeholder: "Property", ariaLabel: "Property" });
      const cmp = el("select", { ariaLabel: "Comparison" });
      for (const [k, label] of COMPARISONS) cmp.append(el("option", { value: k, textContent: label, selected: k === c.comparison }));
      const val = el("input", { value: c.value ? showValue(c.value) : "", placeholder: "Value", ariaLabel: "Value" });
      const remove = el("button", { type: "button", textContent: "×", ariaLabel: "Remove condition" });
      const update = () => {
        const next: Condition = {
          property: prop.value.trim(),
          comparison: cmp.value as Comparison,
          value: cmp.value === "exists" || cmp.value === "missing" ? null : readValue(val.value),
        };
        save({ ...query, conditions: query.conditions.map((x, j) => (j === i ? next : x)) });
      };
      prop.addEventListener("change", update);
      cmp.addEventListener("change", update);
      val.addEventListener("change", update);
      remove.addEventListener("click", () => save({ ...query, conditions: query.conditions.filter((_, j) => j !== i) }));
      return el("div", { className: "query-row" }, prop, cmp, val, remove);
    });
    const add = el("button", { type: "button", textContent: "+ Condition" });
    add.addEventListener("click", () => save({ ...query, conditions: [...query.conditions, { property: "Mood", comparison: "greaterOrEqual", value: { type: "number", value: 4 } }] }));

    const sortBy = el("input", { value: query.sort?.by ?? "", placeholder: "Sort by (a property, or title)", ariaLabel: "Sort by" });
    const desc = el("input", { type: "checkbox", checked: query.sort?.descending ?? false, ariaLabel: "Descending" });
    const limit = el("input", { type: "number", min: "1", value: query.limit == null ? "" : String(query.limit), placeholder: "No limit", ariaLabel: "Limit" });
    const sortChange = () =>
      save({ ...query, sort: sortBy.value.trim() === "" ? null : { by: sortBy.value.trim(), descending: desc.checked } });
    sortBy.addEventListener("change", sortChange);
    desc.addEventListener("change", sortChange);
    limit.addEventListener("change", () => {
      const n = Number(limit.value);
      save({ ...query, limit: limit.value === "" || !Number.isFinite(n) || n < 1 ? null : Math.floor(n) });
    });

    form.replaceChildren(
      el("div", { className: "query-row" }, kind),
      ...rows,
      el("div", { className: "query-row" }, add),
      el("div", { className: "query-row" }, sortBy, el("label", {}, desc, " Descending"), limit),
    );
  }

  edit.addEventListener("click", () => {
    form.hidden = !form.hidden;
    edit.textContent = form.hidden ? "Edit filter" : "Done";
    if (!form.hidden) renderForm();
  });

  const stop = host?.client.subscribe((e) => {
    if (e.type === "pagesChanged") void refresh();
  });
  void refresh();

  return {
    dom,
    contentDOM,
    update(next) {
      const k = attrsOf(next).kind;
      if (k.type !== "query") return false;
      node = next;
      setAttrs(next);
      const q = parseQuery(k.query);
      if (JSON.stringify(q) !== JSON.stringify(query)) {
        query = q;
        if (!form.hidden) renderForm();
        void refresh();
      }
      return true;
    },
    stopEvent: (e) => widget.contains(e.target as globalThis.Node),
    ignoreMutation: (m) => widget.contains(m.target as globalThis.Node),
    destroy: () => stop?.(),
  };
}
