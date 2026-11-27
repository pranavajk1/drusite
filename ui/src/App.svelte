<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";

  import mark from "./assets/mark-64.png";
  import type { CoreClient } from "./core/client";
  import type { AccountStatus, BlockId, PageId, PageInfo, SearchHit, Settings } from "./core/types";
  import { setAttachmentClient } from "./editor/attachments";
  import { setQueryHost } from "./editor/query";
  import { today } from "./journal/dates";
  import { phaseLabel } from "./lib/Account.svelte";
  import PageEditor from "./lib/PageEditor.svelte";
  import PageTree from "./lib/PageTree.svelte";
  import SearchPalette from "./lib/Search.svelte";
  import SettingsSheet, { type Opening } from "./lib/Settings.svelte";
  import TrashSheet from "./lib/Trash.svelte";
  import Icon from "./lib/ui/Icon.svelte";
  import Menu, { type MenuItem } from "./lib/ui/Menu.svelte";

  let { client }: { client: CoreClient } = $props();

  let pages = $state<PageInfo[]>([]);
  let current = $state<PageId | null>(null);
  let error = $state<string | null>(null);
  let readySent = false;
  let account = $state<AccountStatus>({ server: null, email: null, phase: "signedOut", message: null });
  /** The open Settings section, or null when Settings is closed. */
  let settingsAt = $state<Opening | null>(null);
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
  /** On desktops, the sidebar can be put away. */
  let collapsed = $state(localStorage.getItem("drusite.sidebar") === "collapsed");
  /**
   * The tab bar steps aside on a touch screen while the editor has the focus (its formatting bar
   * takes the bottom edge) or while the keyboard is up for some other field. Focus alone doesn't
   * mean a keyboard: iOS doesn't open one for a field focused without a tap.
   */
  let writing = $state(false);
  const touch = window.matchMedia?.("(pointer: coarse)").matches ?? false;
  let tallest = window.innerHeight;
  function keyboardUp(): boolean {
    const vv = window.visualViewport;
    // iOS shrinks only the visual viewport; Android shrinks the window.
    tallest = Math.max(tallest, window.innerHeight);
    return (vv ? window.innerHeight - vv.height > 120 : false) || window.innerHeight < tallest - 120;
  }
  function focusChanged() {
    // After focusout the new focus isn't set yet; look once it is.
    setTimeout(() => {
      if (!touch) return;
      const el = document.activeElement;
      const inEditor = !!el?.closest(".ProseMirror");
      const typing = !!el?.closest("input:not([type=checkbox]):not([type=radio]), textarea");
      writing = inEditor || (typing && keyboardUp());
    });
  }
  onMount(() => {
    window.visualViewport?.addEventListener("resize", focusChanged);
    window.addEventListener("resize", focusChanged);
    return () => {
      window.visualViewport?.removeEventListener("resize", focusChanged);
      window.removeEventListener("resize", focusChanged);
    };
  });

  let pageMenu = $state<HTMLElement | null>(null);

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
    const mod = (e.metaKey || e.ctrlKey) && !e.altKey;
    if (mod && !e.shiftKey && e.key.toLowerCase() === "k") {
      e.preventDefault();
      showSearch = true;
    } else if (mod && !e.shiftKey && e.key === ",") {
      e.preventDefault();
      settingsAt = "home";
    } else if (mod && e.key === "\\") {
      e.preventDefault();
      toggleSidebar();
    }
  }

  function toggleSidebar() {
    collapsed = !collapsed;
    localStorage.setItem("drusite.sidebar", collapsed ? "collapsed" : "open");
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

  function pageActions(id: PageId): MenuItem[] {
    return [
      { label: "Add a page inside", icon: "plus", run: () => createPage(id) },
      { label: "Search", icon: "search", hint: "⌘K", run: () => (showSearch = true) },
      { label: "Move to Trash", icon: "trash-2", danger: true, run: () => deletePage(id) },
    ];
  }

  function opened() {
    if (readySent) return;
    readySent = true;
    void client.ready();
  }

  const byId = $derived(new Map(pages.map((p) => [p.id, p])));
  const info = $derived(current ? (byId.get(current) ?? null) : null);
  const title = $derived(info?.title ?? "");
  /** The open page's ancestors, outermost first, for the breadcrumb. */
  const trail = $derived.by(() => {
    const out: PageInfo[] = [];
    for (let p = info?.parent ? byId.get(info.parent) : undefined; p && out.length < 20; p = p.parent ? byId.get(p.parent) : undefined)
      out.unshift(p);
    return out;
  });
  const syncLabel = $derived(phaseLabel(account.phase));
</script>

<svelte:window onkeydown={globalKey} onfocusin={focusChanged} onfocusout={focusChanged} />

{#snippet syncDot()}
  <span class="dot sync-dot {account.phase}" aria-hidden="true"></span>
{/snippet}

<div class="app" class:drawer-open={drawer} class:collapsed class:writing>
  <aside class="sidebar" aria-label="Sidebar">
    <div class="sidebar-head">
      <div class="workspace">
        <img class="workspace-mark" src={mark} alt="" width="20" height="20" />
        <span class="workspace-name">Drusite</span>
      </div>
      <button class="icon-btn sidebar-collapse" aria-label="Close sidebar" title="Close sidebar (⌘\)" onclick={() => (drawer ? (drawer = false) : toggleSidebar())}>
        <Icon name="panel-left" />
      </button>
    </div>
    <div class="sidebar-scroll">
      <div class="nav-group">
        <button class="nav-row" aria-keyshortcuts="Meta+K Control+K" onclick={() => ((drawer = false), (showSearch = true))}>
          <Icon name="search" /><span class="nav-label">Search</span><kbd>⌘K</kbd>
        </button>
        <button class="nav-row" onclick={() => openDay(today())}>
          <Icon name="calendar-days" /><span class="nav-label">Today</span>
        </button>
        {#if settings.journalParent}
          <button class="nav-row" class:current={current === settings.journalParent} onclick={() => select(settings.journalParent!)}>
            <Icon name="book-open" /><span class="nav-label">Journal</span>
          </button>
        {/if}
      </div>
      <div class="nav-section">
        <span class="nav-section-label">Pages</span>
        <button class="icon-btn nav-section-add" aria-label="Add a page" title="Add a page" onclick={() => createPage(null)}>
          <Icon name="plus" size={14} />
        </button>
      </div>
      <PageTree
        {pages}
        {current}
        journalParent={settings.journalParent}
        onselect={select}
        oncreate={createPage}
        ondelete={deletePage}
        onmove={movePage}
      />
      <div class="nav-group nav-bottom">
        <button class="nav-row" onclick={() => ((drawer = false), (settingsAt = "home"))}>
          <Icon name="settings" /><span class="nav-label">Settings</span>
        </button>
        <button class="nav-row" onclick={() => ((drawer = false), (showTrash = true))}>
          <Icon name="trash-2" /><span class="nav-label">Trash</span>
        </button>
      </div>
    </div>
    <button class="sync-status" onclick={() => ((drawer = false), (settingsAt = "account"))}>
      {@render syncDot()}
      <span class="sync-label">{syncLabel}</span>
      {#if account.phase === "online" && account.email}<span class="sync-email">{account.email}</span>{/if}
    </button>
  </aside>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="drawer-backdrop" onclick={() => (drawer = false)}></div>

  <div class="main">
    <header class="topbar">
      <button class="icon-btn topbar-menu" aria-label="Pages" aria-expanded={drawer} onclick={() => (drawer = !drawer)}>
        <Icon name="menu" size={20} />
      </button>
      {#if collapsed}
        <button class="icon-btn topbar-expand" aria-label="Open sidebar" title="Open sidebar (⌘\)" onclick={toggleSidebar}>
          <Icon name="panel-left" />
        </button>
      {/if}
      <nav class="breadcrumb" aria-label="Breadcrumb">
        {#each trail as p (p.id)}
          <button class="crumb" onclick={() => select(p.id)}>{p.title || "Untitled"}</button>
          <span class="crumb-sep" aria-hidden="true">/</span>
        {/each}
        <span class="crumb current topbar-title" aria-current="page">{title || "Untitled"}</span>
      </nav>
      <button class="topbar-sync" aria-label="Sync: {syncLabel}" title={syncLabel} onclick={() => (settingsAt = "account")}>
        {@render syncDot()}<span class="topbar-sync-label">{syncLabel}</span>
      </button>
      <button
        class="icon-btn topbar-more"
        aria-label="Page actions"
        aria-haspopup="menu"
        aria-expanded={pageMenu !== null}
        onclick={(e) => (pageMenu = pageMenu ? null : e.currentTarget)}
      >
        <Icon name="ellipsis" size={18} />
      </button>
    </header>

    <main class="content">
      {#if error}
        <div class="error" role="alert">
          <Icon name="circle-alert" />
          <span class="error-text">{error}</span>
          <button class="btn small ghost" onclick={() => (error = null)}>Dismiss</button>
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

  <nav class="tabbar" aria-label="Main">
    <button class="tab" aria-label="Pages" onclick={() => (drawer = true)}><Icon name="house" size={22} /><span>Pages</span></button>
    <button class="tab" onclick={() => openDay(today())}><Icon name="calendar-days" size={22} /><span>Today</span></button>
    <button class="tab" onclick={() => (showSearch = true)}><Icon name="search" size={22} /><span>Search</span></button>
    <button class="tab" onclick={() => createPage(null)}><Icon name="square-pen" size={22} /><span>New page</span></button>
  </nav>
</div>

{#if pageMenu && current}
  <Menu anchor={pageMenu} label="Page actions" onclose={() => (pageMenu = null)} items={pageActions(current)} />
{/if}
{#if showSearch}
  <SearchPalette {client} {pages} onopen={openHit} onclose={() => (showSearch = false)} />
{/if}
{#if showTrash}
  <TrashSheet {client} onrestore={select} onclose={() => (showTrash = false)} onerror={show} />
{/if}
{#if trashed}
  <div class="toast" role="status">
    <Icon name="trash-2" />
    <span class="toast-text">Moved “{trashed.title}” to the trash</span>
    <button class="toast-action" onclick={undoDelete}>Undo</button>
  </div>
{/if}
{#if settingsAt}
  <SettingsSheet
    {client}
    {settings}
    {pages}
    {account}
    section={settingsAt}
    onsection={(s) => (settingsAt = s)}
    onclose={() => (settingsAt = null)}
    onerror={show}
    onopen={select}
  />
{/if}
