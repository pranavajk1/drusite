<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  One view's settings, as Notion's view menu has them: its name and layout, which properties it
  shows, its filter (conditions joined by "and"), its sorts, how it groups, and a calendar's
  date property. Each change is saved as it's made; every device sees it.
-->
<script lang="ts">
  import type { CoreClient } from "../../core/client";
  import type { Filter, FilterCondition, FilterOp, FilterValue, PropertyDef, Schema, View } from "../../core/types";
  import Icon from "../ui/Icon.svelte";
  import Popover from "../ui/Popover.svelte";
  import Select from "../ui/Select.svelte";
  import { LAYOUTS, kindIcon } from "./util";

  let {
    client,
    schema,
    view,
    anchor,
    section = "view",
    onclose,
    onerror,
    onsaved,
  }: {
    client: CoreClient;
    schema: Schema;
    view: View;
    anchor: HTMLElement;
    section?: "view" | "filter" | "sort" | "properties" | "group";
    onclose: () => void;
    onerror: (e: unknown) => void;
    onsaved: (schema: Schema) => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  let v = $state<View>(structuredClone($state.snapshot(view)) as View);
  // svelte-ignore state_referenced_locally
  let tab = $state(section);
  let confirmDelete = $state(false);

  async function save() {
    try {
      onsaved(await client.putView(schema.database, $state.snapshot(v) as View));
    } catch (e) {
      onerror(e);
    }
  }

  const prop = (id: string) => schema.properties.find((p) => p.id === id);

  // ---- Properties ----
  const shownIds = $derived(v.properties.map((p) => p.id));
  function toggleShown(p: PropertyDef) {
    if (shownIds.includes(p.id)) v.properties = v.properties.filter((x) => x.id !== p.id);
    else v.properties = [...v.properties, { id: p.id, width: null }];
    void save();
  }
  function move(i: number, by: number) {
    const j = i + by;
    if (j < 0 || j >= v.properties.length) return;
    const list = [...v.properties];
    [list[i], list[j]] = [list[j]!, list[i]!];
    v.properties = list;
    void save();
  }

  // ---- Filter: conditions joined by "and" ----
  const conditions = $derived.by((): FilterCondition[] | null => {
    const f = v.filter;
    if (!f) return [];
    if (f.type === "condition") return [f];
    if (f.type === "and" && f.filters.every((x) => x.type === "condition")) return f.filters.map((x) => x as FilterCondition & { type: "condition" });
    return null;
  });

  function setConditions(list: FilterCondition[]) {
    v.filter = list.length === 0 ? null : { type: "and", filters: list.map((c): Filter => ({ type: "condition", ...c })) };
    void save();
  }

  function opsFor(p: PropertyDef | undefined): { value: FilterOp; label: string }[] {
    const t = p?.kind.type;
    if (t === "number" || t === "uniqueId")
      return [
        { value: "equals", label: "=" },
        { value: "notEquals", label: "≠" },
        { value: "greater", label: ">" },
        { value: "less", label: "<" },
        { value: "greaterOrEqual", label: "≥" },
        { value: "lessOrEqual", label: "≤" },
        { value: "isEmpty", label: "is empty" },
        { value: "isNotEmpty", label: "is not empty" },
      ];
    if (t === "checkbox") return [{ value: "equals", label: "is" }];
    if (t === "date" || t === "createdTime" || t === "editedTime")
      return [
        { value: "equals", label: "is" },
        { value: "before", label: "is before" },
        { value: "after", label: "is after" },
        { value: "onOrBefore", label: "is on or before" },
        { value: "onOrAfter", label: "is on or after" },
        { value: "pastWeek", label: "is within the past week" },
        { value: "nextWeek", label: "is within the next week" },
        { value: "thisWeek", label: "is this week" },
        { value: "isEmpty", label: "is empty" },
        { value: "isNotEmpty", label: "is not empty" },
      ];
    if (t === "select" || t === "status")
      return [
        { value: "equals", label: "is" },
        { value: "notEquals", label: "is not" },
        { value: "isEmpty", label: "is empty" },
        { value: "isNotEmpty", label: "is not empty" },
      ];
    if (t === "multiSelect" || t === "people" || t === "relation")
      return [
        { value: "contains", label: "contains" },
        { value: "notContains", label: "does not contain" },
        { value: "isEmpty", label: "is empty" },
        { value: "isNotEmpty", label: "is not empty" },
      ];
    return [
      { value: "contains", label: "contains" },
      { value: "notContains", label: "does not contain" },
      { value: "equals", label: "is" },
      { value: "notEquals", label: "is not" },
      { value: "startsWith", label: "starts with" },
      { value: "endsWith", label: "ends with" },
      { value: "isEmpty", label: "is empty" },
      { value: "isNotEmpty", label: "is not empty" },
    ];
  }

  const NO_VALUE: FilterOp[] = ["isEmpty", "isNotEmpty", "pastWeek", "pastMonth", "pastYear", "nextWeek", "nextMonth", "nextYear", "thisWeek"];

  function defaultValue(p: PropertyDef | undefined): FilterValue | null {
    const t = p?.kind.type;
    if (t === "checkbox") return { type: "checkbox", value: true };
    if (t === "number") return { type: "number", value: 0 };
    if (t === "date" || t === "createdTime" || t === "editedTime") return { type: "relative", value: "today" };
    if ((t === "select" || t === "status" || t === "multiSelect") && p!.options[0]) return { type: "id", value: p!.options[0].id };
    return { type: "text", value: "" };
  }

  function addCondition() {
    const p = schema.properties.find((x) => x.kind.type !== "title") ?? schema.properties[0]!;
    const op = opsFor(p)[0]!.value;
    setConditions([...(conditions ?? []), { property: p.id, op, value: defaultValue(p) }]);
  }

  function update(i: number, c: FilterCondition) {
    const list = [...(conditions ?? [])];
    list[i] = c;
    setConditions(list);
  }

  // ---- Sorts ----
  function addSort() {
    const p = schema.properties.find((x) => !v.sorts.some((s) => s.property === x.id));
    if (p) {
      v.sorts = [...v.sorts, { property: p.id, descending: false }];
      void save();
    }
  }

  // ---- Group ----
  const groupable = $derived(
    schema.properties.filter((p) => ["select", "status", "multiSelect", "checkbox", "people", "relation", "date", "text"].includes(p.kind.type)),
  );
  const dates = $derived(schema.properties.filter((p) => ["date", "createdTime", "editedTime"].includes(p.kind.type)));

  async function deleteView() {
    try {
      onsaved(await client.deleteView(schema.database, v.id));
      onclose();
    } catch (e) {
      onerror(e);
    }
  }

  const TABS: { id: typeof tab; label: string; icon: "sliders-horizontal" | "list-filter" | "arrow-up-down" | "eye" | "rows-3" }[] = [
    { id: "view", label: "Layout", icon: "sliders-horizontal" },
    { id: "properties", label: "Properties", icon: "eye" },
    { id: "filter", label: "Filter", icon: "list-filter" },
    { id: "sort", label: "Sort", icon: "arrow-up-down" },
    { id: "group", label: "Group", icon: "rows-3" },
  ];
</script>

<Popover {anchor} label="View settings" {onclose} width={360}>
  <div class="view-tabs" role="tablist" aria-label="View settings">
    {#each TABS as t (t.id)}
      <button role="tab" class="view-tab" aria-selected={tab === t.id} onclick={() => (tab = t.id)}><Icon name={t.icon} size={14} />{t.label}</button>
    {/each}
  </div>

  {#if tab === "view"}
    <label class="prop-form-row">
      <span>Name</span>
      <input class="input" aria-label="View name" bind:value={v.name} onchange={save} />
    </label>
    <div class="layout-grid" role="radiogroup" aria-label="Layout">
      {#each LAYOUTS as l (l.layout)}
        <button
          role="radio"
          aria-checked={v.layout === l.layout}
          class="layout-choice"
          onclick={() => {
            v.layout = l.layout;
            if (l.layout === "calendar" && !v.dateProperty) v.dateProperty = dates[0]?.id ?? null;
            void save();
          }}><Icon name={l.icon} size={18} />{l.label}</button
        >
      {/each}
    </div>
    {#if v.layout === "calendar"}
      <label class="prop-form-row">
        <span>Show by</span>
        <Select
          label="Calendar by"
          value={v.dateProperty ?? ""}
          options={dates.map((d) => ({ value: d.id, label: d.name }))}
          onchange={(d) => ((v.dateProperty = d), void save())}
        />
      </label>
    {/if}
    {#if v.layout === "table"}
      <label class="date-toggle"><input type="checkbox" class="checkbox" bind:checked={v.wrap} onchange={save} /> Wrap cells</label>
    {/if}
    <div class="form-actions">
      {#if confirmDelete}
        <button class="btn small danger solid" onclick={deleteView}>Delete view</button>
      {:else}
        <button class="btn small ghost danger" disabled={schema.views.length <= 1} onclick={() => (confirmDelete = true)}>Delete view</button>
      {/if}
    </div>
  {:else if tab === "properties"}
    <ul class="prop-list">
      {#each v.properties as vp, i (vp.id)}
        {@const p = prop(vp.id)}
        {#if p}
          <li class="prop-list-row">
            <Icon name={kindIcon(p.kind)} size={14} /><span class="prop-list-name">{p.name}</span>
            <button class="icon-btn" aria-label="Move {p.name} up" onclick={() => move(i, -1)}><Icon name="chevron-left" size={14} /></button>
            <button class="icon-btn" aria-label="Move {p.name} down" onclick={() => move(i, 1)}><Icon name="chevron-right" size={14} /></button>
            {#if p.id !== "title"}<button class="icon-btn" aria-label="Hide {p.name}" onclick={() => toggleShown(p)}><Icon name="eye" size={14} /></button>{/if}
          </li>
        {/if}
      {/each}
      {#each schema.properties.filter((p) => !shownIds.includes(p.id)) as p (p.id)}
        <li class="prop-list-row hidden-prop">
          <Icon name={kindIcon(p.kind)} size={14} /><span class="prop-list-name">{p.name}</span>
          <button class="icon-btn" aria-label="Show {p.name}" onclick={() => toggleShown(p)}><Icon name="eye-off" size={14} /></button>
        </li>
      {/each}
    </ul>
  {:else if tab === "filter"}
    {#if conditions === null}
      <p class="prop-form-note">This view's filter has nested groups (from Notion). It still applies.</p>
      <button class="btn small ghost" onclick={() => setConditions([])}>Clear the filter</button>
    {:else}
      {#each conditions as c, i (i)}
        {@const p = prop(c.property)}
        <div class="filter-row">
          <Select
            label="Property"
            value={c.property}
            options={schema.properties.map((x) => ({ value: x.id, label: x.name }))}
            onchange={(id) => update(i, { property: id, op: opsFor(prop(id))[0]!.value, value: defaultValue(prop(id)) })}
          />
          <Select label="Condition" value={c.op} options={opsFor(p)} onchange={(op) => update(i, { ...c, op })} />
          {#if !NO_VALUE.includes(c.op)}
            {#if p && (p.kind.type === "select" || p.kind.type === "status" || p.kind.type === "multiSelect")}
              <Select
                label="Option"
                value={c.value?.type === "id" ? c.value.value : ""}
                options={p.options.map((o) => ({ value: o.id, label: o.name }))}
                onchange={(id) => update(i, { ...c, value: { type: "id", value: id } })}
              />
            {:else if p?.kind.type === "checkbox"}
              <Select
                label="Value"
                value={c.value?.type === "checkbox" && c.value.value ? "yes" : "no"}
                options={[
                  { value: "yes", label: "Checked" },
                  { value: "no", label: "Unchecked" },
                ]}
                onchange={(x) => update(i, { ...c, value: { type: "checkbox", value: x === "yes" } })}
              />
            {:else if p && (p.kind.type === "date" || p.kind.type === "createdTime" || p.kind.type === "editedTime")}
              <Select
                label="Day"
                value={c.value?.type === "relative" ? c.value.value : "exact"}
                options={[
                  { value: "today", label: "Today" },
                  { value: "tomorrow", label: "Tomorrow" },
                  { value: "yesterday", label: "Yesterday" },
                  { value: "oneWeekAgo", label: "One week ago" },
                  { value: "oneWeekFromNow", label: "One week from now" },
                  { value: "exact", label: "Exact date" },
                ]}
                onchange={(x) =>
                  update(i, { ...c, value: x === "exact" ? { type: "date", value: new Date().toISOString().slice(0, 10) } : { type: "relative", value: x as never } })}
              />
              {#if c.value?.type === "date"}
                <input class="input" type="date" aria-label="Date" value={c.value.value} onchange={(e) => update(i, { ...c, value: { type: "date", value: (e.currentTarget as HTMLInputElement).value } })} />
              {/if}
            {:else}
              <input
                class="input"
                aria-label="Value"
                inputmode={p?.kind.type === "number" ? "decimal" : undefined}
                value={c.value && "value" in c.value ? String(c.value.value) : ""}
                onchange={(e) => {
                  const raw = (e.currentTarget as HTMLInputElement).value;
                  update(i, { ...c, value: p?.kind.type === "number" ? { type: "number", value: Number(raw) || 0 } : { type: "text", value: raw } });
                }}
              />
            {/if}
          {/if}
          <button class="icon-btn" aria-label="Remove condition" onclick={() => setConditions((conditions ?? []).filter((_, j) => j !== i))}><Icon name="x" size={14} /></button>
        </div>
      {/each}
      <button class="btn small ghost" onclick={addCondition}><Icon name="plus" size={14} /> Add a filter</button>
    {/if}
  {:else if tab === "sort"}
    {#each v.sorts as s, i (i)}
      <div class="filter-row">
        <Select
          label="Sort by"
          value={s.property}
          options={schema.properties.map((x) => ({ value: x.id, label: x.name }))}
          onchange={(id) => ((v.sorts[i]!.property = id), void save())}
        />
        <Select
          label="Direction"
          value={s.descending ? "desc" : "asc"}
          options={[
            { value: "asc", label: "Ascending" },
            { value: "desc", label: "Descending" },
          ]}
          onchange={(d) => ((v.sorts[i]!.descending = d === "desc"), void save())}
        />
        <button class="icon-btn" aria-label="Remove sort" onclick={() => ((v.sorts = v.sorts.filter((_, j) => j !== i)), void save())}><Icon name="x" size={14} /></button>
      </div>
    {/each}
    <button class="btn small ghost" onclick={addSort}><Icon name="plus" size={14} /> Add a sort</button>
  {:else if tab === "group"}
    <label class="prop-form-row">
      <span>Group by</span>
      <Select
        label="Group by"
        value={v.group?.property ?? ""}
        options={[{ value: "", label: "None" }, ...groupable.map((p) => ({ value: p.id, label: p.name }))]}
        onchange={(id) => {
          v.group = id ? { property: id, hidden: [], hideEmpty: false, dateUnit: null } : null;
          void save();
        }}
      />
    </label>
    {#if v.group}
      <label class="date-toggle"
        ><input type="checkbox" class="checkbox" checked={v.group.hideEmpty} onchange={(e) => ((v.group!.hideEmpty = (e.currentTarget as HTMLInputElement).checked), void save())} /> Hide
        empty groups</label
      >
      {#if prop(v.group.property)?.kind.type === "date"}
        <label class="prop-form-row">
          <span>By</span>
          <Select
            label="Date grouping"
            value={v.group.dateUnit ?? "month"}
            options={[
              { value: "day", label: "Day" },
              { value: "week", label: "Week" },
              { value: "month", label: "Month" },
              { value: "year", label: "Year" },
            ]}
            onchange={(u) => ((v.group!.dateUnit = u), void save())}
          />
        </label>
      {/if}
    {/if}
  {/if}
</Popover>
