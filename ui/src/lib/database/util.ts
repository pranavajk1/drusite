// SPDX-License-Identifier: AGPL-3.0-or-later

// Shared pieces of the database views: property kinds' names and icons, empty values, and
// reading what a person typed into a cell.

import type {
  Cell,
  Color,
  Layout,
  NumberFormat,
  PropertyDef,
  PropertyKind,
  RollupFunction,
  RowView,
  Schema,
  SelectOption,
  Value,
} from "../../core/types";
import type { IconName } from "../icons/icons";

export type KindName = PropertyKind["type"];

/** The kinds a person can pick when adding a property, in Notion's order. */
export const KINDS: { type: KindName; label: string; icon: IconName }[] = [
  { type: "text", label: "Text", icon: "type" },
  { type: "number", label: "Number", icon: "hash" },
  { type: "select", label: "Select", icon: "circle-dot" },
  { type: "multiSelect", label: "Multi-select", icon: "tags" },
  { type: "status", label: "Status", icon: "circle-check-big" },
  { type: "date", label: "Date", icon: "calendar" },
  { type: "people", label: "Person", icon: "users" },
  { type: "files", label: "Files & media", icon: "paperclip" },
  { type: "checkbox", label: "Checkbox", icon: "square-check" },
  { type: "url", label: "URL", icon: "link" },
  { type: "email", label: "Email", icon: "at-sign" },
  { type: "phone", label: "Phone", icon: "phone" },
  { type: "formula", label: "Formula", icon: "sigma" },
  { type: "relation", label: "Relation", icon: "arrow-up-right" },
  { type: "rollup", label: "Rollup", icon: "chart-column" },
  { type: "createdTime", label: "Created time", icon: "clock" },
  { type: "createdBy", label: "Created by", icon: "user" },
  { type: "editedTime", label: "Last edited time", icon: "clock" },
  { type: "editedBy", label: "Last edited by", icon: "user" },
  { type: "uniqueId", label: "ID", icon: "fingerprint" },
];

export function kindIcon(kind: PropertyKind): IconName {
  if (kind.type === "title") return "type";
  return KINDS.find((k) => k.type === kind.type)?.icon ?? "type";
}

export function kindLabel(kind: PropertyKind): string {
  if (kind.type === "title") return "Title";
  return KINDS.find((k) => k.type === kind.type)?.label ?? kind.type;
}

/** A fresh kind of `type`, with the settings Notion starts it with. */
export function newKind(type: KindName, schema: Schema): PropertyKind {
  switch (type) {
    case "number":
      return { type, format: "number" };
    case "relation":
      return { type, database: schema.database, twin: null, mirror: false, single: false };
    case "rollup": {
      const rel = schema.properties.find((p) => p.kind.type === "relation");
      return { type, relation: rel?.id ?? "", target: "title", function: "count_all" };
    }
    case "formula":
      return { type, expression: "" };
    case "uniqueId":
      return { type, prefix: null };
    default:
      return { type } as PropertyKind;
  }
}

export const LAYOUTS: { layout: Layout; label: string; icon: IconName }[] = [
  { layout: "table", label: "Table", icon: "table" },
  { layout: "board", label: "Board", icon: "square-kanban" },
  { layout: "calendar", label: "Calendar", icon: "calendar" },
  { layout: "list", label: "List", icon: "list" },
  { layout: "gallery", label: "Gallery", icon: "layout-grid" },
];

export const COLORS: Color[] = ["default", "gray", "brown", "orange", "yellow", "green", "blue", "purple", "pink", "red"];

export const NUMBER_FORMATS: { value: NumberFormat; label: string }[] = [
  { value: "number", label: "Number" },
  { value: "number_with_commas", label: "Number with commas" },
  { value: "percent", label: "Percent" },
  { value: "dollar", label: "US dollar" },
  { value: "euro", label: "Euro" },
  { value: "pound", label: "Pound" },
  { value: "yen", label: "Yen" },
  { value: "rupee", label: "Rupee" },
  { value: "won", label: "Won" },
  { value: "yuan", label: "Yuan" },
  { value: "canadian_dollar", label: "Canadian dollar" },
  { value: "australian_dollar", label: "Australian dollar" },
  { value: "franc", label: "Swiss franc" },
  { value: "real", label: "Real" },
  { value: "krona", label: "Krona" },
];

export const ROLLUPS: { value: RollupFunction; label: string }[] = [
  { value: "show_original", label: "Show original" },
  { value: "show_unique", label: "Show unique values" },
  { value: "count_all", label: "Count all" },
  { value: "count_values", label: "Count values" },
  { value: "count_unique_values", label: "Count unique values" },
  { value: "count_empty", label: "Count empty" },
  { value: "count_not_empty", label: "Count not empty" },
  { value: "percent_empty", label: "Percent empty" },
  { value: "percent_not_empty", label: "Percent not empty" },
  { value: "sum", label: "Sum" },
  { value: "average", label: "Average" },
  { value: "median", label: "Median" },
  { value: "min", label: "Min" },
  { value: "max", label: "Max" },
  { value: "range", label: "Range" },
  { value: "earliest_date", label: "Earliest date" },
  { value: "latest_date", label: "Latest date" },
  { value: "date_range", label: "Date range" },
  { value: "checked", label: "Checked" },
  { value: "unchecked", label: "Unchecked" },
  { value: "percent_checked", label: "Percent checked" },
  { value: "percent_unchecked", label: "Percent unchecked" },
];

/** A short random ID, as the core makes them. */
export function shortId(): string {
  const a = "abcdefghijklmnopqrstuvwxyz0123456789";
  const bytes = crypto.getRandomValues(new Uint8Array(8));
  return Array.from(bytes, (b) => a[b % a.length]).join("");
}

export function cellOf(row: RowView, prop: string): Cell | undefined {
  return row.cells.find((c) => c.property === prop);
}

export function option(p: PropertyDef, id: string): SelectOption | undefined {
  return p.options.find((o) => o.id === id);
}

/** Whether a cell can be edited (rather than being computed). */
export function editable(p: PropertyDef): boolean {
  switch (p.kind.type) {
    case "formula":
    case "rollup":
    case "createdTime":
    case "createdBy":
    case "editedTime":
    case "editedBy":
    case "uniqueId":
      return false;
    default:
      return true;
  }
}

/** Reads a typed value for a simple property; null means "clear it". */
export function parseTyped(kind: PropertyKind, text: string): Value | null {
  const t = text.trim();
  if (t === "") return null;
  switch (kind.type) {
    case "text":
      return { type: "text", value: text };
    case "number": {
      const n = Number(t.replace(/[,$€£¥%\s]/g, ""));
      if (!Number.isFinite(n)) return null;
      return { type: "number", value: kind.format === "percent" && t.endsWith("%") ? n / 100 : n };
    }
    case "url":
      return { type: "url", value: t };
    case "email":
      return { type: "email", value: t };
    case "phone":
      return { type: "phone", value: t };
    default:
      return null;
  }
}

/** The text a simple cell edits as. */
export function editText(cell: Cell | undefined): string {
  const v = cell?.value;
  if (!v) return "";
  switch (v.type) {
    case "text":
    case "url":
    case "email":
    case "phone":
      return v.value;
    case "number":
      return String(v.value);
    default:
      return cell?.text ?? "";
  }
}

/** Today as YYYY-MM-DD, the person's own day. */
export function todayIso(): string {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

export const PHONE = () => window.matchMedia?.("(max-width: 700px)").matches ?? false;
