// SPDX-License-Identifier: AGPL-3.0-or-later

// Page queries: the filter block's JSON, and (for the in-memory core) how a query runs. The real
// core runs queries in Rust with the same rules.

import type { Condition, PageInfo, PageQuery, PropertyValue } from "../core/types";
import { DATE } from "./mood";

/** What a new filter block lists: the latest journal entries. */
export const DEFAULT_QUERY: PageQuery = {
  kind: "journal",
  conditions: [],
  sort: { by: DATE, descending: true },
  limit: 10,
};

export function parseQuery(json: string): PageQuery {
  try {
    const q = JSON.parse(json) as Partial<PageQuery>;
    return {
      kind: q.kind ?? null,
      conditions: Array.isArray(q.conditions) ? q.conditions : [],
      sort: q.sort ?? null,
      limit: typeof q.limit === "number" ? q.limit : null,
    };
  } catch {
    return { ...DEFAULT_QUERY };
  }
}

const rank = (v: PropertyValue) => ({ checkbox: 0, number: 1, date: 2, text: 3 })[v.type];

export function compareValues(a: PropertyValue, b: PropertyValue): number {
  if (a.type !== b.type) return rank(a) - rank(b);
  switch (a.type) {
    case "text":
      return a.value.toLowerCase().localeCompare((b.value as string).toLowerCase());
    case "number":
      return (a.value ?? 0) - ((b.value as number | null) ?? 0);
    case "checkbox":
      return Number(a.value) - Number(b.value);
    case "date":
      return a.value < (b.value as string) ? -1 : a.value > (b.value as string) ? 1 : 0;
  }
}

export function conditionMatches(c: Condition, value: PropertyValue | undefined): boolean {
  if (c.comparison === "exists") return value !== undefined;
  if (c.comparison === "missing") return value === undefined;
  const want = c.value ?? null;
  if (value === undefined || want === null) return false;
  if (c.comparison === "contains") {
    return value.type === "text" && want.type === "text" && value.value.toLowerCase().includes(want.value.toLowerCase());
  }
  if (value.type !== want.type) return c.comparison === "notEquals";
  const o = compareValues(value, want);
  switch (c.comparison) {
    case "equals":
      return o === 0;
    case "notEquals":
      return o !== 0;
    case "less":
      return o < 0;
    case "lessOrEqual":
      return o <= 0;
    case "greater":
      return o > 0;
    case "greaterOrEqual":
      return o >= 0;
  }
}

const prop = (p: PageInfo, name: string) => p.properties.find((x) => x.name === name)?.value;

export function runQuery(pages: PageInfo[], query: PageQuery): PageInfo[] {
  let out = pages.filter(
    (p) => (query.kind == null || p.kind === query.kind) && query.conditions.every((c) => conditionMatches(c, prop(p, c.property))),
  );
  const sort = query.sort;
  if (sort) {
    out = [...out].sort((a, b) => {
      if (sort.by === "title") {
        const o = a.title.toLowerCase().localeCompare(b.title.toLowerCase());
        return (sort.descending ? -o : o) || (a.id < b.id ? -1 : 1);
      }
      const x = prop(a, sort.by);
      const y = prop(b, sort.by);
      if (x && !y) return -1;
      if (!x && y) return 1;
      const o = x && y ? compareValues(x, y) : 0;
      return (sort.descending ? -o : o) || (a.id < b.id ? -1 : 1);
    });
  }
  return query.limit == null ? out : out.slice(0, query.limit);
}
