<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "../core/client";
  import type { PageId, TrashItem } from "../core/types";

  let {
    client,
    onrestore,
    onclose,
    onerror,
  }: {
    client: CoreClient;
    onrestore: (id: PageId) => void;
    onclose: () => void;
    onerror: (e: unknown) => void;
  } = $props();

  let items = $state<TrashItem[]>([]);
  let emptying = $state(false);

  onMount(() => {
    const stop = client.subscribe((event) => {
      if (event.type === "trashChanged") items = event.pages;
    });
    client.trash().then((t) => (items = t), onerror);
    return stop;
  });

  const days = (ms: number) => Math.max(0, Math.ceil((ms - Date.now()) / 86_400_000));
  const when = (ms: number) => new Date(ms).toLocaleDateString(undefined, { day: "numeric", month: "short" });

  async function run(f: () => Promise<void>) {
    try {
      await f();
    } catch (e) {
      onerror(e);
    }
  }

  async function restore(id: PageId) {
    await run(() => client.restorePage(id));
    onrestore(id);
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="sheet-backdrop" onclick={onclose}>
  <div class="sheet" role="dialog" aria-modal="true" aria-labelledby="trash-title" tabindex="-1" onclick={(e) => e.stopPropagation()}>
    <div class="sheet-head">
      <h2 id="trash-title">Trash</h2>
      <button class="sheet-close" aria-label="Close" onclick={onclose}>×</button>
    </div>
    <p class="hint">Deleted pages stay here for 30 days, then they're deleted for good.</p>
    {#if items.length === 0}
      <p class="hint">The trash is empty.</p>
    {:else}
      <ul class="trash-list">
        {#each items as item (item.id)}
          <li class="trash-item">
            <div class="trash-title">
              <span>{item.title || "Untitled"}</span>
              <small>
                Deleted {when(item.trashedAt)}{item.subpages > 0
                  ? ` with ${item.subpages} ${item.subpages === 1 ? "subpage" : "subpages"}`
                  : ""} · {days(item.deletedAt)} days left
              </small>
            </div>
            <button onclick={() => restore(item.id)}>Restore</button>
            <button class="danger" aria-label="Delete {item.title || 'Untitled'} forever" onclick={() => run(() => client.deleteForever(item.id))}>Delete forever</button>
          </li>
        {/each}
      </ul>
      {#if emptying}
        <p>Delete {items.length === 1 ? "this page" : `all ${items.length} pages`} for good? This can't be undone.</p>
        <button class="trash-empty" onclick={() => run(async () => { await client.emptyTrash(); emptying = false; })}>Yes, empty the trash</button>
        <button onclick={() => (emptying = false)}>Cancel</button>
      {:else}
        <button class="trash-empty" onclick={() => (emptying = true)}>Empty trash</button>
      {/if}
    {/if}
  </div>
</div>
