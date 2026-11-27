<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Adding or changing a property: its name and type, and what the type needs (options and their
  colours, a number format, the database a relation points at and whether it's two-way, a
  rollup's relation, property and function, a formula, checked as it's typed).
-->
<script lang="ts">
  import type { CoreClient } from "../../core/client";
  import type { FormulaCheck, PageInfo, PropertyDef, Schema, SelectOption } from "../../core/types";
  import Icon from "../ui/Icon.svelte";
  import Popover from "../ui/Popover.svelte";
  import Select from "../ui/Select.svelte";
  import { COLORS, KINDS, NUMBER_FORMATS, ROLLUPS, type KindName, kindIcon, newKind, shortId } from "./util";

  let {
    client,
    schema,
    prop = null,
    anchor,
    databases,
    onclose,
    onerror,
    onsaved,
  }: {
    client: CoreClient;
    schema: Schema;
    /** The property to change; none to add one. */
    prop?: PropertyDef | null;
    anchor: HTMLElement;
    /** Every database, for a relation's target. */
    databases: PageInfo[];
    onclose: () => void;
    onerror: (e: unknown) => void;
    onsaved?: (schema: Schema) => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  const start: PropertyDef = prop
    ? structuredClone($state.snapshot(prop))
    : {
        id: shortId(),
        name: "",
        kind: { type: "text" },
        options: [],
        position: Math.max(0, ...schema.properties.map((p) => p.position)) + 1,
        description: null,
      };
  let def = $state<PropertyDef>(start);
  // svelte-ignore state_referenced_locally
  const isNew = !prop;
  let twoWay = $state(true);
  // svelte-ignore state_referenced_locally
  let twinName = $state(schema.title || "Related");
  let check = $state<FormulaCheck | null>(null);
  let confirmDelete = $state(false);

  function setType(type: KindName) {
    def.kind = newKind(type, schema);
    if ((type === "select" || type === "multiSelect") && def.options.length === 0) def.options = [];
  }

  const relations = $derived(schema.properties.filter((p) => p.kind.type === "relation" && p.id !== def.id));
  let targetSchema = $state<Schema | null>(null);
  $effect(() => {
    const k = def.kind;
    const rel = k.type === "rollup" ? schema.properties.find((p) => p.id === k.relation) : null;
    const db = rel?.kind.type === "relation" ? rel.kind.database : null;
    if (!db) {
      targetSchema = null;
      return;
    }
    void client.database(db).then((s) => (targetSchema = s)).catch(onerror);
  });

  let checking: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const k = def.kind;
    if (k.type !== "formula") return;
    const expression = k.expression;
    clearTimeout(checking);
    checking = setTimeout(async () => {
      if (!expression.trim()) {
        check = null;
        return;
      }
      try {
        check = await client.checkFormula(schema.database, expression);
      } catch (e) {
        onerror(e);
      }
    }, 200);
  });

  function typeName(t: FormulaCheck["result"]): string {
    if (!t) return "";
    if (t.type === "list") return `list of ${typeName(t.item)}`;
    return { any: "a value", number: "a number", text: "text", bool: "a checkbox", date: "a date", person: "a person", page: "a page" }[t.type] ?? t.type;
  }

  function addOption() {
    const o: SelectOption = {
      id: shortId(),
      name: `Option ${def.options.length + 1}`,
      color: COLORS[(def.options.length + 1) % COLORS.length]!,
      group: def.kind.type === "status" ? "toDo" : null,
      position: def.options.length,
    };
    def.options = [...def.options, o];
  }

  function insertProp(name: string) {
    if (def.kind.type !== "formula") return;
    def.kind = { ...def.kind, expression: `${def.kind.expression}prop("${name}")` };
  }

  async function save() {
    if (!def.name.trim()) def.name = isNew ? (KINDS.find((k) => k.type === def.kind.type)?.label ?? "Property") : (prop?.name ?? "Property");
    if (def.kind.type === "formula" && check?.error) return;
    try {
      const twin = isNew && def.kind.type === "relation" && twoWay ? twinName : null;
      const s = await client.putProperty(schema.database, $state.snapshot(def) as PropertyDef, twin);
      onsaved?.(s);
      onclose();
    } catch (e) {
      onerror(e);
    }
  }

  async function remove() {
    if (!prop) return;
    try {
      const s = await client.deleteProperty(schema.database, prop.id);
      onsaved?.(s);
      onclose();
    } catch (e) {
      onerror(e);
    }
  }
</script>

