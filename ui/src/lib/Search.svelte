<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "../core/client";
  import type { PageInfo, SearchHit } from "../core/types";
  import Icon from "./ui/Icon.svelte";
  import Modal from "./ui/Modal.svelte";

  let {
    client,
    pages = [],
    onopen,
    onclose,
  }: {
    client: CoreClient;
    /** The page list, to show where each hit is. */
    pages?: PageInfo[];
    onopen: (hit: SearchHit) => void;
    onclose: () => void;
  } = $props();

  let input: HTMLInputElement;
  let query = $state("");
  let hits = $state<SearchHit[]>([]);
  let active = $state(0);
  let seq = 0;

  onMount(() => input.focus());

  const byId = $derived(new Map(pages.map((p) => [p.id, p])));

  /** "in Parent", or nothing for a top-level page. */
  function where(hit: SearchHit): string {
    const parent = byId.get(hit.page)?.parent;
    const p = parent ? byId.get(parent) : undefined;
    return p ? p.title || "Untitled" : "";
  }

  async function run() {
    const mine = ++seq;
    const q = query;
    try {
      const found = q.trim() === "" ? [] : await client.search(q, 20);
      if (mine !== seq) return;
      hits = found;
      active = 0;
    } catch {
      if (mine === seq) hits = [];
    }
  }

  function key(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      onclose();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      active = Math.min(active + 1, hits.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      active = Math.max(active - 1, 0);
    } else if (e.key === "Enter" && hits[active]) {
      e.preventDefault();
      onopen(hits[active]!);
    } else return;
    document.getElementById(`search-hit-${active}`)?.scrollIntoView({ block: "nearest" });
  }
</script>

<Modal label="Search" size="palette" phone="full" {onclose} class="search">
  <div class="search-bar">
    <Icon name="search" size={20} />
    <input
      bind:this={input}
      class="search-input"
      type="search"
      placeholder="Search your pages"
      aria-label="Search your pages"
      aria-controls="search-results"
      aria-activedescendant={hits.length ? `search-hit-${active}` : undefined}
      autocomplete="off"
      spellcheck="false"
      enterkeyhint="go"
      bind:value={query}
      oninput={run}
      onkeydown={key}
    />
    <button class="btn small ghost search-cancel" onclick={onclose}>Cancel</button>
  </div>
  {#if query.trim() !== ""}
    <ul id="search-results" class="search-results" role="listbox" aria-label="Results">
      {#each hits as hit, i (hit.page)}
        {@const place = where(hit)}
        <li id="search-hit-{i}" role="option" aria-selected={i === active}>
          <button class="search-hit" class:active={i === active} onmouseenter={() => (active = i)} onclick={() => onopen(hit)}>
            <Icon name={byId.get(hit.page)?.kind === "journal" ? "calendar-days" : "file-text"} size={18} />
            <span class="search-text">
              <span class="search-title">{hit.title || "Untitled"}{#if place}<span class="search-where">— {place}</span>{/if}</span>
              {#if hit.block}
                <span class="search-snippet">
                  {#each hit.snippet as part, j (j)}{#if part.highlight}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}
                </span>
              {/if}
            </span>
            {#if i === active}<Icon name="chevron-right" size={14} class="search-enter" />{/if}
          </button>
        </li>
      {:else}
        <li class="search-none empty">
          <Icon name="search" size={28} />
          <span>Nothing matches “{query.trim()}”.</span>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="search-idle">Search titles and text in every page.</p>
  {/if}
  <div class="search-foot" aria-hidden="true">
    <span><kbd>↑</kbd><kbd>↓</kbd> Select</span>
    <span><kbd>↵</kbd> Open</span>
    <span><kbd>esc</kbd> Close</span>
  </div>
</Modal>
