<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Editing one cell, in a popover by the cell: text, numbers and links in a field; options picked
  from a list (or made by typing a new name); dates with an optional time and end; people; the
  rows a relation links to; files by link.
-->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "../../core/client";
  import type { Cell, DateValue, PageId, Person, PropertyDef, RowView, Schema, SelectOption, Value } from "../../core/types";
  import Icon from "../ui/Icon.svelte";
  import Popover from "../ui/Popover.svelte";
  import { COLORS, cellOf, editText, parseTyped, shortId } from "./util";

  let {
    client,
    schema,
    prop,
    row,
    anchor,
    people = [],
    onclose,
    onerror,
    onopen,
  }: {
    client: CoreClient;
    schema: Schema;
    prop: PropertyDef;
    row: RowView;
    anchor: HTMLElement;
    /** People seen in this database, to pick from. */
    people?: Person[];
    onclose: () => void;
    onerror: (e: unknown) => void;
    onopen?: (page: PageId) => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  const cell: Cell | undefined = cellOf(row, prop.id);
  // svelte-ignore state_referenced_locally
  const kind = prop.kind;
  // svelte-ignore state_referenced_locally
  let text = $state(kind.type === "title" ? row.title : editText(cell));
  let query = $state("");
  let saving = false;

  async function set(value: Value | null) {
    try {
      await client.setCell(row.id, prop.id, value);
    } catch (e) {
      onerror(e);
    }
  }

  async function commitText() {
    if (saving) return;
    saving = true;
    if (kind.type === "title") await set({ type: "text", value: text });
    else await set(parseTyped(kind, text));
    onclose();
  }

  function textKey(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void commitText();
    }
  }

  // ---- Options ----
  const chosen = $derived.by(() => {
    const v = cell?.value;
    if (v?.type === "multiSelect") return v.value;
    if (v?.type === "select" || v?.type === "status") return [v.value];
    return [] as string[];
  });
  let picked = $state<string[]>([]);
  $effect(() => {
    picked = chosen;
  });
  const shown = $derived(prop.options.filter((o) => o.name.toLowerCase().includes(query.trim().toLowerCase())));
  const exact = $derived(prop.options.some((o) => o.name.toLowerCase() === query.trim().toLowerCase()));

  function optionValue(ids: string[]): Value | null {
    if (kind.type === "multiSelect") return ids.length ? { type: "multiSelect", value: ids } : null;
    const id = ids[0];
    if (!id) return null;
    return kind.type === "status" ? { type: "status", value: id } : { type: "select", value: id };
  }

  async function pick(o: SelectOption) {
    if (kind.type === "multiSelect") {
      picked = picked.includes(o.id) ? picked.filter((x) => x !== o.id) : [...picked, o.id];
      await set(optionValue(picked));
    } else {
      await set(picked[0] === o.id ? null : optionValue([o.id]));
      onclose();
    }
  }

  async function createOption() {
    const name = query.trim();
    if (!name) return;
    const o: SelectOption = {
      id: shortId(),
      name,
      color: COLORS[(prop.options.length + 1) % COLORS.length]!,
      group: kind.type === "status" ? "toDo" : null,
      position: prop.options.length,
    };
    try {
      await client.putProperty(schema.database, { ...prop, options: [...prop.options, o] });
      query = "";
      await pick(o);
    } catch (e) {
      onerror(e);
    }
  }

  function optionKey(e: KeyboardEvent) {
    if (e.key !== "Enter") return;
    e.preventDefault();
    const first = shown[0];
    if (query.trim() && !exact) void createOption();
    else if (first) void pick(first);
  }

  // ---- Dates ----
  // svelte-ignore state_referenced_locally
  const date: DateValue | null = cell?.value?.type === "date" ? cell.value.value : null;
  let startDay = $state(date?.start.slice(0, 10) ?? "");
  let startTime = $state(date?.start.slice(11, 16) ?? "");
  let endDay = $state(date?.end?.slice(0, 10) ?? "");
  let withEnd = $state(!!date?.end);
  let withTime = $state(!!date?.start.includes("T"));

  async function saveDate() {
    if (!startDay) return set(null);
    const at = (d: string) => (withTime && startTime ? `${d}T${startTime}` : d);
    await set({ type: "date", value: { start: at(startDay), end: withEnd && endDay ? at(endDay) : null } });
  }

  // ---- People ----
  let personName = $state("");
  const current = $derived(cell?.value?.type === "people" ? cell.value.value : []);
  const candidates = $derived(
    people.filter((p) => !current.some((c) => c.id === p.id) && p.name.toLowerCase().includes(personName.trim().toLowerCase())),
  );
  async function addPerson(p: Person) {
    personName = "";
    await set({ type: "people", value: [...current, p] });
  }
  async function removePerson(p: Person) {
    const rest = current.filter((c) => c.id !== p.id);
    await set(rest.length ? { type: "people", value: rest } : null);
  }
  function newPerson() {
    const name = personName.trim();
    if (!name) return;
    const email = name.includes("@") ? name : null;
    void addPerson({ id: email ?? name, name: email ? email.split("@")[0]! : name, email });
  }

  // ---- Relations ----
  let targets = $state<{ id: PageId; title: string }[]>([]);
  const linked = $derived(cell?.value?.type === "relation" ? cell.value.value : []);
  onMount(() => {
    if (kind.type !== "relation") return;
    void client
      .queryView(kind.database, null)
      .then((r) => {
        targets = r.groups.flatMap((g) => g.rows).map((x) => ({ id: x.id, title: x.title }));
        // A row can sit in several groups (multi-select): list it once.
        targets = targets.filter((t, i) => targets.findIndex((x) => x.id === t.id) === i);
      })
      .catch(onerror);
  });
  const shownTargets = $derived(targets.filter((t) => (t.title || "Untitled").toLowerCase().includes(query.trim().toLowerCase())));
  async function toggleLink(id: PageId) {
    const next = linked.includes(id) ? linked.filter((x) => x !== id) : kind.type === "relation" && kind.single ? [id] : [...linked, id];
    await set(next.length ? { type: "relation", value: next } : null);
  }
  async function newLinkedRow() {
    if (kind.type !== "relation") return;
    try {
      const page = await client.createRow(kind.database, query.trim());
      query = "";
      targets = [...targets, { id: page.id, title: page.title }];
      await toggleLink(page.id);
    } catch (e) {
      onerror(e);
    }
  }

  // ---- Files ----
  let fileUrl = $state("");
  const files = $derived(cell?.value?.type === "files" ? cell.value.value : []);
  async function addFile() {
    const url = fileUrl.trim();
    if (!url) return;
    fileUrl = "";
    const name = decodeURIComponent(url.split(/[?#]/)[0]!.split("/").pop() || url);
    await set({ type: "files", value: [...files, { name, url, blob: null }] });
  }
  async function removeFile(i: number) {
    const rest = files.filter((_, j) => j !== i);
    await set(rest.length ? { type: "files", value: rest } : null);
  }
</script>

<Popover {anchor} label="Edit {prop.name}" onclose={kind.type === "date" ? () => void saveDate().then(onclose) : onclose} width={kind.type === "relation" ? 320 : 280}>
  {#if kind.type === "title" || kind.type === "text"}
    <textarea class="input cell-input" rows="3" bind:value={text} onkeydown={textKey} onblur={commitText}></textarea>
  {:else if kind.type === "number" || kind.type === "url" || kind.type === "email" || kind.type === "phone"}
    <input
      class="input cell-input"
      type={kind.type === "number" ? "text" : kind.type === "email" ? "email" : kind.type === "url" ? "url" : "tel"}
      inputmode={kind.type === "number" ? "decimal" : undefined}
      bind:value={text}
      onkeydown={textKey}
      onblur={commitText}
    />
  {:else if kind.type === "select" || kind.type === "multiSelect" || kind.type === "status"}
    <input class="input cell-input" placeholder="Search for an option…" bind:value={query} onkeydown={optionKey} />
    <div class="option-list" role="listbox" aria-label="Options" aria-multiselectable={kind.type === "multiSelect"}>
      {#each shown as o (o.id)}
        <button class="option-row" role="option" aria-selected={picked.includes(o.id)} onclick={() => pick(o)}>
          <span class="chip tag-{o.color}">{o.name}</span>
          {#if kind.type === "status" && o.group}<span class="option-group">{o.group === "toDo" ? "To-do" : o.group === "inProgress" ? "In progress" : "Complete"}</span>{/if}
          {#if picked.includes(o.id)}<Icon name="check" size={14} />{/if}
        </button>
      {/each}
      {#if query.trim() && !exact}
        <button class="option-row create" onclick={createOption}>Create <span class="chip tag-default">{query.trim()}</span></button>
      {/if}
    </div>
  {:else if kind.type === "date"}
    <div class="date-form">
      <label class="date-row"><span>Start</span><input class="input" type="date" bind:value={startDay} /></label>
      {#if withTime}<label class="date-row"><span>Time</span><input class="input" type="time" bind:value={startTime} /></label>{/if}
      {#if withEnd}<label class="date-row"><span>End</span><input class="input" type="date" bind:value={endDay} /></label>{/if}
      <label class="date-toggle"><input type="checkbox" class="checkbox" bind:checked={withEnd} /> End date</label>
      <label class="date-toggle"><input type="checkbox" class="checkbox" bind:checked={withTime} /> Include time</label>
      <div class="form-actions">
        <button class="btn small ghost" onclick={() => ((startDay = ""), void saveDate().then(onclose))}>Clear</button>
        <button class="btn small primary" onclick={() => void saveDate().then(onclose)}>Done</button>
      </div>
    </div>
  {:else if kind.type === "people"}
    <div class="chips wrap picked">
      {#each current as p (p.id)}
        <span class="person"
          ><span class="person-avatar" aria-hidden="true">{(p.name || "?")[0]!.toUpperCase()}</span>{p.name}<button
            class="chip-x"
            aria-label="Remove {p.name}"
            onclick={() => removePerson(p)}><Icon name="x" size={12} /></button
          ></span
        >
      {/each}
    </div>
    <input
      class="input cell-input"
      placeholder="Name or email…"
      bind:value={personName}
      onkeydown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          if (candidates[0]) void addPerson(candidates[0]);
          else newPerson();
        }
      }}
    />
    <div class="option-list">
      {#each candidates as p (p.id)}
        <button class="option-row" onclick={() => addPerson(p)}><span class="person-avatar" aria-hidden="true">{(p.name || "?")[0]!.toUpperCase()}</span>{p.name}</button>
      {/each}
      {#if personName.trim() && !candidates.length}<button class="option-row create" onclick={newPerson}>Add “{personName.trim()}”</button>{/if}
    </div>
  {:else if kind.type === "relation"}
    <input class="input cell-input" placeholder="Search for a page…" bind:value={query} />
    <div class="option-list" role="listbox" aria-label="Pages" aria-multiselectable={!kind.single}>
      {#each shownTargets as t (t.id)}
        <div class="option-row" role="option" aria-selected={linked.includes(t.id)}>
          <button class="option-main" onclick={() => toggleLink(t.id)}><Icon name="file-text" size={14} /><span>{t.title || "Untitled"}</span></button>
          {#if linked.includes(t.id)}<Icon name="check" size={14} />{/if}
          {#if onopen}<button class="icon-btn" aria-label="Open {t.title || 'Untitled'}" onclick={() => (onclose(), onopen?.(t.id))}><Icon name="arrow-up-right" size={14} /></button>{/if}
        </div>
      {/each}
      {#if query.trim()}<button class="option-row create" onclick={newLinkedRow}><Icon name="plus" size={14} />New page “{query.trim()}”</button>{/if}
    </div>
  {:else if kind.type === "files"}
    {#each files as f, i (i)}
      <div class="option-row">
        <Icon name="paperclip" size={14} /><a class="cell-link" href={f.url ?? "#"} target="_blank" rel="noopener noreferrer">{f.name}</a>
        <button class="icon-btn" aria-label="Remove {f.name}" onclick={() => removeFile(i)}><Icon name="x" size={14} /></button>
      </div>
    {/each}
    <input
      class="input cell-input"
      placeholder="Paste a link…"
      bind:value={fileUrl}
      onkeydown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          void addFile();
        }
      }}
    />
  {:else}
    <p class="cell-computed">{cell?.error ?? cell?.text ?? ""}</p>
  {/if}
</Popover>
