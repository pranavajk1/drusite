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
  // Folded until you unfold a page, so a large workspace shows its top level. What you unfold is
  // remembered across restarts, and the open page's ancestors always unfold.
  const EXPANDED = "drusite.expanded";
  const expanded = new SvelteSet<PageId>(readExpanded());
  function readExpanded(): PageId[] {
    try {
      const ids: unknown = JSON.parse(localStorage.getItem(EXPANDED) ?? "[]");
      return Array.isArray(ids) ? ids.filter((id): id is PageId => typeof id === "string") : [];
    } catch {
      return [];
    }
  }
  $effect(() => {
    for (let p = current && byId.get(current)?.parent; p; p = byId.get(p)?.parent) expanded.add(p);
  });
  $effect(() => {
    // Forget pages that are gone, once the tree has loaded.
    if (pages.length === 0) return;
    for (const id of expanded) if (!byId.has(id)) expanded.delete(id);
    localStorage.setItem(EXPANDED, JSON.stringify([...expanded]));
  });
  let dragging = $state<PageId | null>(null);
  // Where a dragged page would land: before or after a row, inside it, or at the end of the top level.
  type Zone = "before" | "inside" | "after";
  let dropTarget = $state<{ id: PageId; zone: Zone } | "root" | null>(null);

  /** Whether `id` is `ancestor` or inside it, so dropping `ancestor` there would make a loop. */
  function within(id: PageId | null, ancestor: PageId): boolean {
    for (let p: PageId | null = id; p; p = byId.get(p)?.parent ?? null) if (p === ancestor) return true;
    return false;
  }

  function zoneAt(e: DragEvent, row: HTMLElement): Zone {
    const { top, height } = row.getBoundingClientRect();
    const y = (e.clientY - top) / height;
    return y < 0.25 ? "before" : y > 0.75 ? "after" : "inside";
  }

  /** The parent and index a drop on `target` means, or null when it isn't allowed. */
  function placement(id: PageId, target: { id: PageId; zone: Zone } | "root"): [PageId | null, number] | null {
    const siblingsOf = (parent: PageId | null) =>
      (parent === null ? roots : (byId.get(parent)?.children ?? [])).filter((s) => s !== id);
    if (target === "root") return [null, siblingsOf(null).length];
    if (target.zone === "inside") {
      if (within(target.id, id)) return null;
      return [target.id, siblingsOf(target.id).length];
    }
    if (target.id === id) return null;
    const parent = byId.get(target.id)?.parent ?? null;
    if (within(parent, id)) return null;
    const at = siblingsOf(parent).indexOf(target.id);
    return [parent, target.zone === "before" ? at : at + 1];
  }

  function over(e: DragEvent, target: { id: PageId; zone: Zone } | "root") {
    if (!dragging || !placement(dragging, target)) {
      dropTarget = null;
      return;
    }
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    dropTarget = target;
  }

  function drop(e: DragEvent) {
    e.preventDefault();
    const id = dragging;
    const target = dropTarget;
    dragging = null;
    dropTarget = null;
    const place = id && target && placement(id, target);
    if (!id || !place) return;
    const [parent, index] = place;
    if (parent) expanded.add(parent);
    onmove(id, parent, index);
  }
</script>

{#snippet node(id: PageId, depth: number)}
  {@const page = byId.get(id)}
  {#if page}
    <li>
      <div
        class="tree-row"
        class:current={id === current}
        class:drop={dropTarget !== null && dropTarget !== "root" && dropTarget.id === id && dropTarget.zone === "inside"}
        class:drop-before={dropTarget !== null && dropTarget !== "root" && dropTarget.id === id && dropTarget.zone === "before"}
        class:drop-after={dropTarget !== null && dropTarget !== "root" && dropTarget.id === id && dropTarget.zone === "after"}
        style:--depth={depth}
        draggable="true"
        role="treeitem"
        aria-selected={id === current}
        tabindex="0"
        ondragstart={(e) => {
          dragging = id;
          e.dataTransfer?.setData("text/plain", id);
        }}
        ondragend={() => {
          dragging = null;
          dropTarget = null;
        }}
        ondragover={(e) => over(e, { id, zone: zoneAt(e, e.currentTarget) })}
        ondrop={drop}
        onclick={() => onselect(id)}
        onkeydown={(e) => e.key === "Enter" && onselect(id)}
      >
        <button
          class="tree-caret"
          aria-label={expanded.has(id) ? "Collapse" : "Expand"}
          aria-expanded={page.children.length > 0 ? expanded.has(id) : undefined}
          disabled={page.children.length === 0}
          onclick={(e) => {
            e.stopPropagation();
            if (expanded.has(id)) expanded.delete(id);
            else expanded.add(id);
          }}>{page.children.length === 0 ? "•" : "▶"}</button
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
          title="Move to Trash"
          aria-label="Delete page"
          onclick={(e) => {
            e.stopPropagation();
            ondelete(id);
          }}>×</button
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
    ondragover={(e) => over(e, "root")}
    ondrop={drop}
  >
    <button class="new-page" onclick={() => oncreate(null)}>+ New page</button>
  </div>
</nav>
