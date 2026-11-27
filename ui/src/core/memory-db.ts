// SPDX-License-Identifier: AGPL-3.0-or-later

// Databases in the memory core (the browser tests' stand-in for the Rust core). It keeps
// schemas, views and values, and answers views with groups, sorts and simple filters. Formulas
// and rollups aren't computed here: the Rust core's tests cover those.

import type {
  Cell,
  Color,
  Filter,
  PageId,
  PropertyDef,
  RowGroup,
  RowView,
  Schema,
  SelectOption,
  Template,
  Value,
  View,
  ViewResult,
} from "./types";

let seq = 0;
export function shortId(): string {
  seq += 1;
  return `m${seq.toString(36).padStart(7, "0")}`;
}

export interface DbHost {
  title(id: PageId): string;
  rows(db: PageId): PageId[];
}

interface MemDb {
  properties: PropertyDef[];
  views: View[];
  inline: boolean;
}

function option(name: string, color: Color, group: SelectOption["group"], position: number): SelectOption {
  return { id: shortId(), name, color, group, position };
}

function view(name: string, layout: View["layout"], position: number, properties: string[]): View {
  return {
    id: shortId(),
    name,
    layout,
    position,
    filter: null,
    sorts: [],
    group: null,
    properties: properties.map((id) => ({ id, width: null })),
    dateProperty: null,
    cover: { type: "none" },
    wrap: false,
  };
}

export class MemDatabases {
  private readonly dbs = new Map<PageId, MemDb>();
  private readonly values = new Map<PageId, Map<string, Value>>();

  constructor(private readonly host: DbHost) {}

  isDatabase(id: PageId): boolean {
    return this.dbs.has(id);
  }

  create(id: PageId, inline: boolean, template: Template): void {
    const title: PropertyDef = { id: "title", name: "Name", kind: { type: "title" }, options: [], position: 0, description: null };
    const db: MemDb = { properties: [title], views: [view("Table", "table", 0, ["title"])], inline };
    if (template === "tasks") {
      title.name = "Task";
      const status: PropertyDef = {
        id: shortId(),
        name: "Status",
        kind: { type: "status" },
        options: [
          option("Not started", "default", "toDo", 0),
          option("In progress", "blue", "inProgress", 1),
          option("Done", "green", "complete", 2),
        ],
        position: 1,
        description: null,
      };
      const due: PropertyDef = { id: shortId(), name: "Due", kind: { type: "date" }, options: [], position: 2, description: null };
      const priority: PropertyDef = {
        id: shortId(),
        name: "Priority",
        kind: { type: "select" },
        options: [option("High", "red", null, 0), option("Medium", "yellow", null, 1), option("Low", "gray", null, 2)],
        position: 3,
        description: null,
      };
      const assignee: PropertyDef = { id: shortId(), name: "Assignee", kind: { type: "people" }, options: [], position: 4, description: null };
      db.properties.push(status, due, priority, assignee);
      const board = view("Board", "board", 0, ["title", due.id, priority.id, assignee.id]);
      board.group = { property: status.id, hidden: [""], hideEmpty: false, dateUnit: null };
      const table = view("All tasks", "table", 1, ["title", status.id, due.id, priority.id, assignee.id]);
      const calendar = view("Calendar", "calendar", 2, ["title"]);
      calendar.dateProperty = due.id;
      db.views = [board, table, calendar];
    }
    this.dbs.set(id, db);
  }

  schema(id: PageId): Schema {
    const db = this.dbs.get(id);
    if (!db) throw { code: "invalid", message: "that page isn't a database" };
    return {
      database: id,
      title: this.host.title(id),
      properties: [...db.properties].sort((a, b) => (a.id === "title" ? -1 : b.id === "title" ? 1 : a.position - b.position)),
      views: [...db.views].sort((a, b) => a.position - b.position),
      inline: db.inline,
    };
  }

  putProperty(id: PageId, p: PropertyDef): Schema {
    const db = this.dbs.get(id)!;
    const i = db.properties.findIndex((x) => x.id === p.id);
    if (i >= 0) db.properties[i] = p;
    else {
      if (p.kind.type === "status" && p.options.length === 0)
        p.options = [option("Not started", "default", "toDo", 0), option("In progress", "blue", "inProgress", 1), option("Done", "green", "complete", 2)];
      db.properties.push(p);
      for (const v of db.views) v.properties.push({ id: p.id, width: null });
    }
    return this.schema(id);
  }

  deleteProperty(id: PageId, prop: string): Schema {
    const db = this.dbs.get(id)!;
    db.properties = db.properties.filter((p) => p.id !== prop);
    return this.schema(id);
  }

  putView(id: PageId, v: View): Schema {
    const db = this.dbs.get(id)!;
    const i = db.views.findIndex((x) => x.id === v.id);
    if (i >= 0) db.views[i] = v;
    else db.views.push(v);
    return this.schema(id);
  }

  deleteView(id: PageId, v: string): Schema {
    const db = this.dbs.get(id)!;
    if (db.views.length > 1) db.views = db.views.filter((x) => x.id !== v);
    return this.schema(id);
  }

  setCell(row: PageId, prop: string, value: Value | null): void {
    const m = this.values.get(row) ?? new Map<string, Value>();
    if (value === null) m.delete(prop);
    else m.set(prop, value);
    this.values.set(row, m);
  }

  rowCreated(db: PageId, row: PageId): void {
    const status = this.dbs.get(db)?.properties.find((p) => p.kind.type === "status");
    const first = status?.options.find((o) => o.group === "toDo") ?? status?.options[0];
    if (status && first && !this.values.get(row)?.has(status.id)) this.setCell(row, status.id, { type: "status", value: first.id });
  }

