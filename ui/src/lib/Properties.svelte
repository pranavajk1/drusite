<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { CoreClient } from "../core/client";
  import type { PageInfo, PropertyValue } from "../core/types";
  import { dateOf, dayTitle } from "../journal/dates";
  import { DATE, MOOD, moodOf } from "../journal/mood";
  import MoodPicker from "./MoodPicker.svelte";

  let { client, page, onerror }: { client: CoreClient; page: PageInfo; onerror: (e: unknown) => void } = $props();

  const journal = $derived(page.kind === "journal");
  // A journal entry shows its date and mood in their own row; the rest are listed.
  const listed = $derived(page.properties.filter((p) => !(journal && (p.name === DATE || p.name === MOOD))));

  let adding = $state(false);
  let name = $state("");
  let type = $state<PropertyValue["type"]>("text");
  let raw = $state("");

  async function set(n: string, value: PropertyValue | null) {
    try {
      await client.setProperty(page.id, n, value);
    } catch (e) {
      onerror(e);
    }
  }

  function show(v: PropertyValue): string {
    switch (v.type) {
      case "text":
        return v.value;
      case "number":
        return v.value === null ? "" : String(v.value);
      case "checkbox":
        return v.value ? "Yes" : "No";
      case "date":
        return dateOf(v.value).toLocaleDateString(undefined, { dateStyle: "medium" });
    }
  }

  function parse(): PropertyValue | null {
    switch (type) {
      case "text":
        return { type, value: raw };
      case "number": {
        const n = Number(raw);
        return raw.trim() === "" || !Number.isFinite(n) ? null : { type, value: n };
      }
      case "checkbox":
        return { type, value: raw === "true" };
      case "date":
        return /^\d{4}-\d{2}-\d{2}$/.test(raw) ? { type, value: raw } : null;
    }
  }

  async function add(e: SubmitEvent) {
    e.preventDefault();
    const value = parse();
    if (name.trim() === "" || value === null) return;
    await set(name.trim(), value);
    adding = false;
    name = "";
    raw = "";
  }

  const entryDate = $derived.by(() => {
    const v = page.properties.find((p) => p.name === DATE)?.value;
    return v?.type === "date" ? v.value : null;
  });
</script>

{#if journal}
  <div class="journal-head">
    <span class="journal-mood-label">Mood</span>
    {#if entryDate && page.title !== dayTitle(entryDate)}
      <span class="journal-date">{dateOf(entryDate).toLocaleDateString(undefined, { dateStyle: "full" })}</span>
    {/if}
    <MoodPicker
      score={moodOf(page)?.score ?? null}
      onpick={(s) => set(MOOD, s === null ? null : { type: "number", value: s })}
    />
  </div>
{/if}

{#if listed.length > 0 || adding}
  <dl class="properties">
    {#each listed as p (p.name)}
      <div class="property">
        <dt>{p.name}</dt>
        <dd>
          {#if p.value.type === "checkbox"}
            <input
              type="checkbox"
              checked={p.value.value}
              aria-label={p.name}
              onchange={(e) => set(p.name, { type: "checkbox", value: e.currentTarget.checked })}
            />
          {:else}
            {show(p.value)}
          {/if}
          <button class="property-remove" aria-label="Remove {p.name}" onclick={() => set(p.name, null)}>×</button>
        </dd>
      </div>
    {/each}
  </dl>
{/if}

{#if adding}
  <form class="property-form" onsubmit={add}>
    <input placeholder="Name" aria-label="Property name" bind:value={name} />
    <select aria-label="Property type" bind:value={type} onchange={() => (raw = type === "checkbox" ? "true" : "")}>
      <option value="text">Text</option>
      <option value="number">Number</option>
      <option value="checkbox">Checkbox</option>
      <option value="date">Date</option>
    </select>
    {#if type === "checkbox"}
      <select aria-label="Property value" bind:value={raw}>
        <option value="true">Yes</option>
        <option value="false">No</option>
      </select>
    {:else}
      <input
        type={type === "date" ? "date" : type === "number" ? "number" : "text"}
        aria-label="Property value"
        bind:value={raw}
      />
    {/if}
    <button class="primary" type="submit">Add</button>
    <button type="button" onclick={() => (adding = false)}>Cancel</button>
  </form>
{:else}
  <button class="property-add" onclick={() => (adding = true)}>+ Add a property</button>
{/if}
