<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "./core/client";
  import type { AccountStatus, BlockId, PageId, PageInfo, SearchHit, Settings } from "./core/types";
  import { setAttachmentClient } from "./editor/attachments";
  import { setQueryHost } from "./editor/query";
  import { today } from "./journal/dates";
  import Account, { phaseLabel } from "./lib/Account.svelte";
  import PageEditor from "./lib/PageEditor.svelte";
  import PageTree from "./lib/PageTree.svelte";
  import SearchPalette from "./lib/Search.svelte";
  import SettingsSheet from "./lib/Settings.svelte";
  import TrashSheet from "./lib/Trash.svelte";

  let { client }: { client: CoreClient } = $props();

  let pages = $state<PageInfo[]>([]);
  let current = $state<PageId | null>(null);
  let error = $state<string | null>(null);
  let readySent = false;
  let account = $state<AccountStatus>({ server: null, email: null, phase: "signedOut", message: null });
  let showAccount = $state(false);
  let showSettings = $state(false);
  let showSearch = $state(false);
  let showTrash = $state(false);
  /** The page just moved to the trash, for the Undo toast. */
  let trashed = $state<{ id: PageId; title: string } | null>(null);
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  /** The block a search hit points at, for the editor to show; bumped so the same one can repeat. */
  let reveal = $state<{ block: BlockId; seq: number } | null>(null);
  let settings = $state<Settings>({ journalParent: null, journalTemplate: null, weekStart: 1 });
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
      if (event.type === "settingsChanged") settings = event.settings;
    });
    setQueryHost({ client, open: (id) => select(id), pages: () => pages });
    setAttachmentClient(client);
    (async () => {
      try {
        account = await client.accountStatus();
        settings = await client.settings();
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

  function openHit(hit: SearchHit) {
    showSearch = false;
    select(hit.page);
    reveal = hit.block ? { block: hit.block, seq: (reveal?.seq ?? 0) + 1 } : null;
  }

  function globalKey(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "k") {
      e.preventDefault();
      showSearch = true;
    }
  }

  /** Opens the journal entry for `date`, making it from the template if it's new. */
  async function openDay(date: string) {
    try {
      const { page } = await client.openJournalDay(date);
      pages = await client.listPages();
      select(page.id);
    } catch (e) {
      show(e);
    }
  }

  async function deletePage(id: PageId) {
    try {
      const page = pages.find((p) => p.id === id);
      const parent = page?.parent ?? null;
      await client.deletePage(id);
      trashed = { id, title: page?.title || "Untitled" };
      clearTimeout(toastTimer);
      toastTimer = setTimeout(() => (trashed = null), 6000);
      // The pages inside go to the trash with it, and may include the open one.
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

  async function undoDelete() {
    if (!trashed) return;
    const { id } = trashed;
    trashed = null;
    try {
      await client.restorePage(id);
      select(id);
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

  const info = $derived(pages.find((p) => p.id === current) ?? null);
  const title = $derived(info?.title ?? "");
</script>

<svelte:window onkeydown={globalKey} />

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
    <div class="sidebar-actions">
      <button class="sidebar-action" aria-keyshortcuts="Meta+K Control+K" onclick={() => (showSearch = true)}>🔍 Search</button>
      <button class="sidebar-action" onclick={() => openDay(today())}>📅 Today</button>
      {#if settings.journalParent}
        <button class="sidebar-action" onclick={() => select(settings.journalParent!)}>📖 Journal</button>
      {/if}
    </div>
    <PageTree {pages} {current} onselect={select} oncreate={createPage} ondelete={deletePage} onmove={movePage} />
    <button class="sidebar-action" onclick={() => (showTrash = true)}>🗑️ Trash</button>
    <button class="sidebar-action" onclick={() => (showSettings = true)}>⚙️ Settings</button>
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
        <PageEditor
          {client}
          page={current}
          {title}
          {info}
          {pages}
          {settings}
          onday={openDay}
          {reveal}
          onselect={select}
          onopened={opened}
          onerror={show}
        />
      {/key}
    {/if}
  </main>
</div>
{#if showAccount}
  <Account {client} status={account} onclose={() => (showAccount = false)} />
{/if}
{#if showSearch}
  <SearchPalette {client} onopen={openHit} onclose={() => (showSearch = false)} />
{/if}
{#if showTrash}
  <TrashSheet {client} onrestore={select} onclose={() => (showTrash = false)} onerror={show} />
{/if}
{#if trashed}
  <div class="toast" role="status">
    <span>Moved “{trashed.title}” to the trash</span>
    <button onclick={undoDelete}>Undo</button>
  </div>
{/if}
{#if showSettings}
  <SettingsSheet {client} {settings} {pages} onclose={() => (showSettings = false)} onerror={show} onopen={select} />
{/if}
