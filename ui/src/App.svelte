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
  import AskPanel from "./lib/Ask.svelte";
  import { openLayer, pushPage, replacePage, startHistory } from "./lib/back";
  import PageEditor from "./lib/PageEditor.svelte";
  import PageTree from "./lib/PageTree.svelte";
  import SearchPalette from "./lib/Search.svelte";
  import SettingsSheet, { type Opening } from "./lib/Settings.svelte";
  import ShareSheet from "./lib/Share.svelte";
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
  /** Ask your notes, beside the page (the whole screen on phones); kept open per device. */
  const phone = () => window.matchMedia?.("(max-width: 700px)").matches ?? false;
  let showAsk = $state(!phone() && localStorage.getItem("drusite.ask") === "open");
  function setAsk(open: boolean) {
    showAsk = open;
    drawer = false;
    if (!phone()) localStorage.setItem("drusite.ask", open ? "open" : "closed");
  }
  /** A citation: its page, at its block. On a phone the panel gives way to the page. */
  function openCited(page: PageId, block: BlockId | null) {
    select(page);
    reveal = block ? { block, seq: (reveal?.seq ?? 0) + 1 } : null;
    if (phone()) showAsk = false;
  }
  /** The page just moved to the trash, for the Undo toast. */
  let trashed = $state<{ id: PageId; title: string } | null>(null);
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  /** The block a search hit points at, for the editor to show; bumped so the same one can repeat. */
  let reveal = $state<{ block: BlockId; seq: number } | null>(null);
  let settings = $state<Settings>({ journalParent: null, journalTemplate: null, weekStart: 1, askJournal: false });
  /** On phones, the page list is a drawer over the page. */
  let drawer = $state(false);
  /** On desktops, the sidebar can be put away, and made wider or narrower. */
  let collapsed = $state(localStorage.getItem("drusite.sidebar") === "collapsed");
  const WIDTH = { min: 200, normal: 248, max: 480 };
  const clampWidth = (w: number) => Math.round(Math.max(WIDTH.min, Math.min(WIDTH.max, w)));
  let sidebarWidth = $state(clampWidth(Number(localStorage.getItem("drusite.sidebarWidth")) || WIDTH.normal));
  let resizing = $state(false);
  let accountMenu = $state<HTMLElement | null>(null);
  /**
   * The tab bar steps aside on a touch screen while the keyboard is up (the editor's formatting
   * bar takes the bottom edge then). Focus alone doesn't mean a keyboard: iOS doesn't open one for
   * a field focused without a tap.
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
      writing = (inEditor || typing) && keyboardUp();
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
  // How much of the window the keyboard covers (iOS, and Android 15+, which draw it over the
  // window instead of shrinking it), so bottom sheets can sit above it: `--keyboard-cover`.
  onMount(() => {
    const vv = window.visualViewport;
    if (!vv) return;
    const cover = () => {
      const px = Math.max(0, Math.round(window.innerHeight - vv.height - vv.offsetTop));
      document.documentElement.style.setProperty("--keyboard-cover", `${px}px`);
    };
    cover();
    vv.addEventListener("resize", cover);
    vv.addEventListener("scroll", cover);
    return () => {
      vv.removeEventListener("resize", cover);
      vv.removeEventListener("scroll", cover);
      document.documentElement.style.removeProperty("--keyboard-cover");
    };
  });

  let pageMenu = $state<HTMLElement | null>(null);
  let shareButton = $state<HTMLElement | null>(null);
  /** Share to web: open, from the top bar's button (desktops) or the page actions (phones). */
  let sharing = $state<{ anchor: HTMLElement | null } | null>(null);

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
          if (first) select(first.id, "replace");
          else void createPage(null, "replace");
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
        stopHistory = startHistory(current, (id) => {
          if (byId.has(id)) select(id, "none");
        });
      } catch (e) {
        show(e);
      }
    })();
    return () => {
      stop();
      stopHistory?.();
    };
  });

  let stopHistory: (() => void) | undefined;

  /** Opens a page. `push` lets Back return to the one before; `replace` doesn't (it went away). */
  function select(id: PageId, how: "push" | "replace" | "none" = "push") {
    if (how === "push") pushPage(id);
    else if (how === "replace") replacePage(id);
    current = id;
    drawer = false;
    localStorage.setItem(LAST, id);
  }

  // What's open over the page closes with Back: the pages drawer, and Ask where it fills a phone.
  $effect(() => {
    if (drawer) return openLayer(() => (drawer = false));
  });
  $effect(() => {
    if (showAsk && phone()) return openLayer(() => (showAsk = false));
  });

  async function createPage(parent: PageId | null, how: "push" | "replace" = "push") {
    try {
      const page = await client.createPage(parent, "");
      select(page.id, how);
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
    } else if (mod && !e.shiftKey && e.key.toLowerCase() === "j") {
      e.preventDefault();
      setAsk(!showAsk);
    } else if (e.key === "Escape" && showAsk && !e.defaultPrevented && !settingsAt && !showSearch && !showTrash) {
      // Esc closes the panel when it has the focus, or when nothing does.
      const at = document.activeElement;
      if (!at || at === document.body || at.closest(".ask")) setAsk(false);
    }
  }

  function toggleSidebar() {
    setCollapsed(!collapsed);
  }

  function setCollapsed(to: boolean) {
    collapsed = to;
    localStorage.setItem("drusite.sidebar", collapsed ? "collapsed" : "open");
  }

  function setWidth(w: number) {
    sidebarWidth = clampWidth(w);
    localStorage.setItem("drusite.sidebarWidth", String(sidebarWidth));
  }

  /**
   * Dragging the sidebar's edge resizes it, no narrower than the narrowest; a click without a
   * drag closes it, as in Notion.
   */
  function startResize(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    const startX = e.clientX;
    const startW = sidebarWidth;
    let moved = false;
    resizing = true;
    const move = (ev: PointerEvent) => {
      const dx = ev.clientX - startX;
      if (Math.abs(dx) > 3) moved = true;
      if (!moved) return;
      sidebarWidth = clampWidth(startW + dx);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      resizing = false;
      if (moved) setWidth(sidebarWidth);
      else setCollapsed(true);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  }

  function resizeKey(e: KeyboardEvent) {
    if (e.key === "ArrowLeft") setWidth(sidebarWidth - 16);
    else if (e.key === "ArrowRight") setWidth(sidebarWidth + 16);
    else if (e.key === "Enter") setCollapsed(true);
    else return;
    e.preventDefault();
  }

  async function signOut() {
    try {
      await client.signOut();
    } catch (e) {
      show(e);
    }
  }

  /** The account menu, at the foot of the sidebar: who and where, then the app-wide places. */
  function accountItems(): MenuItem[] {
    const items: MenuItem[] = [
      { label: "Settings", icon: "settings", hint: "⌘,", run: () => (settingsAt = "home") },
      { label: "Trash", icon: "trash-2", run: () => (showTrash = true) },
    ];
    if (account.server) items.push({ label: "Sign out", icon: "log-out", group: true, run: signOut });
    else items.push({ label: "Sign in to sync", icon: "log-in", group: true, run: () => (settingsAt = "account") });
    return items;
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
        if (next) select(next, "replace");
        else await createPage(null, "replace");
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

  async function createDatabase(parent: PageId | null, template: "empty" | "tasks") {
    try {
      const { page } = await client.createDatabase(parent, template === "tasks" ? "Tasks" : "", false, template);
      select(page.id);
    } catch (e) {
      show(e);
    }
  }

  function pageActions(id: PageId): MenuItem[] {
    return [
      { label: "Add a page inside", icon: "plus", run: () => createPage(id) },
      {
        label: byId.get(id)?.published ? "Published to web" : "Share to web",
        icon: "globe",
        run: () => (sharing = { anchor: phone() ? null : shareButton }),
      },
      { label: "Add a database inside", icon: "table", run: () => createDatabase(id, "empty") },
      { label: "Add a task board inside", icon: "square-kanban", run: () => createDatabase(id, "tasks") },
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
  const host = $derived.by(() => {
    try {
      return account.server ? new URL(account.server).host : null;
    } catch {
      return account.server;
    }
  });
  const who = $derived(account.email ?? (account.server ? host : null));
</script>

<svelte:window onkeydown={globalKey} onfocusin={focusChanged} onfocusout={focusChanged} />

{#snippet syncDot()}
  <span class="dot sync-dot {account.phase}" aria-hidden="true"></span>
{/snippet}

{#snippet avatar(size: number)}
  {#if account.email}
    <span class="avatar" class:avatar-lg={size > 24} aria-hidden="true">{account.email[0]!.toUpperCase()}</span>
  {:else}
    <span class="avatar avatar-none" class:avatar-lg={size > 24} aria-hidden="true"><Icon name="user-round" size={size > 24 ? 18 : 13} /></span>
  {/if}
{/snippet}

{#snippet accountHead()}
  <div class="account-head">
    {@render avatar(32)}
    <div class="account-head-text">
      <span class="account-head-name">{who ?? "Not signed in"}</span>
      <span class="account-head-sync">{@render syncDot()}<span>{syncLabel}</span></span>
      {#if host && account.email}<span class="account-head-host">{host}</span>{/if}
    </div>
  </div>
{/snippet}

<div
  class="app"
  class:drawer-open={drawer}
  class:collapsed
  class:writing
  class:resizing
  class:ask-open={showAsk}
  style:--sidebar-width="{sidebarWidth}px"
>
  <aside class="sidebar" aria-label="Sidebar">
    <div class="sidebar-head">
      <div class="workspace">
        <img class="workspace-mark" src={mark} alt="" width="20" height="20" />
        <span class="workspace-name">Drusite</span>
      </div>
      <button class="icon-btn sidebar-collapse" aria-label="Close sidebar" title="Close sidebar (⌘\)" onclick={() => (drawer ? (drawer = false) : toggleSidebar())}>
        <Icon name="panel-left-close" />
      </button>
    </div>
    <div class="sidebar-scroll">
      <div class="nav-group">
        <button class="nav-row" aria-keyshortcuts="Meta+K Control+K" onclick={() => ((drawer = false), (showSearch = true))}>
          <Icon name="search" /><span class="nav-label">Search</span><kbd>⌘K</kbd>
        </button>
        <button class="nav-row" aria-keyshortcuts="Meta+J Control+J" aria-pressed={showAsk} onclick={() => setAsk(!showAsk)}>
          <Icon name="sparkles" /><span class="nav-label">Ask</span><kbd>⌘J</kbd>
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
        oncreatetasks={() => createDatabase(null, "tasks")}
        ondelete={deletePage}
        onmove={movePage}
      />
    </div>
    <div class="sidebar-foot">
      <button
        class="account-button"
        aria-label="Account: {who ?? 'not signed in'}, {syncLabel}"
        aria-haspopup="menu"
        aria-expanded={accountMenu !== null}
        onclick={(e) => (accountMenu = accountMenu ? null : e.currentTarget)}
      >
        {@render avatar(20)}
        <span class="account-name">{who ?? "Not signed in"}</span>
        {@render syncDot()}
        <Icon name="chevrons-up-down" size={14} />
      </button>
    </div>
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
    <div
      class="sidebar-resize"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize sidebar"
      aria-valuemin={WIDTH.min}
      aria-valuemax={WIDTH.max}
      aria-valuenow={sidebarWidth}
      tabindex="0"
      title="Drag to resize, click to close"
      onpointerdown={startResize}
      onkeydown={resizeKey}
    ></div>
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
          <Icon name="panel-left-open" />
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
        bind:this={shareButton}
        class="btn ghost small topbar-share"
        class:published={info?.published}
        aria-haspopup="dialog"
        aria-expanded={sharing !== null}
        title={info?.published ? "Published to web" : "Share to web"}
        onclick={(e) => (sharing = sharing ? null : { anchor: e.currentTarget })}
      >
        {#if info?.published}<Icon name="globe" size={14} />{/if}Share
      </button>
      <button
        class="icon-btn topbar-ask"
        class:active={showAsk}
        aria-label="Ask your notes"
        title="Ask your notes (⌘J)"
        aria-pressed={showAsk}
        onclick={() => setAsk(!showAsk)}
      >
        <Icon name="sparkles" size={18} />
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

  {#if showAsk}
    <div class="ask-host">
      <AskPanel
        {client}
        {settings}
        signedIn={account.server ?? ""}
        onclose={() => setAsk(false)}
        oncite={openCited}
        onsettings={(s) => (settingsAt = s)}
      />
    </div>
  {/if}

  <nav class="tabbar" aria-label="Main">
    <button class="tab" aria-label="Pages" onclick={() => (drawer = true)}><Icon name="house" size={22} /><span>Pages</span></button>
    <button class="tab" onclick={() => openDay(today())}><Icon name="calendar-days" size={22} /><span>Today</span></button>
    <button class="tab" onclick={() => (showSearch = true)}><Icon name="search" size={22} /><span>Search</span></button>
    <button class="tab" onclick={() => setAsk(true)}><Icon name="sparkles" size={22} /><span>Ask</span></button>
    <button class="tab" onclick={() => createPage(null)}><Icon name="square-pen" size={22} /><span>New page</span></button>
  </nav>
</div>

{#if accountMenu}
  <Menu
    anchor={accountMenu}
    label="Account"
    wide
    header={accountHead}
    onclose={() => (accountMenu = null)}
    items={accountItems().map((item) => ({ ...item, run: () => ((drawer = false), item.run()) }))}
  />
{/if}
{#if pageMenu && current}
  <Menu anchor={pageMenu} label="Page actions" onclose={() => (pageMenu = null)} items={pageActions(current)} />
{/if}
{#if sharing && current}
  {#key current}
    <ShareSheet
      {client}
      page={current}
      {title}
      anchor={sharing.anchor}
      phone={sharing.anchor === null}
      onclose={() => (sharing = null)}
    />
  {/key}
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
