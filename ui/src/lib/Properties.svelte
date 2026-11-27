<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { CoreClient } from "../core/client";
  import type { PageInfo, PropertyValue } from "../core/types";
  import { dateOf, dayTitle } from "../journal/dates";
  import { DATE, MOOD, moodOf } from "../journal/mood";
  import type { IconName } from "./icons/icons";
  import MoodPicker from "./MoodPicker.svelte";
  import Icon from "./ui/Icon.svelte";
  import Select from "./ui/Select.svelte";

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

  const TYPES: { value: PropertyValue["type"]; label: string }[] = [
    { value: "text", label: "Text" },
    { value: "number", label: "Number" },
    { value: "checkbox", label: "Checkbox" },
    { value: "date", label: "Date" },
  ];
  const ICON: Record<PropertyValue["type"], IconName> = {
    text: "text-quote",
    number: "list-ordered",
    checkbox: "square-check",
    date: "calendar-days",
  };

  let nameInput = $state<HTMLInputElement | null>(null);
  $effect(() => {
    if (adding) nameInput?.focus();
  });

  function formKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      adding = false;
    }
  }

  const entryDate = $derived.by(() => {
    const v = page.properties.find((p) => p.name === DATE)?.value;
    return v?.type === "date" ? v.value : null;
  });
</script>

{#if journal}
  <div class="journal-head">
    {#if entryDate && page.title !== dayTitle(entryDate)}
      <span class="journal-date">{dateOf(entryDate).toLocaleDateString(undefined, { dateStyle: "full" })}</span>
    {/if}
    <span class="journal-mood-label">Mood</span>
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
        <dt><Icon name={ICON[p.value.type]} size={15} /><span class="property-name">{p.name}</span></dt>
        <dd>
          {#if p.value.type === "checkbox"}
            <input
              class="checkbox"
              type="checkbox"
              checked={p.value.value}
              aria-label={p.name}
              onchange={(e) => set(p.name, { type: "checkbox", value: e.currentTarget.checked })}
            />
          {:else}
            <span class="property-value">{show(p.value)}</span>
          {/if}
          <button class="icon-btn property-remove" aria-label="Remove {p.name}" title="Remove" onclick={() => set(p.name, null)}>
            <Icon name="x" size={14} />
          </button>
        </dd>
      </div>
    {/each}
  </dl>
{/if}

{#if adding}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <form class="property-form" aria-label="Add a property" onsubmit={add} onkeydown={formKey}>
    <input bind:this={nameInput} class="input" placeholder="Property name" aria-label="Property name" bind:value={name} />
    <Select
      label="Property type"
      boxed
      value={type}
      options={TYPES}
      onchange={(t) => {
        type = t;
        raw = t === "checkbox" ? "true" : "";
      }}
    />
    {#if type === "checkbox"}
      <Select
        label="Property value"
        boxed
        value={raw}
        options={[
          { value: "true", label: "Yes" },
          { value: "false", label: "No" },
        ]}
        onchange={(v) => (raw = v)}
      />
    {:else}
      <input
        class="input"
        type={type === "date" ? "date" : type === "number" ? "number" : "text"}
        placeholder="Value"
        aria-label="Property value"
        bind:value={raw}
      />
    {/if}
    <div class="button-row">
      <button class="btn primary small" type="submit">Add</button>
      <button class="btn ghost small" type="button" onclick={() => (adding = false)}>Cancel</button>
    </div>
  </form>
{:else}
  <button class="property-add" onclick={() => (adding = true)}><Icon name="plus" size={15} />Add a property</button>
{/if}