<Popover {anchor} label={isNew ? "New property" : `Edit ${prop?.name}`} {onclose} width={def.kind.type === "formula" ? 380 : 300}>
  <form
    class="prop-form"
    onsubmit={(e) => {
      e.preventDefault();
      void save();
    }}
  >
    <div class="prop-form-head">
      <Icon name={kindIcon(def.kind)} />
      <input class="input" placeholder="Property name" aria-label="Property name" bind:value={def.name} />
    </div>
    {#if def.kind.type !== "title"}
      <label class="prop-form-row">
        <span>Type</span>
        <Select
          label="Type"
          value={def.kind.type}
          options={KINDS.map((k) => ({ value: k.type, label: k.label }))}
          onchange={(t) => setType(t as KindName)}
        />
      </label>
    {/if}

    {#if def.kind.type === "number"}
      {@const k = def.kind}
      <label class="prop-form-row">
        <span>Format</span>
        <Select label="Number format" value={k.format} options={NUMBER_FORMATS} onchange={(f) => (def.kind = { ...k, format: f })} />
      </label>
    {:else if def.kind.type === "select" || def.kind.type === "multiSelect" || def.kind.type === "status"}
      <div class="prop-form-label">Options</div>
      {#each def.options as o, i (o.id)}
        <div class="option-edit">
          <input class="input" aria-label="Option name" bind:value={def.options[i]!.name} />
          <Select label="Colour" value={o.color} options={COLORS.map((c) => ({ value: c, label: c[0]!.toUpperCase() + c.slice(1) }))} onchange={(c) => (def.options[i]!.color = c)} />
          {#if def.kind.type === "status"}
            <Select
              label="Group"
              value={o.group ?? "toDo"}
              options={[
                { value: "toDo", label: "To-do" },
                { value: "inProgress", label: "In progress" },
                { value: "complete", label: "Complete" },
              ]}
              onchange={(g) => (def.options[i]!.group = g)}
            />
          {/if}
          <button type="button" class="icon-btn" aria-label="Remove {o.name}" onclick={() => (def.options = def.options.filter((x) => x.id !== o.id))}>
            <Icon name="x" size={14} />
          </button>
        </div>
      {/each}
      <button type="button" class="btn small ghost" onclick={addOption}><Icon name="plus" size={14} /> Add an option</button>
    {:else if def.kind.type === "relation"}
      {@const k = def.kind}
      <label class="prop-form-row">
        <span>Related to</span>
        <Select
          label="Related database"
          value={k.database}
          options={databases.map((d) => ({ value: d.id, label: d.title || "Untitled" }))}
          onchange={(d) => (def.kind = { ...k, database: d })}
          disabled={!isNew}
        />
      </label>
      <label class="prop-form-row">
        <span>Limit</span>
        <Select
          label="Limit"
          value={k.single ? "one" : "any"}
          options={[
            { value: "any", label: "No limit" },
            { value: "one", label: "1 page" },
          ]}
          onchange={(v) => (def.kind = { ...k, single: v === "one" })}
        />
      </label>
      {#if isNew}
        <label class="date-toggle"><input type="checkbox" class="checkbox" bind:checked={twoWay} /> Show on the other database too</label>
        {#if twoWay}<input class="input" aria-label="Its name there" placeholder="Its name there" bind:value={twinName} />{/if}
      {/if}
    {:else if def.kind.type === "rollup"}
      {@const k = def.kind}
      {#if relations.length === 0}
        <p class="prop-form-note">A rollup sums up a relation. Add a relation first.</p>
      {:else}
        <label class="prop-form-row">
          <span>Relation</span>
          <Select label="Relation" value={k.relation} options={relations.map((r) => ({ value: r.id, label: r.name }))} onchange={(r) => (def.kind = { ...k, relation: r })} />
        </label>
        <label class="prop-form-row">
          <span>Property</span>
          <Select
            label="Property"
            value={k.target}
            options={(targetSchema?.properties ?? []).map((p) => ({ value: p.id, label: p.name }))}
            onchange={(t) => (def.kind = { ...k, target: t })}
          />
        </label>
        <label class="prop-form-row">
          <span>Calculate</span>
          <Select label="Calculate" value={k.function} options={ROLLUPS} onchange={(f) => (def.kind = { ...k, function: f })} />
        </label>
      {/if}
    {:else if def.kind.type === "formula"}
      {@const k = def.kind}
      <textarea
        class="input formula-input"
        rows="4"
        spellcheck="false"
        aria-label="Formula"
        placeholder={'prop("Points") * 2'}
        value={k.expression}
        oninput={(e) => (def.kind = { ...k, expression: (e.currentTarget as HTMLTextAreaElement).value })}
      ></textarea>
      {#if check?.error}
        <p class="formula-check error" role="alert"><Icon name="circle-alert" size={14} /> {check.error.message}</p>
      {:else if check?.result}
        <p class="formula-check ok"><Icon name="circle-check" size={14} /> Gives {typeName(check.result)}</p>
      {/if}
      <div class="formula-props">
        {#each schema.properties.filter((p) => p.id !== def.id) as p (p.id)}
          <button type="button" class="chip tag-default" onclick={() => insertProp(p.name)}>{p.name}</button>
        {/each}
      </div>
    {:else if def.kind.type === "uniqueId"}
      {@const k = def.kind}
      <input
        class="input"
        aria-label="Prefix"
        placeholder="Prefix, e.g. TASK"
        value={k.prefix ?? ""}
        oninput={(e) => (def.kind = { ...k, prefix: (e.currentTarget as HTMLInputElement).value.trim() || null })}
      />
    {/if}

    <div class="form-actions">
      {#if !isNew && def.kind.type !== "title"}
        {#if confirmDelete}
          <button type="button" class="btn small danger solid" onclick={remove}>Delete property</button>
        {:else}
          <button type="button" class="btn small ghost danger" onclick={() => (confirmDelete = true)}>Delete</button>
        {/if}
      {/if}
      <span class="spacer"></span>
      <button type="button" class="btn small ghost" onclick={onclose}>Cancel</button>
      <button type="submit" class="btn small primary" disabled={def.kind.type === "formula" && !!check?.error}>{isNew ? "Add" : "Save"}</button>
    </div>
  </form>
</Popover>
