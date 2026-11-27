<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- A database row's properties, under its title, as Notion shows them on a row's page. -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "../../core/client";
  import type { PageId, PageInfo, PropertyDef, RowView, Schema } from "../../core/types";
  import Icon from "../ui/Icon.svelte";
  import CellEditor from "./CellEditor.svelte";
  import CellValue from "./CellValue.svelte";
  import PropertyForm from "./PropertyForm.svelte";
  import { cellOf, editable, kindIcon } from "./util";

  let {
    client,
    page,
    pages,
    onopen,
    onerror,
  }: {
    client: CoreClient;
    page: PageId;
    pages: PageInfo[];
    onopen: (page: PageId) => void;
    onerror: (e: unknown) => void;
  } = $props();

  let schema = $state<Schema | null>(null);
  let row = $state<RowView | null>(null);
  let editing = $state<{ prop: PropertyDef; anchor: HTMLElement } | null>(null);
  let propForm = $state<{ prop: PropertyDef | null; anchor: HTMLElement } | null>(null);

  async function load() {
    try {
      const r = await client.rowCells(page);
      schema = r.schema;
      row = r.row;
    } catch (e) {
      onerror(e);
    }
  }

  onMount(() => {
    void load();
    let timer: ReturnType<typeof setTimeout> | undefined;
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

  const shownProps = $derived(schema?.properties.filter((p) => p.id !== "title") ?? []);
  const databases = $derived(pages.filter((p) => p.kind === "database"));
</script>

{#if schema && row}
  {@const r = row}
  <div class="props row-props" role="list" aria-label="Properties">
    {#each shownProps as p (p.id)}
      <div class="prop-row" role="listitem">
        <button class="prop-name" onclick={(e) => (propForm = { prop: p, anchor: e.currentTarget })}><Icon name={kindIcon(p.kind)} size={16} /><span>{p.name}</span></button>
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <div
          class="prop-value cell-btn"
          role={editable(p) && p.kind.type !== "checkbox" ? "button" : undefined}
          tabindex={editable(p) && p.kind.type !== "checkbox" ? 0 : undefined}
          aria-label={editable(p) && p.kind.type !== "checkbox" ? `${p.name}: ${cellOf(r, p.id)?.text || "Empty"}` : undefined}
          onclick={(e) => {
            if (p.kind.type !== "checkbox" && editable(p)) editing = { prop: p, anchor: e.currentTarget };
          }}
          onkeydown={(e) => {
            if (e.key === "Enter" && p.kind.type !== "checkbox" && editable(p)) editing = { prop: p, anchor: e.currentTarget };
          }}
        >
          {#if cellOf(r, p.id)?.text || cellOf(r, p.id)?.value || cellOf(r, p.id)?.error || p.kind.type === "checkbox"}
            <CellValue
              prop={p}
              cell={cellOf(r, p.id)}
              {onopen}
              wrap
              ontoggle={p.kind.type === "checkbox" ? (v) => void client.setCell(r.id, p.id, { type: "checkbox", value: v }).then(load).catch(onerror) : undefined}
            />
          {:else}
            <span class="prop-empty">Empty</span>
          {/if}
        </div>
      </div>
    {/each}
    <button class="prop-add" onclick={(e) => (propForm = { prop: null, anchor: e.currentTarget })}><Icon name="plus" size={16} />Add a property</button>
  </div>
{/if}

{#if editing && schema && row}
  <CellEditor {client} {schema} prop={editing.prop} {row} anchor={editing.anchor} onclose={() => ((editing = null), void load())} {onerror} {onopen} />
{/if}
{#if propForm && schema}
  <PropertyForm {client} {schema} prop={propForm.prop} anchor={propForm.anchor} {databases} onclose={() => (propForm = null)} {onerror} onsaved={() => void load()} />
{/if}