  private text(p: PropertyDef, v: Value | undefined, row: PageId): string {
    if (p.kind.type === "title") return this.host.title(row);
    if (!v) return "";
    const opt = (id: string) => p.options.find((o) => o.id === id)?.name ?? "";
    switch (v.type) {
      case "text":
      case "url":
      case "email":
      case "phone":
        return v.value;
      case "number":
        return v.value === null ? "" : String(v.value);
      case "select":
      case "status":
        return opt(v.value);
      case "multiSelect":
        return v.value.map(opt).join(", ");
      case "date":
        return v.value.end ? `${v.value.start} → ${v.value.end}` : v.value.start;
      case "checkbox":
        return v.value ? "Yes" : "";
      case "people":
        return v.value.map((x) => x.name).join(", ");
      case "files":
        return v.value.map((x) => x.name).join(", ");
      case "relation":
        return v.value.map((r) => this.host.title(r)).join(", ");
      case "uniqueId":
        return String(v.value);
    }
  }

  row(db: PageId, row: PageId): RowView {
    const m = this.values.get(row);
    const cells: Cell[] = this.schema(db).properties.map((p) => {
      const value = p.kind.type === "title" ? { type: "text" as const, value: this.host.title(row) } : (m?.get(p.id) ?? null);
      return {
        property: p.id,
        value,
        computed: null,
        pages: value?.type === "relation" ? value.value.map((id) => ({ id, title: this.host.title(id) })) : [],
        text: this.text(p, value ?? undefined, row),
        error: null,
      };
    });
    return { id: row, title: this.host.title(row), cells };
  }

  private passes(f: Filter, row: RowView): boolean {
    if (f.type === "and") return f.filters.every((x) => this.passes(x, row));
    if (f.type === "or") return f.filters.length === 0 || f.filters.some((x) => this.passes(x, row));
    const c = row.cells.find((x) => x.property === f.property);
    if (!c) return true;
    const v = c.value;
    const want = f.value;
    switch (f.op) {
      case "isEmpty":
        return c.text === "";
      case "isNotEmpty":
        return c.text !== "";
      case "contains":
        if (want?.type === "id" && v && "value" in v && Array.isArray(v.value)) return (v.value as string[]).includes(want.value);
        return want?.type === "text" ? c.text.toLowerCase().includes(want.value.toLowerCase()) : true;
      case "equals":
        if (want?.type === "id") return !!v && "value" in v && v.value === want.value;
        if (want?.type === "number") return v?.type === "number" && v.value === want.value;
        if (want?.type === "checkbox") return (v?.type === "checkbox" && v.value) === want.value;
        return want?.type === "text" ? c.text.toLowerCase() === want.value.toLowerCase() : true;
      case "notEquals":
        if (want?.type === "id") return !(v && "value" in v && v.value === want.value);
        return want?.type === "text" ? c.text.toLowerCase() !== want.value.toLowerCase() : true;
      case "greater":
        return v?.type === "number" && want?.type === "number" && (v.value ?? 0) > want.value;
      case "less":
        return v?.type === "number" && want?.type === "number" && (v.value ?? 0) < want.value;
      default:
        return true;
    }
  }

  query(db: PageId, viewId: string | null): ViewResult {
    const schema = this.schema(db);
    const v = schema.views.find((x) => x.id === viewId) ?? schema.views[0]!;
    const all = this.host.rows(db).map((r) => this.row(db, r));
    let rows = v.filter ? all.filter((r) => this.passes(v.filter!, r)) : all;
    if (v.sorts.length) {
      rows = [...rows].sort((a, b) => {
        for (const s of v.sorts) {
          const p = schema.properties.find((x) => x.id === s.property);
          const key = (r: RowView) => {
            const c = r.cells.find((x) => x.property === s.property);
            if (p && (c?.value?.type === "select" || c?.value?.type === "status")) return p.options.findIndex((o) => o.id === c.value!.value);
            if (c?.value?.type === "number") return c.value.value ?? 0;
            return c?.text.toLowerCase() ?? "";
          };
          const x = key(a);
          const y = key(b);
          const o = x < y ? -1 : x > y ? 1 : 0;
          if (o !== 0) return s.descending ? -o : o;
        }
        return 0;
      });
    }
    let groupBy = v.group?.property ?? null;
    if (!groupBy && v.layout === "board") groupBy = schema.properties.find((p) => p.kind.type === "status" || p.kind.type === "select")?.id ?? null;
    const prop = schema.properties.find((p) => p.id === groupBy);
    let groups: RowGroup[];
    if (!prop) {
      groups = [{ key: "", label: "", color: "default", value: null, rows, hidden: false }];
    } else {
      const hidden = v.group?.hidden ?? [];
      const valueFor = (id: string): Value =>
        prop.kind.type === "status" ? { type: "status", value: id } : prop.kind.type === "multiSelect" ? { type: "multiSelect", value: [id] } : { type: "select", value: id };
      groups = [
        { key: "", label: `No ${prop.name}`, color: "default", value: null, rows: [], hidden: hidden.includes("") },
        ...prop.options.map((o) => ({ key: o.id, label: o.name, color: o.color, value: valueFor(o.id), rows: [] as RowView[], hidden: hidden.includes(o.id) })),
      ];
      for (const r of rows) {
        const c = r.cells.find((x) => x.property === prop.id)?.value;
        const keys = c?.type === "multiSelect" ? c.value : c && (c.type === "select" || c.type === "status") ? [c.value] : [""];
        for (const k of keys.length ? keys : [""]) groups.find((g) => g.key === k)?.rows.push(r);
      }
      if (v.group?.hideEmpty) groups = groups.filter((g) => g.rows.length > 0);
    }
    return { schema, view: v, groups, shown: rows.length, total: all.length };
  }
}
