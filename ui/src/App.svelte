<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "./core/client";
  import type { AccountStatus, PageId, PageInfo } from "./core/types";
  import Account, { phaseLabel } from "./lib/Account.svelte";
  import PageEditor from "./lib/PageEditor.svelte";
  import PageTree from "./lib/PageTree.svelte";

  let { client }: { client: CoreClient } = $props();

  let pages = $state<PageInfo[]>([]);
  let current = $state<PageId | null>(null);
  let error = $state<string | null>(null);
  let readySent = false;
  let account = $state<AccountStatus>({ server: null, email: null, phase: "signedOut", message: null });
  let showAccount = $state(false);
  /** On phones, the page list is a drawer over the page. */
  let drawer = $state(false);

  const LAST = "drusite.lastPage";

  function show(e: unknown) {
    error = e instanceof Error ? e.message : String(e);
  }

  onMount(() => {
    const stop = client.subscribe((event) => {
      if (event.type === "pagesChanged") {
        pages = event.pages;
        // The open page can go away underneath: deleted on another device, or a new device's
        // empty first page given up for the account's notes.
        if (current !== null && !pages.some((p) => p.id === current)) {
          const first = pages[0];
          if (first) select(first.id);
          else void createPage(null);
        }
      }
      if (event.type === "accountChanged") account = event.status;
    });
    (async () => {
      try {
        account = await client.accountStatus();
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
    drawer = false;
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
      const parent = pages.find((p) => p.id === id)?.parent ?? null;
      await client.deletePage(id);
      // Deleting a page deletes the pages inside it, which may include the open one.
      const rest = await client.listPages();
      if (current === null || !rest.some((p) => p.id === current)) {
        const next = rest.find((p) => p.id === parent)?.id ?? rest[0]?.id;
        if (next) select(next);
        else await createPage(null);
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

<div class="app" class:drawer-open={drawer}>
  <header class="topbar">
    <button class="topbar-menu" aria-label="Pages" aria-expanded={drawer} onclick={() => (drawer = !drawer)}>☰</button>
    <span class="topbar-title">{title || "Untitled"}</span>
    <button class="topbar-sync" aria-label="Sync: {phaseLabel(account.phase)}" onclick={() => (showAccount = true)}>
      <span class="sync-dot {account.phase}"></span>
    </button>
  </header>
  <aside class="sidebar">
    <div class="sidebar-head">Drusite</div>
    <PageTree {pages} {current} onselect={select} oncreate={createPage} ondelete={deletePage} onmove={movePage} />
    <button class="sync-status" onclick={() => (showAccount = true)}>
      <span class="sync-dot {account.phase}"></span>
      <span class="sync-label">{phaseLabel(account.phase)}{account.phase === "online" && account.email ? ` · ${account.email}` : ""}</span>
    </button>
  </aside>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="drawer-backdrop" onclick={() => (drawer = false)}></div>
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
{#if showAccount}
  <Account {client} status={account} onclose={() => (showAccount = false)} />
{/if}
