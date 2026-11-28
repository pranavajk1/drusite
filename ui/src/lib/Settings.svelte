<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Settings: a modal with its sections down the left, as Notion's. On phones it fills the screen:
  a list of sections first, each opening as its own page with a back button.
-->
<script lang="ts" module>
  import type { IconName } from "./icons/icons";

  export type Section = "account" | "appearance" | "journal" | "markdown" | "import" | "ai" | "agents";
  /** Where Settings opens: a section, or "home" (the first section on desktops, the list on phones). */
  export type Opening = Section | "home";

  const SECTIONS: { id: Section; label: string; icon: IconName; group: string; desc: string }[] = [
    { id: "account", label: "Account & sync", icon: "user-round", group: "Account", desc: "The server this device syncs your notes with." },
    { id: "appearance", label: "Appearance", icon: "palette", group: "Account", desc: "How Drusite looks on this device." },
    {
      id: "journal",
      label: "Journal",
      icon: "book-open",
      group: "Notes",
      desc: "Where entries go and what they start from. These sync, so every device you sign in on has them.",
    },
    { id: "markdown", label: "Markdown folder", icon: "folder-sync", group: "Notes", desc: "Your pages as Markdown files on this computer." },
    { id: "import", label: "Import", icon: "folder-input", group: "Notes", desc: "Bring in notes from Notion, or a folder of Markdown notes." },
    { id: "ai", label: "AI", icon: "sparkles", group: "Connections", desc: "Models that answer questions about your notes." },
    { id: "agents", label: "Agents", icon: "bot", group: "Connections", desc: "Agents such as Claude Code that work with your notes." },
  ];
</script>

