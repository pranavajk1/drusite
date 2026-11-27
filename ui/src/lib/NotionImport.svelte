<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Importing from Notion, in Settings › Import: an export (.zip) offline, or straight from
  Notion's API with an integration's token, which keeps formulas, rollups and relations as they
  are. Both say what came in and what couldn't be carried over.
-->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "../core/client";
  import type { NotionReport, PageId } from "../core/types";
  import Icon from "./ui/Icon.svelte";

  let { client, onerror, onopen }: { client: CoreClient; onerror: (e: unknown) => void; onopen: (id: PageId) => void } = $props();

  let busy = $state(false);
  let status = $state<string | null>(null);
  let report = $state<NotionReport | null>(null);
  let token = $state("");
  let saved = $state(false);
  let fileInput: HTMLInputElement;

  onMount(() => {
    client.notionToken().then((s) => (saved = s), () => (saved = false));
    return client.subscribe((e) => {
      if (e.type === "notionProgress" && busy) status = e.total ? `Reading from Notion: ${e.done} of ${e.total}${e.what ? ` (${e.what})` : ""}…` : `${e.what}…`;
    });
  });

  function summary(r: NotionReport): string {
    const n = (k: number, one: string, many: string) => `${k} ${k === 1 ? one : many}`;
    return `Imported ${n(r.pages, "page", "pages")}, ${n(r.databases, "database", "databases")} with ${n(r.rows, "row", "rows")}, and ${n(r.attachments, "file", "files")}.`;
  }

  async function run(work: () => Promise<NotionReport>) {
    busy = true;
    report = null;
    status = "Importing…";
    try {
      report = await work();
      status = summary(report);
    } catch (e) {
      status = null;
      onerror(e);
    } finally {
      busy = false;
    }
  }

  async function importExport() {
    if (client.canPickFile) {
      const path = await client.pickFile("Choose a Notion export (.zip)", ["zip"]);
      if (path) await run(() => client.importNotion(path));
    } else {
      fileInput.click();
    }
  }

  function fileChosen() {
    const file = fileInput.files?.[0];
    fileInput.value = "";
    if (!file) return;
    void run(async () => {
      const bytes = new Uint8Array(await file.arrayBuffer());
      let binary = "";
      for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
      return client.importNotionData(file.name, btoa(binary));
    });
  }

  async function importApi() {
    const t = token.trim() || null;
    await run(() => client.importNotionApi(t));
    token = "";
    saved = await client.notionToken().catch(() => saved);
  }

  async function forget() {
    try {
      await client.forgetNotionToken();
      saved = false;
    } catch (e) {
      onerror(e);
    }
  }
</script>

<div class="set-group">
  <h3 class="set-title">Notion</h3>
  <div class="set-row">
    <div class="set-row-text">
      <span class="set-label">Import a Notion export</span>
      <span class="set-desc">In Notion: ⋯ › Export › Markdown &amp; CSV, with subpages. Works offline. Notion's export has no views, formulas or rollups.</span>
    </div>
    <button class="btn" disabled={busy} onclick={importExport}><Icon name="file-archive" size={15} />Choose .zip…</button>
    <input bind:this={fileInput} class="file-picker" type="file" accept=".zip,application/zip" onchange={fileChosen} />
  </div>
  <div class="set-row notion-api">
    <div class="set-row-text">
      <span class="set-label">Import from Notion's API</span>
      <span class="set-desc">
        Keeps property types, formulas, rollups and relations. Make an internal integration at notion.so/my-integrations, share your pages
        with it, and paste its token. The token stays on this device{saved ? "; one is kept now" : ""}.
      </span>
      <div class="notion-token">
        <input
          class="input"
          type="password"
          autocomplete="off"
          spellcheck="false"
          aria-label="Notion integration token"
          placeholder={saved ? "Using the kept token" : "ntn_… or secret_…"}
          bind:value={token}
        />
        <button class="btn primary" disabled={busy || (!token.trim() && !saved)} onclick={importApi}><Icon name="upload" size={15} />Import</button>
        {#if saved}<button class="btn ghost" disabled={busy} onclick={forget}>Forget token</button>{/if}
      </div>
    </div>
  </div>
  {#if status}<p class="set-desc" role="status">{status}</p>{/if}
  {#if report}
    {#if report.page}<button class="btn small" onclick={() => onopen(report!.page!)}>Open what came in</button>{/if}
    {#if report.unmapped.length}
      <details class="notion-unmapped">
        <summary>{report.unmapped.length} {report.unmapped.length === 1 ? "thing" : "things"} couldn't be carried over as they were</summary>
        <ul>
          {#each report.unmapped as u, i (i)}<li>{u}</li>{/each}
        </ul>
      </details>
    {/if}
  {/if}
</div>
