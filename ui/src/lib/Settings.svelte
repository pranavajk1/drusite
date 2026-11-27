<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { CoreClient } from "../core/client";
  import type { PageId, PageInfo, Setting, Settings } from "../core/types";
  import { WEEKDAYS } from "../journal/dates";

  let {
    client,
    settings,
    pages,
    onclose,
    onerror,
  }: {
    client: CoreClient;
    settings: Settings;
    pages: PageInfo[];
    onclose: () => void;
    onerror: (e: unknown) => void;
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
  </div>
</div>
