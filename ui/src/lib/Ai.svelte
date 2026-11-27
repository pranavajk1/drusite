<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- AI in Settings: the providers on this device, each with Test connection, and which provider
     and model each feature uses. -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "../core/client";
  import Icon from "./ui/Icon.svelte";
  import Select from "./ui/Select.svelte";
  import type { AiCheckStep, AiFeature, AiFeatureChoice, AiKind, AiKindInfo, AiModel, AiProvider } from "../core/types";

  let {
    client,
    onerror,
  }: {
    client: CoreClient;
    onerror: (e: unknown) => void;
  } = $props();

  let available = $state(false);
  let kinds = $state<AiKindInfo[]>([]);
  let providers = $state<AiProvider[]>([]);
  let features = $state<AiFeatureChoice[]>([]);

  async function load() {
    try {
      const a = await client.ai();
      available = a.available;
      kinds = a.kinds;
      providers = a.providers;
      features = a.features;
      // The pickers start from what's saved.
      for (const f of a.features) drafts[f.feature] = { provider: f.choice?.provider ?? "", model: f.choice?.model ?? "" };
      for (const f of a.features) if (f.choice) void loadModels(f.choice.provider);
    } catch (e) {
      onerror(e);
    }
  }

  onMount(() => {
    const stop = client.subscribe((event) => {
      if (event.type === "aiChanged") void load();
    });
    void load();
    return stop;
  });

  const info = (kind: AiKind) => kinds.find((k) => k.kind === kind);
  const label = (kind: AiKind) => info(kind)?.label ?? kind;
  const choice = (f: AiFeature) => features.find((x) => x.feature === f)?.choice ?? null;

  // Adding or changing a provider.
  let editing = $state<{ id: string | null; kind: AiKind; name: string; baseUrl: string; apiKey: string; keyHint: string | null } | null>(null);
  let formError = $state<string | null>(null);
  let busy = $state(false);

  function add() {
    editing = { id: null, kind: "openAiCompatible", name: "", baseUrl: "", apiKey: "", keyHint: null };
    formError = null;
  }

  function edit(p: AiProvider) {
    editing = { id: p.id, kind: p.kind, name: p.name, baseUrl: p.baseUrl ?? "", apiKey: "", keyHint: p.keyHint };
    formError = null;
  }

  const editingInfo = $derived(editing ? info(editing.kind) : undefined);

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!editing) return;
    busy = true;
    formError = null;
    try {
      const saved = await client.saveAiProvider({
        id: editing.id,
        kind: editing.kind,
        name: editing.name,
        baseUrl: editing.baseUrl.trim() || null,
        // Left empty when changing one: keep the saved key.
        apiKey: editing.id && editing.apiKey === "" ? null : editing.apiKey,
      });
      editing = null;
      await load();
      void test(saved);
    } catch (e) {
      formError = (e as { message?: string }).message ?? String(e);
    } finally {
      busy = false;
    }
  }

  async function remove(p: AiProvider) {
    try {
      await client.deleteAiProvider(p.id);
      delete results[p.id];
    } catch (e) {
      onerror(e);
    }
  }

  // Test connection, per provider. It tries the models the features use on it.
  let results = $state<Record<string, { running: boolean; steps: AiCheckStep[] }>>({});

  async function test(p: AiProvider) {
    results[p.id] = { running: true, steps: [] };
    const chat = choice("chat");
    const embeddings = choice("embeddings");
    try {
      const steps = await client.testAiProvider(
        p.id,
        chat?.provider === p.id ? chat.model : null,
        embeddings?.provider === p.id ? embeddings.model : null,
      );
      results[p.id] = { running: false, steps };
    } catch (e) {
      results[p.id] = { running: false, steps: [] };
      onerror(e);
    }
  }

  function seconds(ms: number): string {
    return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;
  }

  // Per feature: a provider, then a model (typed, or picked from the provider's list).
  const FEATURES: { feature: AiFeature; title: string; hint: string }[] = [
    { feature: "chat", title: "Chat", hint: "Answers questions about your notes." },
    { feature: "embeddings", title: "Embeddings", hint: "Finds notes by meaning. Changing the model indexes everything again." },
  ];
  let models = $state<Record<string, AiModel[]>>({});
  let drafts = $state<Record<string, { provider: string; model: string }>>({});

  async function loadModels(provider: string) {
    if (!provider || models[provider]) return;
    try {
      models[provider] = await client.aiModels(provider);
    } catch {
      // Not every endpoint lists its models; the name can still be typed.
      models[provider] = [];
    }
  }

  async function pick(f: AiFeature) {
    const d = drafts[f];
    if (!d) return;
    try {
      if (!d.provider) {
        await client.setAiFeature(f, null);
      } else if (d.model.trim()) {
        await client.setAiFeature(f, { provider: d.provider, model: d.model.trim() });
      }
    } catch (e) {
      onerror(e);
      await load();
    }
  }

  function suggestions(f: AiFeature, provider: string): AiModel[] {
    const list = models[provider] ?? [];
    const wanted = list.filter((m) => m.embeddings === (f === "embeddings"));
    return wanted.length ? wanted : list;
  }

  const forFeature = (f: AiFeature) => providers.filter((p) => f !== "embeddings" || info(p.kind)?.embeds);
</script>

