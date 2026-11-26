<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { CoreClient } from "../core/client";
  import type { AccountStatus, SyncPhase } from "../core/types";

  let {
    client,
    status,
    onclose,
  }: { client: CoreClient; status: AccountStatus; onclose: () => void } = $props();

  const LAST_SERVER = "drusite.lastServer";

  let server = $state(localStorage.getItem(LAST_SERVER) ?? "");
  let busy = $state(false);
  let problem = $state<string | null>(null);

  async function act(f: () => Promise<void>) {
    busy = true;
    problem = null;
    try {
      await f();
    } catch (e) {
      problem = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function signIn(e: SubmitEvent) {
    e.preventDefault();
    const typed = server.trim();
    if (typed === "") return;
    localStorage.setItem(LAST_SERVER, typed);
    void act(() => client.signIn(typed));
  }

  function key(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }

  const message = $derived(problem ?? status.message);
</script>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="sheet-backdrop" onclick={onclose} onkeydown={key}>
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="sheet"
    role="dialog"
    aria-modal="true"
    aria-labelledby="account-title"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
  >
    <div class="sheet-head">
      <h2 id="account-title">Sync</h2>
      <button class="sheet-close" aria-label="Close" onclick={onclose}>×</button>
    </div>

    {#if status.phase === "signedOut"}
      <p>
        Sign in to a Drusite server to keep your notes in sync between your devices. Until then,
        they stay on this device only.
      </p>
      <form onsubmit={signIn}>
        <label for="server">Server address</label>
        <div class="field-row">
          <input
            id="server"
            type="text"
            inputmode="url"
            autocapitalize="off"
            autocomplete="url"
            spellcheck="false"
            placeholder="notes.example.com"
            bind:value={server}
            disabled={busy}
          />
          <button class="primary" type="submit" disabled={busy || server.trim() === ""}>
            {busy ? "Opening…" : "Sign in"}
          </button>
        </div>
      </form>
    {:else if status.phase === "signingIn"}
      <p>Finish signing in to <strong>{status.server}</strong> in the browser, then come back here.</p>
      <button disabled={busy} onclick={() => act(() => client.cancelSignIn())}>Cancel</button>
    {:else}
      <dl class="account-facts">
        <dt>Server</dt>
        <dd>{status.server}</dd>
        {#if status.email}
          <dt>Account</dt>
          <dd>{status.email}</dd>
        {/if}
        <dt>Status</dt>
        <dd><span class="sync-dot {status.phase}"></span> {phaseLabel(status.phase)}</dd>
      </dl>
      <button class="danger" disabled={busy} onclick={() => act(() => client.signOut())}>
        Sign out
      </button>
      <p class="hint">Signing out keeps your notes on this device.</p>
    {/if}

    {#if message}
      <p class="sheet-message" role="status">{message}</p>
    {/if}
  </div>
</div>

<script lang="ts" module>
  export function phaseLabel(phase: SyncPhase): string {
    switch (phase) {
      case "signedOut":
        return "Not syncing";
      case "signingIn":
        return "Signing in…";
      case "connecting":
        return "Connecting…";
      case "online":
        return "Synced";
      case "offline":
        return "Offline; changes wait on this device";
      case "failed":
        return "Not syncing";
    }
  }
</script>
