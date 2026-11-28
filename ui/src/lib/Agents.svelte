<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Agents in Settings: connecting one (to the server, or to this computer), the ones connected
     here, and what agents changed, each with an undo. -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { CoreClient } from "../core/client";
  import type { AgentAccess, AgentCall, AgentToken, LocalAgents } from "../core/types";
  import Icon from "./ui/Icon.svelte";
  import Switch from "./ui/Switch.svelte";

  let {
    client,
    server,
    onerror,
  }: {
    client: CoreClient;
    /** The server this device syncs with, if it's signed in. */
    server: string | null;
    onerror: (e: unknown) => void;
  } = $props();

  let local = $state<LocalAgents | null>(null);
  let tokens = $state<AgentToken[]>([]);
  let activity = $state<AgentCall[]>([]);
  let serverProblem = $state<string | null>(null);
  let busy = $state(false);

  async function load() {
    try {
      const a = await client.agents();
      local = a.local;
      tokens = a.tokens;
      activity = a.activity;
      serverProblem = a.serverProblem;
    } catch (e) {
      onerror(e);
    }
  }

  onMount(() => {
    const stop = client.subscribe((event) => {
      if (event.type === "agentsChanged") void load();
    });
    void load();
    return stop;
  });

  const serverMcp = $derived(server ? `${server.replace(/\/+$/, "")}/mcp` : null);

  // Connecting an agent on this computer.
  let name = $state("Claude Code");
  let write = $state(false);
  let journal = $state(false);
  let made = $state<{ token: string; url: string; name: string } | null>(null);
  let copied = $state(false);

  async function setEnabled(enabled: boolean) {
    if (!local) return;
    busy = true;
    try {
      await client.setLocalAgents(enabled, local.port);
    } catch (e) {
      onerror(e);
    } finally {
      busy = false;
    }
  }

  async function connect(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    try {
      const access: AgentAccess = { write, journal: write && journal };
      const r = await client.createAgentToken(name, access);
      made = { ...r, name };
      copied = false;
    } catch (e) {
      onerror(e);
    } finally {
      busy = false;
    }
  }

  /** The command that connects Claude Code; other agents take the same URL and header. */
  function command(m: { token: string; url: string; name: string }): string {
    const id = m.name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "drusite";
    return `claude mcp add --transport http drusite-${id} ${m.url} --header "Authorization: Bearer ${m.token}"`;
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = true;
    } catch (e) {
      onerror(e);
    }
  }

  async function disconnect(t: AgentToken) {
    try {
      await client.revokeAgentToken(t.id);
    } catch (e) {
      onerror(e);
    }
  }

  async function undo(call: AgentCall) {
    try {
      await client.undoAgentCall(call.id, call.source);
      await load();
    } catch (e) {
      onerror(e);
    }
  }

  function accessText(a: AgentAccess): string {
    if (!a.write) return "reads only";
    return a.journal ? "reads and edits, journal too" : "reads and edits";
  }

  function when(ms: number): string {
    const minutes = Math.round((Date.now() - ms) / 60000);
    if (minutes < 1) return "just now";
    if (minutes < 60) return `${minutes} min ago`;
    const d = new Date(ms);
    return d.toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
  }
</script>

<div class="set-group">
  <p class="set-desc">
    Agents such as Claude Code can read your notes and, if you allow it, add to and edit them. They can never delete
    pages, and journal entries stay read-only to them unless you allow more. Everything they change is listed below,
    where you can undo it.
  </p>
</div>

{#if serverMcp}
  <div class="set-group">
    <h3 class="set-title">Through your server</h3>
    <p class="set-desc">
      To connect an agent to your server, from any computer (it signs in through the browser and asks you what it may
      do):
    </p>
    <div class="code-copy">
      <pre class="agent-command"><code>claude mcp add --transport http drusite {serverMcp}</code></pre>
      <button class="icon-btn" aria-label="Copy command" title="Copy" onclick={() => copy(`claude mcp add --transport http drusite ${serverMcp}`)}>
        <Icon name="copy" size={15} />
      </button>
    </div>
  </div>
{/if}

{#if local?.available}
  <div class="set-group">
    <h3 class="set-title">On this computer</h3>
    <div class="set-row">
      <div class="set-row-text">
        <label class="set-label" for="agents-local">Let agents on this computer connect</label>
        <span class="set-desc">
          At <code>{local.url}</code>, while Drusite is open, with a token from here. Agents can also connect through
          your server.
        </span>
      </div>
      <Switch id="agents-local" checked={local.enabled} disabled={busy} onchange={setEnabled} />
    </div>
    {#if local.problem}<p class="field-error">{local.problem}</p>{/if}
    {#if local.enabled}
      {#if made}
        <div class="form-card agent-made" data-testid="agent-made">
          <p class="set-label">Run this to connect {made.name}. The token is shown only now.</p>
          <pre class="agent-command"><code>{command(made)}</code></pre>
          <div class="button-row form-actions">
            <button class="btn ghost" onclick={() => (made = null)}>Done</button>
            <button class="btn primary" onclick={() => made && copy(command(made))}>
              <Icon name={copied ? "check" : "copy"} size={15} />{copied ? "Copied" : "Copy"}
            </button>
          </div>
        </div>
      {:else}
        <form class="form-card agent-form" onsubmit={connect}>
          <div class="field">
            <label for="agent-name">Connect an agent</label>
            <div class="field-inline">
              <input id="agent-name" class="input" bind:value={name} maxlength="100" required />
              <button class="btn primary" type="submit" disabled={busy}>Connect</button>
            </div>
          </div>
          <div class="choices" role="radiogroup" aria-label="What it may do">
            <label class="choice"><input type="radio" name="agent-access" checked={!write} onchange={() => (write = false)} /> Read only</label>
            <label class="choice"><input type="radio" name="agent-access" checked={write} onchange={() => (write = true)} /> Read, add and edit pages</label>
            {#if write}
              <label class="choice choice-nested"><input class="checkbox" type="checkbox" bind:checked={journal} /> Edit journal entries too</label>
            {/if}
          </div>
        </form>
      {/if}
    {/if}
    {#if tokens.length}
      <ul class="list agent-list" aria-label="Agents on this computer">
        {#each tokens as t (t.id)}
          <li class="list-row">
            <Icon name="bot" class="list-row-icon" />
            <div class="list-row-text">
              <span>{t.name}</span>
              <small>{accessText(t.access)} · {t.lastUsedAt ? `last used ${when(t.lastUsedAt)}` : "not used yet"}</small>
            </div>
            <button class="btn small danger" onclick={() => disconnect(t)}>Disconnect</button>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<div class="set-group">
  <h3 class="set-title">What agents changed</h3>
  {#if serverProblem}<p class="hint">The server's list couldn't be read: {serverProblem}</p>{/if}
  {#if activity.length}
    <ul class="list agent-activity" aria-label="What agents changed">
      {#each activity as call (call.source + call.id)}
        <li class="list-row">
          <Icon name="history" class="list-row-icon" />
          <div class="list-row-text">
            <span>{call.summary}</span>
            <small>{call.agent} · {when(call.at)}{call.source === "server" ? " · through the server" : ""}</small>
          </div>
          {#if call.undoneAt}
            <span class="badge muted">Undone</span>
          {:else}
            <button class="btn small" onclick={() => undo(call)}><Icon name="undo-2" size={14} />Undo</button>
          {/if}
        </li>
      {/each}
    </ul>
  {:else}
    <p class="hint">Nothing yet.</p>
  {/if}
</div>