{#if !available}
  <div class="set-group">
    <p class="set-desc">AI isn't available in this build.</p>
  </div>
{:else}
  <div class="set-group">
    <p class="set-desc">
      Use any AI: a vendor's, or your own models through an OpenAI-compatible endpoint (LiteLLM, Ollama, vLLM, LM
      Studio, llama.cpp). Keys stay on this device, in its encrypted store, and don't sync; set them up on each device.
    </p>
  </div>

  <div class="set-group">
    <h3 class="set-title">Providers</h3>
    {#if providers.length}
      <ul class="list ai-providers" aria-label="AI providers">
        {#each providers as p (p.id)}
          {@const r = results[p.id]}
          <li class="ai-provider">
            <div class="list-row">
              <Icon name="plug" class="list-row-icon" />
              <div class="list-row-text">
                <span>{p.name}</span>
                <small>{label(p.kind)}{p.baseUrl ? ` · ${p.baseUrl}` : ""}{p.keyHint ? ` · key ${p.keyHint}` : ""}</small>
              </div>
              <div class="button-row">
                <button class="btn small" disabled={r?.running} onclick={() => test(p)}>
                  {#if r?.running}<Icon name="loader-circle" size={14} class="spin" />Testing…{:else}Test connection{/if}
                </button>
                <button class="btn small ghost" onclick={() => edit(p)}>Edit</button>
                <button class="btn small ghost danger" onclick={() => remove(p)}>Remove</button>
              </div>
            </div>
            {#if r && r.steps.length}
              <ul class="ai-check" aria-label="Test connection: {p.name}">
                {#each r.steps as s (s.what)}
                  <li class:ai-ok={s.ok} class:ai-fail={!s.ok}>
                    <Icon name={s.ok ? "circle-check" : "circle-x"} size={15} />
                    <span><strong>{s.what}</strong>: {s.detail} <small>({seconds(s.millis)})</small></span>
                  </li>
                {/each}
              </ul>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}

    {#if editing}
      <form class="form-card ai-form" onsubmit={save} aria-label={editing.id ? "Change provider" : "Add a provider"}>
        <div class="field">
          <label for="ai-kind">Provider</label>
          <Select
            id="ai-kind"
            boxed
            value={editing.kind}
            disabled={!!editing.id}
            options={kinds.map((k) => ({ value: k.kind, label: k.label }))}
            onchange={(k) => editing && (editing.kind = k)}
          />
        </div>
        <div class="field">
          <label for="ai-name">Name</label>
          <input id="ai-name" class="input" bind:value={editing.name} maxlength="80" placeholder={editingInfo?.label} />
        </div>
        <div class="field">
          <label for="ai-url">{editingInfo?.defaultBaseUrl ? "Address (optional)" : "Address"}</label>
          <input
            id="ai-url"
            class="input"
            bind:value={editing.baseUrl}
            inputmode="url"
            autocomplete="off"
            spellcheck="false"
            required={!editingInfo?.defaultBaseUrl}
            placeholder={editingInfo?.defaultBaseUrl ?? "http://localhost:11434/v1"}
          />
        </div>
        <div class="field">
          <label for="ai-key">{editingInfo?.needsKey ? "API key" : "API key (if it needs one)"}</label>
          <input
            id="ai-key"
            class="input"
            type="password"
            bind:value={editing.apiKey}
            autocomplete="off"
            spellcheck="false"
            required={!!editingInfo?.needsKey && !editing.keyHint}
            placeholder={editing.keyHint ? `Saved (${editing.keyHint}); type to replace` : ""}
          />
        </div>
        {#if formError}<p class="field-error" role="alert">{formError}</p>{/if}
        <div class="button-row form-actions">
          <button class="btn ghost" type="button" onclick={() => (editing = null)}>Cancel</button>
          <button class="btn primary" type="submit" disabled={busy}>Save</button>
        </div>
      </form>
    {:else}
      <button class="btn" onclick={add}><Icon name="plus" size={15} />Add a provider…</button>
    {/if}
  </div>

  {#if providers.length}
    <div class="set-group">
      <h3 class="set-title">Models</h3>
      {#each FEATURES as { feature, title, hint } (feature)}
        {@const d = drafts[feature]}
        {#if d}
          <div class="set-row ai-feature-row">
            <div class="set-row-text">
              <span class="set-label">{title}</span>
              <span class="set-desc">{hint}</span>
            </div>
            <div class="ai-feature">
              <Select
                label="{title}: provider"
                boxed
                value={d.provider}
                options={[{ value: "", label: "Off" }, ...forFeature(feature).map((p) => ({ value: p.id, label: p.name }))]}
                onchange={(v) => {
                  d.provider = v;
                  d.model = "";
                  void loadModels(d.provider);
                  if (!d.provider) void pick(feature);
                }}
              />
              {#if d.provider}
                <input
                  class="input"
                  aria-label="{title}: model"
                  list="ai-models-{feature}"
                  bind:value={d.model}
                  placeholder="Model"
                  autocomplete="off"
                  spellcheck="false"
                  onfocus={() => loadModels(d.provider)}
                  onchange={() => pick(feature)}
                />
                <datalist id="ai-models-{feature}">
                  {#each suggestions(feature, d.provider) as m (m.id)}<option value={m.id}>{m.name ?? m.id}</option>{/each}
                </datalist>
              {/if}
            </div>
          </div>
        {/if}
      {/each}
    </div>
  {/if}
{/if}
