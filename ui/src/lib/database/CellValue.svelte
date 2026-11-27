<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- A cell's value as it shows: chips for options and people, a checkbox, links to rows. -->
<script lang="ts">
  import type { Cell, PageId, PropertyDef } from "../../core/types";
  import Icon from "../ui/Icon.svelte";
  import { option } from "./util";

  let {
    prop,
    cell,
    onopen,
    ontoggle,
    wrap = false,
  }: {
    prop: PropertyDef;
    cell: Cell | undefined;
    onopen?: (page: PageId) => void;
    /** A checkbox was clicked. */
    ontoggle?: (checked: boolean) => void;
    wrap?: boolean;
  } = $props();

  const v = $derived(cell?.value ?? null);
</script>

{#if cell?.error}
  <span class="cell-error" title={cell.error}><Icon name="circle-alert" size={14} /> Error</span>
{:else if prop.kind.type === "checkbox"}
  <input
    type="checkbox"
    class="checkbox"
    aria-label={prop.name}
    checked={v?.type === "checkbox" && v.value}
    disabled={!ontoggle}
    onclick={(e) => {
      e.stopPropagation();
      ontoggle?.((e.currentTarget as HTMLInputElement).checked);
    }}
  />
{:else if v && (v.type === "select" || v.type === "status")}
  {@const o = option(prop, v.value)}
  {#if o}<span class="chip tag-{o.color}" class:status-chip={v.type === "status"}>{#if v.type === "status"}<span class="status-dot"></span>{/if}{o.name}</span>{/if}
{:else if v && v.type === "multiSelect"}
  <span class="chips" class:wrap>
    {#each v.value as id (id)}
      {@const o = option(prop, id)}
      {#if o}<span class="chip tag-{o.color}">{o.name}</span>{/if}
    {/each}
  </span>
{:else if v && v.type === "people"}
  <span class="chips" class:wrap>
    {#each v.value as p (p.id)}
      <span class="person"><span class="person-avatar" aria-hidden="true">{(p.name || "?")[0]!.toUpperCase()}</span>{p.name}</span>
    {/each}
  </span>
{:else if cell && cell.pages.length}
  <span class="chips" class:wrap>
    {#each cell.pages as p (p.id)}
      <button
        class="row-link"
        onclick={(e) => {
          e.stopPropagation();
          onopen?.(p.id);
        }}><Icon name="file-text" size={14} />{p.title || "Untitled"}</button
      >
    {/each}
  </span>
{:else if v && v.type === "url"}
  <a class="cell-link" href={v.value} target="_blank" rel="noopener noreferrer" onclick={(e) => e.stopPropagation()}>{v.value}</a>
{:else if v && v.type === "email"}
  <a class="cell-link" href="mailto:{v.value}" onclick={(e) => e.stopPropagation()}>{v.value}</a>
{:else if cell?.computed?.type === "bool"}
  <input type="checkbox" class="checkbox" aria-label={prop.name} checked={cell.computed.value} disabled />
{:else}
  <span class="cell-text" class:wrap class:number={v?.type === "number" || cell?.computed?.type === "number"}>{cell?.text ?? ""}</span>
{/if}
