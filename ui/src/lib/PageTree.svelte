<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { SvelteSet } from "svelte/reactivity";

  import type { PageId, PageInfo } from "../core/types";

  let {
    pages,
    current,
    onselect,
    oncreate,
    ondelete,
    onmove,
  }: {
    pages: PageInfo[];
    current: PageId | null;
    onselect: (id: PageId) => void;
    oncreate: (parent: PageId | null) => void;
    ondelete: (id: PageId) => void;
    onmove: (id: PageId, parent: PageId | null, index: number) => void;
  } = $props();

  const byId = $derived(new Map(pages.map((p) => [p.id, p])));
  const roots = $derived(pages.filter((p) => p.parent === null).map((p) => p.id));
  // Folded by default, so a large workspace shows its top level; the open page's ancestors unfold.
  const expanded = new SvelteSet<PageId>();
  $effect(() => {
    for (let p = current && byId.get(current)?.parent; p; p = byId.get(p)?.parent) expanded.add(p);
  });
  let confirming = $state<PageId | null>(null);
  let dragging = $state<PageId | null>(null);
  let dropTarget = $state<PageId | "root" | null>(null);

  function askDelete(id: PageId) {
    if (confirming === id) {
      confirming = null;
      ondelete(id);
      return;
    }
    confirming = id;
    setTimeout(() => {
      if (confirming === id) confirming = null;
    }, 3000);
  }

  function drop(e: DragEvent, parent: PageId | null) {
    e.preventDefault();
    const id = dragging;
    dragging = null;
    dropTarget = null;
    if (!id || id === parent) return;
    const siblings = parent === null ? roots : (byId.get(parent)?.children ?? []);
    onmove(id, parent, siblings.filter((s) => s !== id).length);
  }
</script>

{#snippet node(id: PageId, depth: number)}
  {@const page = byId.get(id)}
  {#if page}
    <li>
      <div
        class="tree-row"
        class:current={id === current}
        class:drop={dropTarget === id}
        style:--depth={depth}
        draggable="true"
        role="treeitem"
        aria-selected={id === current}
        tabindex="0"
        ondragstart={(e) => {
          dragging = id;
          e.dataTransfer?.setData("text/plain", id);
        }}
        ondragover={(e) => {
          e.preventDefault();
          dropTarget = id;
        }}
        ondragleave={() => (dropTarget = dropTarget === id ? null : dropTarget)}
        ondrop={(e) => drop(e, id)}
        onclick={() => onselect(id)}
        onkeydown={(e) => e.key === "Enter" && onselect(id)}
      >
        <button
          class="tree-caret"
          aria-label={expanded.has(id) ? "Collapse" : "Expand"}
          disabled={page.children.length === 0}
          onclick={(e) => {
            e.stopPropagation();
            if (expanded.has(id)) expanded.delete(id);
            else expanded.add(id);
          }}>{page.children.length === 0 ? "•" : expanded.has(id) ? "▾" : "▸"}</button
        >
        <span class="tree-title">{page.title || "Untitled"}</span>
        <button
          class="tree-action"
          title="Add a page inside"
          aria-label="Add a page inside"
          onclick={(e) => {
            e.stopPropagation();
            expanded.add(id);
            oncreate(id);
          }}>+</button
        >
        <button
          class="tree-action"
          class:confirm={confirming === id}
          title="Delete"
          aria-label="Delete page"
          onclick={(e) => {
            e.stopPropagation();
            askDelete(id);
          }}>{confirming === id ? "Delete?" : "×"}</button
        >
      </div>
      {#if page.children.length > 0 && expanded.has(id)}
        <ul role="group">
          {#each page.children as child (child)}
            {@render node(child, depth + 1)}
          {/each}
        </ul>
      {/if}
    </li>
  {/if}
{/snippet}

<nav class="tree" aria-label="Pages">
  <ul role="tree">
    {#each roots as id (id)}
      {@render node(id, 0)}
    {/each}
  </ul>
  <div
    class="tree-root-drop"
    class:drop={dropTarget === "root"}
    role="presentation"
    ondragover={(e) => {
      e.preventDefault();
      dropTarget = "root";
    }}
    ondrop={(e) => drop(e, null)}
  >
    <button class="new-page" onclick={() => oncreate(null)}>+ New page</button>
  </div>
</nav>
