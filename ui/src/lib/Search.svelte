<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "../core/client";
  import type { SearchHit } from "../core/types";

  let {
    client,
    onopen,
    onclose,
  }: {
    client: CoreClient;
    onopen: (hit: SearchHit) => void;
    onclose: () => void;
  } = $props();

  let input: HTMLInputElement;
  let query = $state("");
  let hits = $state<SearchHit[]>([]);
  let active = $state(0);
  let seq = 0;

  onMount(() => input.focus());

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
    if (e.key === "Escape") onclose();
    else if (e.key === "ArrowDown") {
      e.preventDefault();
      active = Math.min(active + 1, hits.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      active = Math.max(active - 1, 0);
    } else if (e.key === "Enter" && hits[active]) {
      e.preventDefault();
      onopen(hits[active]!);
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="sheet-backdrop search-backdrop" onclick={onclose}>
  <div class="search" role="dialog" aria-modal="true" aria-label="Search" tabindex="-1" onclick={(e) => e.stopPropagation()}>
    <input
      bind:this={input}
      class="search-input"
      type="search"
      placeholder="Search your pages"
      aria-label="Search your pages"
      aria-controls="search-results"
      autocomplete="off"
      spellcheck="false"
      bind:value={query}
      oninput={run}
      onkeydown={key}
    />
    {#if query.trim() !== ""}
      <ul id="search-results" class="search-results" role="listbox" aria-label="Results">
        {#each hits as hit, i (hit.page)}
          <li role="option" aria-selected={i === active}>
            <button class:active={i === active} onmouseenter={() => (active = i)} onclick={() => onopen(hit)}>
              <span class="search-title">{hit.title || "Untitled"}</span>
              {#if hit.block}
                <span class="search-snippet">
                  {#each hit.snippet as part, j (j)}{#if part.highlight}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}
                </span>
              {/if}
            </button>
          </li>
        {:else}
          <li class="search-none">Nothing matches “{query.trim()}”.</li>
        {/each}
      </ul>
    {/if}
  </div>
</div>
