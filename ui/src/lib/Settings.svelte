<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "../core/client";
  import Agents from "./Agents.svelte";
  import Ai from "./Ai.svelte";
  import type { BridgeRound, BridgeStatus, PageId, PageInfo, Setting, Settings } from "../core/types";
  import { WEEKDAYS } from "../journal/dates";

  let {
    client,
    settings,
    pages,
    server = null,
    onclose,
    onerror,
    onopen,
  }: {
    client: CoreClient;
    settings: Settings;
    pages: PageInfo[];
    /** The server this device syncs with, if signed in. */
    server?: string | null;
    onclose: () => void;
    onerror: (e: unknown) => void;
    /** Opens a page (the one an import made). */
    onopen: (id: PageId) => void;
  } = $props();

  const candidates = $derived(pages.filter((p) => p.kind === "page"));

  async function set(setting: Setting) {
    try {
      await client.setSetting(setting);
    } catch (e) {
      onerror(e);
    }
  }

  const page = (v: string): PageId | null => (v === "" ? null : (v as PageId));

  // The Markdown folder: desktops only, and only on this device.
  let bridge = $state<BridgeStatus | null>(null);
  let busy = $state(false);
  onMount(() => {
    const stop = client.subscribe((event) => {
      if (event.type === "bridgeChanged") bridge = event.status;
    });
    client.bridgeStatus().then((s) => (bridge = s), onerror);
    return stop;
  });

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

  async function chooseFolder() {
    const folder = await client.pickFolder();
    if (folder) await setFolder(folder);
  }

  let imported = $state<string | null>(null);
  async function importNotes() {
    const folder = await client.pickFolder();
    if (!folder) return;
    busy = true;
    imported = "Importing…";
    try {
      const r = await client.importFolder(folder);
      const missing = r.unresolved.length ? ` ${r.unresolved.length} links or embeds pointed at nothing.` : "";
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

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="sheet-backdrop" onclick={onclose}>
  <div class="sheet" role="dialog" aria-modal="true" aria-labelledby="settings-title" tabindex="-1" onclick={(e) => e.stopPropagation()}>
    <div class="sheet-head">
      <h2 id="settings-title">Settings</h2>
      <button class="sheet-close" aria-label="Close" onclick={onclose}>×</button>
    </div>
    <p class="hint">These sync with your account, so every device you sign in on has them.</p>

    <label for="journal-parent">Journal entries go under</label>
    <select id="journal-parent" value={settings.journalParent ?? ""} onchange={(e) => set({ type: "journalParent", value: page(e.currentTarget.value) })}>
      <option value="">A “Journal” page, made when needed</option>
      {#each candidates as p (p.id)}<option value={p.id}>{p.title || "Untitled"}</option>{/each}
    </select>

    <label for="journal-template">New entries start from</label>
    <select id="journal-template" value={settings.journalTemplate ?? ""} onchange={(e) => set({ type: "journalTemplate", value: page(e.currentTarget.value) })}>
      <option value="">An empty page</option>
      {#each candidates as p (p.id)}<option value={p.id}>{p.title || "Untitled"}</option>{/each}
    </select>
    <p class="hint">A template's <code>{"{{date}}"}</code>, <code>{"{{title}}"}</code> and <code>{"{{weekday}}"}</code> are filled in.</p>

    <label for="week-start">Weeks start on</label>
    <select id="week-start" value={settings.weekStart} onchange={(e) => set({ type: "weekStart", value: Number(e.currentTarget.value) })}>
      {#each WEEKDAYS as d, i (d)}<option value={i}>{d}</option>{/each}
    </select>

    {#if bridge}
      <h3 class="settings-section">Markdown folder</h3>
      <p class="hint">
        Every page as a Markdown file in a folder on this computer, kept in step both ways: edit the files in any
        editor, or let an agent work on them, and the changes come back. Deleting a file moves its page to the trash.
        The files are plain text, outside Drusite's encryption, and only on this computer.
      </p>
      {#if bridge.folder}
        <p class="bridge-folder"><code>{bridge.folder}</code></p>
        {#if bridge.last}<p class="hint" class:bridge-error={!!bridge.last.error}>{summary(bridge.last)}</p>{/if}
        <button disabled={busy} onclick={chooseFolder}>Change folder…</button>
        <button disabled={busy} onclick={() => setFolder(null)}>Turn off</button>
      {:else}
        <button disabled={busy} onclick={chooseFolder}>Choose a folder…</button>
      {/if}

      <h3 class="settings-section">Import</h3>
      <p class="hint">
        Copies a folder of Markdown notes, such as an Obsidian vault, into new pages: folders become pages, wikilinks
        become links, embedded files become attachments, and daily notes become journal entries. The folder isn't
        changed.
      </p>
      <button disabled={busy} onclick={importNotes}>Import a folder…</button>
      {#if imported}<p class="hint">{imported}</p>{/if}
    {/if}

    <Ai {client} {onerror} />

    <Agents {client} {server} {onerror} />
  </div>
</div>
