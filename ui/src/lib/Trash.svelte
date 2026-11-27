<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "../core/client";
  import type { PageId, TrashItem } from "../core/types";
  import Icon from "./ui/Icon.svelte";
  import Modal from "./ui/Modal.svelte";

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

<Modal title="Trash" {onclose}>
  <div class="modal-body">
    <p class="hint">Deleted pages stay here for 30 days, then they're deleted for good.</p>
    {#if items.length === 0}
      <div class="empty">
        <Icon name="trash-2" size={28} />
        <span>The trash is empty.</span>
      </div>
    {:else}
      <ul class="list trash-list">
        {#each items as item (item.id)}
          <li class="list-row trash-item">
            <Icon name="file-text" class="list-row-icon" />
            <div class="list-row-text trash-title">
              <span>{item.title || "Untitled"}</span>
              <small>
                Deleted {when(item.trashedAt)}{item.subpages > 0
                  ? ` with ${item.subpages} ${item.subpages === 1 ? "subpage" : "subpages"}`
                  : ""} · {days(item.deletedAt)} days left
              </small>
            </div>
            <button class="icon-btn" aria-label="Restore" title="Restore" onclick={() => restore(item.id)}>
              <Icon name="rotate-ccw" />
            </button>
            <button
              class="icon-btn danger-hover"
              aria-label="Delete {item.title || 'Untitled'} forever"
              title="Delete forever"
              onclick={() => run(() => client.deleteForever(item.id))}><Icon name="trash-2" /></button
            >
          </li>
        {/each}
      </ul>
    {/if}
  </div>
  {#if items.length > 0}
    <div class="modal-foot">
      {#if emptying}
        <p class="modal-foot-text">
          Delete {items.length === 1 ? "this page" : `all ${items.length} pages`} for good? This can't be undone.
        </p>
        <button class="btn ghost" onclick={() => (emptying = false)}>Cancel</button>
        <button
          class="btn danger solid"
          onclick={() =>
            run(async () => {
              await client.emptyTrash();
              emptying = false;
            })}>Yes, empty the trash</button
        >
      {:else}
        <span class="modal-foot-text hint">{items.length} {items.length === 1 ? "page" : "pages"}</span>
        <button class="btn danger" onclick={() => (emptying = true)}>Empty trash</button>
      {/if}
    </div>
  {/if}
</Modal>