<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "../core/client";
  import { setThemeChoice, themeChoice, type ThemeChoice } from "../core/theme";
  import type { AccountStatus, BridgeRound, BridgeStatus, PageId, PageInfo, Setting, Settings } from "../core/types";
  import { WEEKDAYS } from "../journal/dates";
  import Account from "./Account.svelte";
  import Agents from "./Agents.svelte";
  import Ai from "./Ai.svelte";
  import { openLayer } from "./back";
  import NotionImport from "./NotionImport.svelte";
  import Icon from "./ui/Icon.svelte";
  import Modal from "./ui/Modal.svelte";
  import Select from "./ui/Select.svelte";
  import Switch from "./ui/Switch.svelte";

  let {
    client,
    settings,
    pages,
    account,
    section,
    onsection,
    onclose,
    onerror,
    onopen,
  }: {
    client: CoreClient;
    settings: Settings;
    pages: PageInfo[];
    account: AccountStatus;
    section: Opening;
    onsection: (s: Opening) => void;
    onclose: () => void;
    onerror: (e: unknown) => void;
    /** Opens a page (the one an import made). */
    onopen: (id: PageId) => void;
  } = $props();

  const server = $derived(account.phase === "signedOut" ? null : account.server);
  const candidates = $derived(pages.filter((p) => p.kind === "page"));
  const pageOptions = (empty: string) => [
    { value: "", label: empty },
    ...candidates.map((p) => ({ value: p.id as string, label: p.title || "Untitled" })),
  ];

  /**
   * What new journal entries can start from: an empty page, or any ordinary page (named with the
   * pages it's inside, so two "Daily" pages can be told apart). Never the page entries go under,
   * an entry, a database or a database's row; the core refuses those too.
   */
  const templateOptions = $derived.by(() => {
    const byId = new Map(pages.map((p) => [p.id, p]));
    const path = (p: PageInfo) => {
      const names = [p.title || "Untitled"];
      for (let at = p.parent ? byId.get(p.parent) : undefined, n = 0; at && n < 20; at = at.parent ? byId.get(at.parent) : undefined, n++)
        names.unshift(at.title || "Untitled");
      return names.join(" / ");
    };
    const usable = pages
      .filter((p) => p.kind === "page" && p.id !== settings.journalParent && byId.get(p.parent!)?.kind !== "database")
      .map((p) => ({ value: p.id as string, label: path(p) }))
      .sort((a, b) => a.label.localeCompare(b.label));
    return [{ value: "", label: "Empty page" }, ...usable];
  });

  // The Markdown folder and import: desktops only, and only on this device.
  let bridge = $state<BridgeStatus | null>(null);
  let busy = $state(false);
  onMount(() => {
    const stop = client.subscribe((event) => {
      if (event.type === "bridgeChanged") bridge = event.status;
    });
    client.bridgeStatus().then((s) => (bridge = s), onerror);
    return stop;
  });

  // Importing from Notion works everywhere; the Markdown folder only where there's a bridge.
  const shown = $derived(SECTIONS.filter((s) => s.id !== "markdown" || bridge !== null));
  // On phones a section is a screen of its own: Back goes to the list.
  $effect(() => {
    if (section !== "home" && (window.matchMedia?.("(max-width: 700px)").matches ?? false))
      return openLayer(() => onsection("home"));
  });

  /** The section on screen. On phones "home" is the list; on desktops, the first section. */
  const current = $derived<Section>(section === "home" ? "account" : section);
  const info = $derived(SECTIONS.find((s) => s.id === current)!);
  const groups = $derived([...new Set(shown.map((s) => s.group))]);

  async function set(setting: Setting) {
    try {
      await client.setSetting(setting);
    } catch (e) {
      onerror(e);
    }
  }

  const page = (v: string): PageId | null => (v === "" ? null : (v as PageId));

  let theme = $state<ThemeChoice>(themeChoice());
  function pickTheme(t: ThemeChoice) {
    theme = t;
    setThemeChoice(t);
  }

  async function setFolder(folder: string | null) {
    busy = true;
    try {
      bridge = await client.setBridgeFolder(folder);
    } catch (e) {
      onerror(e);
    } finally {
      busy = false;
    }
  }

  /** The system's folder picker; a failure to open it is shown, not swallowed. */
  async function pick(title: string): Promise<string | null> {
    try {
      return await client.pickFolder(title);
    } catch (e) {
      onerror(e);
      return null;
    }
  }

  async function chooseFolder() {
    const folder = await pick("Choose the folder for your pages");
    if (folder) await setFolder(folder);
  }

  let imported = $state<string | null>(null);
  async function importNotes() {
    const folder = await pick("Choose a folder of notes to import");
    if (!folder) return;
    busy = true;
    imported = "Importing…";
    try {
      const r = await client.importFolder(folder);
      const n = r.unresolved.length;
      const missing = n === 0 ? "" : n === 1 ? " 1 link or embed pointed at nothing." : ` ${n} links or embeds pointed at nothing.`;
      imported = `Imported ${r.pages} ${r.pages === 1 ? "page" : "pages"} and ${r.attachments} ${r.attachments === 1 ? "file" : "files"}.${missing}`;
      if (r.page) onopen(r.page);
    } catch (e) {
      imported = null;
      onerror(e);
    } finally {
      busy = false;
    }
  }

  function summary(r: BridgeRound): string {
    if (r.error) return r.error;
    const parts: string[] = [];
    if (r.merged) parts.push(`${r.merged} edited ${r.merged === 1 ? "file" : "files"} read`);
    if (r.created) parts.push(`${r.created} new ${r.created === 1 ? "page" : "pages"}`);
    if (r.trashed) parts.push(`${r.trashed} moved to the trash`);
    if (r.written) parts.push(`${r.written} ${r.written === 1 ? "file" : "files"} written`);
    if (r.problems.length) parts.push(`${r.problems.length} couldn't be read: ${r.problems.join("; ")}`);
    return parts.length ? `Last change: ${parts.join(", ")}.` : "Up to date.";
  }
</script>

