<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "./core/client";
  import type { PageId, PageInfo } from "./core/types";
  import PageEditor from "./lib/PageEditor.svelte";
  import PageTree from "./lib/PageTree.svelte";

  let { client }: { client: CoreClient } = $props();

  let pages = $state<PageInfo[]>([]);
  let current = $state<PageId | null>(null);
  let error = $state<string | null>(null);
  let readySent = false;

  const LAST = "drusite.lastPage";

  function show(e: unknown) {
    error = e instanceof Error ? e.message : String(e);
  }

  onMount(() => {
    const stop = client.subscribe((event) => {
      if (event.type === "pagesChanged") pages = event.pages;
    });
    (async () => {
      try {
        pages = await client.listPages();
        if (pages.length === 0) {
          const first = await client.createPage(null, "");
          pages = await client.listPages();
          current = first.id;
        } else {
          const last = localStorage.getItem(LAST);
          current = pages.find((p) => p.id === last)?.id ?? pages[0]!.id;
        }
      } catch (e) {
        show(e);
      }
    })();
    return stop;
  });

  function select(id: PageId) {
    current = id;
    localStorage.setItem(LAST, id);
  }

  async function createPage(parent: PageId | null) {
    try {
      const page = await client.createPage(parent, "");
      select(page.id);
    } catch (e) {
      show(e);
    }
  }

  async function deletePage(id: PageId) {
    try {
      await client.deletePage(id);
      if (current === id) {
        const rest = await client.listPages();
        current = rest[0]?.id ?? null;
        if (current === null) await createPage(null);
      }
    } catch (e) {
      show(e);
    }
  }

  async function movePage(id: PageId, parent: PageId | null, index: number) {
    try {
      await client.movePage(id, parent, index);
    } catch (e) {
      show(e);
    }
  }

  function opened() {
    if (readySent) return;
    readySent = true;
    void client.ready();
  }

  const title = $derived(pages.find((p) => p.id === current)?.title ?? "");
</script>

<div class="app">
  <aside class="sidebar">
    <div class="sidebar-head">Drusite</div>
    <PageTree {pages} {current} onselect={select} oncreate={createPage} ondelete={deletePage} onmove={movePage} />
  </aside>
  <main class="main">
    {#if error}
      <div class="error" role="alert">
        {error}
        <button onclick={() => (error = null)}>Dismiss</button>
      </div>
    {/if}
    {#if current}
      {#key current}
        <PageEditor {client} page={current} {title} onopened={opened} onerror={show} />
      {/key}
    {/if}
  </main>
</div>
