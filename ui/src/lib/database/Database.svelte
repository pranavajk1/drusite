<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  A database: its views as tabs, and the open view laid out as a table, board, calendar, list or
  gallery. Full-page under its page's title, or inline in another page (`inline`). Everything
  here goes through the core: a view is asked for again whenever the workspace changes, here or
  on another device.
-->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "../../core/client";
  import type { PageId, PageInfo, Person, PropertyDef, RowGroup, RowView, Schema, Value, View, ViewResult } from "../../core/types";
  import Icon from "../ui/Icon.svelte";
  import CellEditor from "./CellEditor.svelte";
  import CellValue from "./CellValue.svelte";
  import { type Drop, draggable } from "./drag";
  import PropertyForm from "./PropertyForm.svelte";
  import ViewSettings from "./ViewSettings.svelte";
  import { LAYOUTS, cellOf, editable, kindIcon, shortId, todayIso } from "./util";

  let {
    client,
    database,
    view: initialView = null,
    pages,
    inline = false,
    onopen,
    onerror,
    onview,
  }: {
    client: CoreClient;
    database: PageId;
    /** The view to show first; the first view when none. */
    view?: string | null;
    pages: PageInfo[];
    inline?: boolean;
    onopen: (page: PageId) => void;
    onerror: (e: unknown) => void;
    /** The view shown changed (an inline block keeps it). */
    onview?: (view: string) => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  let viewId = $state<string | null>(initialView ?? localStorage.getItem(`drusite.view.${database}`));
  let result = $state<ViewResult | null>(null);
  let loading = true;
  let again = false;

  async function load() {
    if (loading && result) {
      again = true;
      return;
    }
    loading = true;
    try {
      result = await client.queryView(database, viewId);
      if (viewId !== result.view.id) viewId = result.view.id;
    } catch (e) {
      // A view deleted elsewhere: fall back to the first.
      if (viewId) {
        viewId = null;
        loading = false;
        return load();
      }
      onerror(e);
    } finally {
      loading = false;
      if (again) {
        again = false;
        void load();
      }
    }
  }

  let timer: ReturnType<typeof setTimeout> | undefined;
  onMount(() => {
    void load();
    const stop = client.subscribe((e) => {
      if (e.type === "pagesChanged") {
        clearTimeout(timer);
        timer = setTimeout(load, 60);
      }
    });
    return () => {
      stop();
      clearTimeout(timer);
    };
  });

  function showView(id: string) {
    viewId = id;
    localStorage.setItem(`drusite.view.${database}`, id);
    onview?.(id);
    void load();
  }

  const schema = $derived(result?.schema ?? null);
  const view = $derived(result?.view ?? null);
  const shown = $derived.by((): PropertyDef[] => {
    if (!schema || !view) return [];
    const list = view.properties.map((vp) => schema.properties.find((p) => p.id === vp.id)).filter((p): p is PropertyDef => !!p);
    return list.length ? list : schema.properties.filter((p) => p.id === "title");
  });
  const extra = $derived(shown.filter((p) => p.id !== "title"));
  const groups = $derived((result?.groups ?? []).filter((g) => !g.hidden));
  const grouped = $derived(!!view && (view.layout === "board" || !!view.group));
  const allRows = $derived.by(() => {
    const seen = new Set<PageId>();
    const out: RowView[] = [];
    for (const g of result?.groups ?? []) for (const r of g.rows) if (!seen.has(r.id)) (seen.add(r.id), out.push(r));
    return out;
  });
  const people = $derived.by((): Person[] => {
    const m = new Map<string, Person>();
    for (const r of allRows) for (const c of r.cells) if (c.value?.type === "people") for (const p of c.value.value) m.set(p.id, p);
    return [...m.values()];
  });
  const databases = $derived(pages.filter((p) => p.kind === "database"));

  // ---- Editing ----
  let editing = $state<{ row: RowView; prop: PropertyDef; anchor: HTMLElement } | null>(null);
  let propForm = $state<{ prop: PropertyDef | null; anchor: HTMLElement } | null>(null);
  let settings = $state<{ anchor: HTMLElement; section: "view" | "filter" | "sort" | "properties" | "group" } | null>(null);

  function editCell(row: RowView, prop: PropertyDef, anchor: HTMLElement) {
    if (prop.kind.type === "checkbox") return;
    if (!editable(prop) && !cellOf(row, prop.id)?.error) return;
    editing = { row, prop, anchor };
  }

  async function setCell(row: PageId, prop: string, value: Value | null) {
    try {
      await client.setCell(row, prop, value);
      await load();
    } catch (e) {
      onerror(e);
    }
  }

  async function newRow(group: RowGroup | null = null, values: { property: string; value: Value }[] = [], open = true) {
    if (!schema) return;
    const vals = [...values];
    const by = view?.group?.property ?? (view?.layout === "board" ? boardProp?.id : undefined);
    if (group?.value && by) vals.push({ property: by, value: group.value });
    // A row made in a filtered view gets what the filter asks for, where that's one value.
    try {
      const page = await client.createRow(database, "", vals);
      await load();
      if (open) onopen(page.id);
    } catch (e) {
      onerror(e);
    }
  }

  async function addView(layout: View["layout"]) {
    if (!schema) return;
    const label = LAYOUTS.find((l) => l.layout === layout)!.label;
    const v: View = {
      id: shortId(),
      name: label,
      layout,
      position: Math.max(0, ...schema.views.map((x) => x.position)) + 1,
      filter: null,
      sorts: [],
      group: null,
      properties: (view?.properties ?? [{ id: "title", width: null }]).map((p) => ({ ...p })),
      dateProperty: schema.properties.find((p) => p.kind.type === "date")?.id ?? null,
      cover: { type: "none" },
      wrap: false,
    };
    try {
      await client.putView(database, v);
      showView(v.id);
    } catch (e) {
      onerror(e);
    }
  }
  let addingView = $state(false);

  // ---- Board ----
  const boardProp = $derived.by(() => {
    if (!schema || !view) return undefined;
    const id = view.group?.property;
    return id
      ? schema.properties.find((p) => p.id === id)
      : (schema.properties.find((p) => p.kind.type === "status") ?? schema.properties.find((p) => p.kind.type === "select"));
  });

  /** A card dropped on a group (a board's column): give it that group's value, and put it in place. */
  async function dropCard(id: string, drop: Drop) {
    if (!result || !boardProp) return;
    const row = allRows.find((r) => r.id === id);
    const to = result.groups.find((g) => g.key === drop.target);
    if (!row || !to) return;
    const from = result.groups.find((g) => g.rows.some((r) => r.id === id));
    try {
      if (from?.key !== to.key) {
        let value = to.value;
        // Multi-select: swap the tag it was under for the one it's dropped on.
        const cur = cellOf(row, boardProp.id)?.value;
        if (cur?.type === "multiSelect" && to.value?.type === "multiSelect") {
          const kept = cur.value.filter((x) => x !== from?.key);
          value = { type: "multiSelect", value: [...new Set([...kept, ...to.value.value])] };
        } else if (cur?.type === "multiSelect" && !to.value) {
          const kept = cur.value.filter((x) => x !== from?.key);
          value = kept.length ? { type: "multiSelect", value: kept } : null;
        }
        await client.setCell(row.id, boardProp.id, value);
      }
      // Its place among the database's rows: just before the card it was dropped on.
      if (!view?.sorts.length) {
        const order = pages.find((p) => p.id === database)?.children ?? [];
        const rest = order.filter((r) => r !== id);
        const index = drop.before ? rest.indexOf(drop.before as PageId) : (() => {
          const last = to.rows.filter((r) => r.id !== id).at(-1);
          return last ? rest.indexOf(last.id) + 1 : rest.length;
        })();
        if (index >= 0) await client.movePage(id as PageId, database, index);
      }
      await load();
    } catch (e) {
      onerror(e);
    }
  }

  // ---- Calendar ----
  let month = $state(todayIso().slice(0, 7));
  const dateProp = $derived(schema?.properties.find((p) => p.id === view?.dateProperty) ?? schema?.properties.find((p) => p.kind.type === "date"));
  const weeks = $derived.by(() => {
    const [y, m] = month.split("-").map(Number) as [number, number];
    const first = new Date(y, m - 1, 1);
    const startDay = (first.getDay() + 6) % 7;
    const start = new Date(y, m - 1, 1 - startDay);
    const out: string[][] = [];
    for (let w = 0; w < 6; w++) {
      const week: string[] = [];
      for (let d = 0; d < 7; d++) {
        const day = new Date(start.getFullYear(), start.getMonth(), start.getDate() + w * 7 + d);
        week.push(`${day.getFullYear()}-${String(day.getMonth() + 1).padStart(2, "0")}-${String(day.getDate()).padStart(2, "0")}`);
      }
      if (w < 4 || week.some((d) => d.startsWith(month))) out.push(week);
    }
    return out;
  });
  function dayOf(row: RowView): { start: string; end: string } | null {
    if (!dateProp) return null;
    const c = cellOf(row, dateProp.id);
    const v = c?.value?.type === "date" ? c.value.value : c?.computed?.type === "date" ? c.computed.value : null;
    if (!v) return null;
    return { start: v.start.slice(0, 10), end: (v.end ?? v.start).slice(0, 10) };
  }
  const byDay = $derived.by(() => {
    const m = new Map<string, RowView[]>();
    for (const r of allRows) {
      const d = dayOf(r);
      if (!d) continue;
      for (const week of weeks)
        for (const day of week)
          if (day >= d.start && day <= d.end) {
            const list = m.get(day) ?? [];
            list.push(r);
            m.set(day, list);
          }
    }
    return m;
  });
  function shiftMonth(by: number) {
    const [y, mm] = month.split("-").map(Number) as [number, number];
    const d = new Date(y, mm - 1 + by, 1);
    month = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}`;
  }
  async function dropOnDay(id: string, drop: Drop) {
    if (!dateProp || dateProp.kind.type !== "date") return;
    const row = allRows.find((r) => r.id === id);
    const c = row ? cellOf(row, dateProp.id) : undefined;
    const old = c?.value?.type === "date" ? c.value.value : null;
    const day = drop.target;
    let value: Value = { type: "date", value: { start: day, end: null } };
    if (old) {
      // Keep its time, and the length of a range.
      const time = old.start.length > 10 ? old.start.slice(10) : "";
      const len = old.end ? (Date.parse(old.end.slice(0, 10)) - Date.parse(old.start.slice(0, 10))) / 86_400_000 : null;
      const end = len !== null ? new Date(Date.parse(day) + len * 86_400_000).toISOString().slice(0, 10) + (old.end!.length > 10 ? old.end!.slice(10) : "") : null;
      value = { type: "date", value: { start: day + time, end } };
    }
    await setCell(id as PageId, dateProp.id, value);
  }
  const monthName = $derived(new Date(`${month}-01T00:00`).toLocaleDateString(undefined, { month: "long", year: "numeric" }));
  const today = todayIso();

  const filterCount = $derived.by(() => {
    const f = view?.filter;
    if (!f) return 0;
    return f.type === "condition" ? 1 : f.filters.length;
  });
</script>

{#snippet title(row: RowView)}
  <span class="row-title">{row.title || "Untitled"}</span>
{/snippet}

{#snippet props(row: RowView, compact: boolean)}
  {#each extra as p (p.id)}
    {@const c = cellOf(row, p.id)}
    {#if c && (c.text || c.value?.type === "checkbox" || c.error)}
      <div class="card-prop" class:compact>
        <CellValue prop={p} cell={c} {onopen} ontoggle={p.kind.type === "checkbox" ? (v) => setCell(row.id, p.id, { type: "checkbox", value: v }) : undefined} />
      </div>
    {/if}
  {/each}
{/snippet}

<section class="database" class:inline aria-label={schema?.title || "Database"}>
  {#if inline && schema}
    <button class="db-inline-title" onclick={() => onopen(database)}><Icon name="table-properties" size={16} />{schema.title || "Untitled"}</button>
  {/if}
  <div class="db-bar">
    <div class="db-tabs" role="tablist" aria-label="Views">
      {#each schema?.views ?? [] as v (v.id)}
        <button role="tab" class="db-tab" aria-selected={v.id === view?.id} onclick={() => showView(v.id)}>
          <Icon name={LAYOUTS.find((l) => l.layout === v.layout)?.icon ?? "table"} size={14} />{v.name}
        </button>
      {/each}
      <button class="icon-btn db-add-view" aria-label="Add a view" aria-expanded={addingView} onclick={() => (addingView = !addingView)}><Icon name="plus" size={14} /></button>
    </div>
    <div class="db-tools">
      <button
        class="icon-btn"
        class:active={filterCount > 0}
        aria-label={filterCount ? `Filter (${filterCount})` : "Filter"}
        title="Filter"
        onclick={(e) => (settings = { anchor: e.currentTarget, section: "filter" })}><Icon name="list-filter" size={16} /></button
      >
      <button
        class="icon-btn"
        class:active={!!view?.sorts.length}
        aria-label="Sort"
        title="Sort"
        onclick={(e) => (settings = { anchor: e.currentTarget, section: "sort" })}><Icon name="arrow-up-down" size={16} /></button
      >
      <button class="icon-btn" aria-label="View settings" title="View settings" onclick={(e) => (settings = { anchor: e.currentTarget, section: "view" })}
        ><Icon name="sliders-horizontal" size={16} /></button
      >
      <button class="btn small primary db-new" onclick={() => newRow()}><Icon name="plus" size={14} />New</button>
    </div>
  </div>
  {#if addingView}
    <div class="db-layouts" role="group" aria-label="New view">
      {#each LAYOUTS as l (l.layout)}
        <button class="btn small ghost" onclick={() => ((addingView = false), addView(l.layout))}><Icon name={l.icon} size={14} />{l.label}</button>
      {/each}
    </div>
  {/if}

  {#if !result}
    <p class="db-loading">Loading…</p>
  {:else if view?.layout === "board"}
    <div class="board" role="list" aria-label="Board">
      {#each groups as g (g.key)}
        <div class="board-col" role="listitem" aria-label={g.label || "No value"} data-drop={g.key}>
          <div class="board-col-head">
            <span class="chip tag-{g.color}">{g.label || "No value"}</span><span class="board-count">{g.rows.length}</span>
          </div>
          {#each g.rows as r (r.id)}
            <div
              class="card"
              data-card={r.id}
              use:draggable={{ id: r.id, ondrop: dropCard }}
              role="button"
              tabindex="0"
              onclick={() => onopen(r.id)}
              onkeydown={(e) => e.key === "Enter" && onopen(r.id)}
            >
              {@render title(r)}
              {@render props(r, false)}
            </div>
          {/each}
          <button class="board-new" onclick={() => newRow(g)}><Icon name="plus" size={14} />New</button>
        </div>
      {/each}
    </div>
  {:else if view?.layout === "calendar"}
    {#if !dateProp}
      <p class="empty"><Icon name="calendar" size={28} />A calendar needs a date property.</p>
    {:else}
      <div class="cal-head">
        <span class="cal-month">{monthName}</span>
        <button class="btn small ghost" onclick={() => (month = todayIso().slice(0, 7))}>Today</button>
        <button class="icon-btn" aria-label="Previous month" onclick={() => shiftMonth(-1)}><Icon name="chevron-left" /></button>
        <button class="icon-btn" aria-label="Next month" onclick={() => shiftMonth(1)}><Icon name="chevron-right" /></button>
      </div>
      <div class="cal-grid" role="grid" aria-label={monthName}>
        <div class="cal-row cal-weekdays" role="row">
          {#each ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] as d (d)}<span role="columnheader">{d}</span>{/each}
        </div>
        {#each weeks as week (week[0])}
          <div class="cal-row" role="row">
            {#each week as day (day)}
              <div class="cal-day" class:other={!day.startsWith(month)} class:today={day === today} role="gridcell" data-drop={day}>
                <div class="cal-day-head">
                  <span class="cal-day-num">{Number(day.slice(8))}</span>
                  <button class="icon-btn cal-add" aria-label="New on {day}" onclick={() => newRow(null, [{ property: dateProp!.id, value: { type: "date", value: { start: day, end: null } } }])}
                    ><Icon name="plus" size={12} /></button
                  >
                </div>
                {#each byDay.get(day) ?? [] as r (r.id)}
                  <div
                    class="cal-card"
                    data-card={r.id}
                    use:draggable={{ id: r.id, ondrop: dropOnDay }}
                    role="button"
                    tabindex="0"
                    onclick={() => onopen(r.id)}
                    onkeydown={(e) => e.key === "Enter" && onopen(r.id)}
                  >
                    {r.title || "Untitled"}
                  </div>
                {/each}
              </div>
            {/each}
          </div>
        {/each}
      </div>
    {/if}
  {:else if view?.layout === "list"}
    {#each groups as g (g.key)}
      {#if grouped}<div class="db-group-head"><span class="chip tag-{g.color}">{g.label || "No value"}</span><span class="board-count">{g.rows.length}</span></div>{/if}
      <ul class="db-list">
        {#each g.rows as r (r.id)}
          <li>
            <button class="db-list-row" onclick={() => onopen(r.id)}>
              <Icon name="file-text" size={16} />{@render title(r)}<span class="db-list-props">{@render props(r, true)}</span>
            </button>
          </li>
        {/each}
      </ul>
    {/each}
    <button class="db-new-row" onclick={() => newRow()}><Icon name="plus" size={14} />New</button>
  {:else if view?.layout === "gallery"}
    {#each groups as g (g.key)}
      {#if grouped}<div class="db-group-head"><span class="chip tag-{g.color}">{g.label || "No value"}</span></div>{/if}
      <div class="gallery">
        {#each g.rows as r (r.id)}
          <div class="card gallery-card" role="button" tabindex="0" onclick={() => onopen(r.id)} onkeydown={(e) => e.key === "Enter" && onopen(r.id)}>
            <div class="gallery-cover" aria-hidden="true"><Icon name="file-text" size={28} /></div>
            {@render title(r)}
            {@render props(r, false)}
          </div>
        {/each}
        <button class="gallery-new" onclick={() => newRow(g)}><Icon name="plus" size={16} />New</button>
      </div>
    {/each}
  {:else}
    <div class="table-scroll">
      <table class="db-table" class:wrap={view?.wrap}>
        <thead>
          <tr>
            {#each shown as p (p.id)}
              <th style:width={`${view?.properties.find((x) => x.id === p.id)?.width ?? (p.id === "title" ? 240 : 180)}px`}>
                <button class="th-btn" onclick={(e) => (propForm = { prop: p, anchor: e.currentTarget })}><Icon name={kindIcon(p.kind)} size={14} />{p.name}</button>
              </th>
            {/each}
            <th class="th-add"><button class="icon-btn" aria-label="Add a property" onclick={(e) => (propForm = { prop: null, anchor: e.currentTarget })}><Icon name="plus" size={14} /></button></th>
          </tr>
        </thead>
        {#each groups as g (g.key)}
          <tbody>
            {#if grouped}
              <tr class="group-row"><td colspan={shown.length + 1}><span class="chip tag-{g.color}">{g.label || "No value"}</span><span class="board-count">{g.rows.length}</span></td></tr>
            {/if}
            {#each g.rows as r (r.id)}
              <tr>
                {#each shown as p (p.id)}
                  <td>
                    {#if p.id === "title"}
                      <div class="title-cell">
                        <button class="cell-btn" onclick={(e) => editCell(r, p, e.currentTarget)}>{@render title(r)}</button>
                        <button class="btn small ghost open-row" onclick={() => onopen(r.id)}><Icon name="arrow-up-right" size={14} />Open</button>
                      </div>
                    {:else}
                      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
                      <div
                        class="cell-btn"
                        role={editable(p) && p.kind.type !== "checkbox" ? "button" : undefined}
                        tabindex={editable(p) && p.kind.type !== "checkbox" ? 0 : undefined}
                        aria-label={editable(p) && p.kind.type !== "checkbox" ? `${p.name}: ${cellOf(r, p.id)?.text || "Empty"}` : undefined}
                        onclick={(e) => editCell(r, p, e.currentTarget)}
                        onkeydown={(e) => e.key === "Enter" && editCell(r, p, e.currentTarget)}
                      >
                        <CellValue
                          prop={p}
                          cell={cellOf(r, p.id)}
                          {onopen}
                          wrap={view?.wrap}
                          ontoggle={p.kind.type === "checkbox" ? (v) => setCell(r.id, p.id, { type: "checkbox", value: v }) : undefined}
                        />
                      </div>
                    {/if}
                  </td>
                {/each}
                <td></td>
              </tr>
            {/each}
            <tr class="new-row">
              <td colspan={shown.length + 1}><button class="db-new-row" onclick={() => newRow(grouped ? g : null)}><Icon name="plus" size={14} />New</button></td>
            </tr>
          </tbody>
        {/each}
      </table>
    </div>
  {/if}
  {#if result && result.shown < result.total}
    <p class="db-foot">{result.shown} of {result.total} shown</p>
  {/if}
</section>

{#if editing && schema}
  <CellEditor
    {client}
    {schema}
    prop={editing.prop}
    row={editing.row}
    anchor={editing.anchor}
    {people}
    onclose={() => ((editing = null), void load())}
    {onerror}
    {onopen}
  />
{/if}
{#if propForm && schema}
  <PropertyForm
    {client}
    {schema}
    prop={propForm.prop}
    anchor={propForm.anchor}
    {databases}
    onclose={() => (propForm = null)}
    {onerror}
    onsaved={() => void load()}
  />
{/if}
{#if settings && schema && view}
  <ViewSettings {client} {schema} {view} anchor={settings.anchor} section={settings.section} onclose={() => (settings = null)} {onerror} onsaved={() => void load()} />
{/if}