<Modal label="Settings" size="lg" phone="full" head={false} {onclose} class="settings {section === 'home' ? 'at-home' : 'in-section'}">
  <nav class="settings-nav" aria-label="Settings sections">
    <div class="settings-nav-head">
      <h2 class="settings-nav-title">Settings</h2>
      <button class="icon-btn settings-close-phone" aria-label="Close" onclick={onclose}><Icon name="x" size={20} /></button>
    </div>
    {#each groups as group (group)}
      <div class="settings-nav-group">
        <span class="settings-nav-label">{group}</span>
        {#each shown.filter((s) => s.group === group) as s (s.id)}
          <button
            class="settings-nav-row"
            class:current={s.id === current}
            aria-current={s.id === current ? "page" : undefined}
            onclick={() => onsection(s.id)}
          >
            <Icon name={s.icon} />
            <span class="nav-label">{s.label}</span>
            <Icon name="chevron-right" size={16} class="settings-nav-chevron" />
          </button>
        {/each}
      </div>
    {/each}
  </nav>

  <section class="settings-page" aria-labelledby="settings-section-title">
    <header class="settings-page-head">
      <button class="icon-btn settings-back" aria-label="All settings" onclick={() => onsection("home")}>
        <Icon name="chevron-left" size={22} />
      </button>
      <h2 id="settings-section-title" class="settings-page-title">{info.label}</h2>
      <button class="icon-btn modal-close" aria-label="Close" onclick={onclose}><Icon name="x" size={18} /></button>
    </header>
    <div class="settings-page-body">
      <p class="settings-page-desc">{info.desc}</p>

      {#if current === "account"}
        <Account {client} status={account} />
      {:else if current === "appearance"}
        <div class="set-group">
          <div class="set-row">
            <div class="set-row-text">
              <label class="set-label" for="theme">Theme</label>
              <span class="set-desc">Light, dark, or whatever this device is set to.</span>
            </div>
            <Select
              id="theme"
              value={theme}
              options={[
                { value: "system", label: "Use system setting" },
                { value: "light", label: "Light" },
                { value: "dark", label: "Dark" },
              ]}
              onchange={pickTheme}
            />
          </div>
        </div>
      {:else if current === "journal"}
        <div class="set-group">
          <div class="set-row">
            <div class="set-row-text">
              <label class="set-label" for="journal-parent">Journal entries go under</label>
              <span class="set-desc">The page that holds a page per day.</span>
            </div>
            <Select
              id="journal-parent"
              value={settings.journalParent ?? ""}
              options={pageOptions("A “Journal” page, made when needed")}
              onchange={(v) => set({ type: "journalParent", value: page(v) })}
            />
          </div>
          <div class="set-row">
            <div class="set-row-text">
              <label class="set-label" for="journal-template">New entries start from</label>
              <span class="set-desc">
                A copy of a page you pick, with its <code>{"{{date}}"}</code>, <code>{"{{title}}"}</code> and
                <code>{"{{weekday}}"}</code> filled in.
              </span>
            </div>
            <Select
              id="journal-template"
              value={settings.journalTemplate ?? ""}
              options={templateOptions}
              onchange={(v) => set({ type: "journalTemplate", value: page(v) })}
            />
          </div>
          <div class="set-row">
            <div class="set-row-text">
              <label class="set-label" for="ask-journal">Ask your notes reads the journal</label>
              <span class="set-desc">
                Lets answers use journal entries. For this account, on every device.
              </span>
            </div>
            <Switch id="ask-journal" checked={settings.askJournal} onchange={(on) => set({ type: "askJournal", value: on })} />
          </div>
          <div class="set-row">
            <div class="set-row-text">
              <label class="set-label" for="week-start">Weeks start on</label>
              <span class="set-desc">For the journal's calendar.</span>
            </div>
            <Select
              id="week-start"
              value={settings.weekStart}
              options={WEEKDAYS.map((d, i) => ({ value: i, label: d }))}
              onchange={(v) => set({ type: "weekStart", value: v })}
            />
          </div>
        </div>
      {:else if current === "markdown" && bridge}
        <div class="set-group">
          <p class="set-desc">
            Every page as a Markdown file in a folder on this computer, kept in step both ways: edit the files in any
            editor, or let an agent work on them, and the changes come back. Deleting a file moves its page to the
            trash. The files are plain text, outside Drusite's encryption, and only on this computer.
          </p>
          <div class="set-row">
            <div class="set-row-text">
              <span class="set-label">Folder</span>
              {#if bridge.folder}
                <code class="set-path">{bridge.folder}</code>
              {:else}
                <span class="set-desc">Off</span>
              {/if}
            </div>
            <div class="button-row">
              {#if bridge.folder}
                <button class="btn" disabled={busy} onclick={chooseFolder}>Change folder…</button>
                <button class="btn" disabled={busy} onclick={() => setFolder(null)}>Turn off</button>
              {:else}
                <button class="btn primary" disabled={busy} onclick={chooseFolder}>Choose a folder…</button>
              {/if}
            </div>
          </div>
          {#if bridge.folder && bridge.last}
            <p class="hint" class:field-error={!!bridge.last.error}>{summary(bridge.last)}</p>
          {/if}
        </div>
      {:else if current === "import"}
        <NotionImport {client} {onerror} onopen={(id) => (onclose(), onopen(id))} />
        {#if bridge}
        <div class="set-group">
          <h3 class="set-title">Markdown</h3>
          <p class="set-desc">
            Copies a folder of Markdown notes, such as an Obsidian vault, into new pages: folders become pages,
            wikilinks become links, embedded files become attachments, and daily notes become journal entries. The
            folder isn't changed.
          </p>
          <div class="set-row">
            <div class="set-row-text">
              <span class="set-label">Import a folder</span>
              {#if imported}<span class="set-desc" role="status">{imported}</span>{/if}
            </div>
            <button class="btn" disabled={busy} onclick={importNotes}>
              <Icon name="folder-input" size={15} />Import a folder…
            </button>
          </div>
        </div>
        {/if}
      {:else if current === "ai"}
        <Ai {client} {onerror} />
      {:else if current === "agents"}
        <Agents {client} {server} {onerror} />
      {/if}
    </div>
  </section>
</Modal>
