<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Settings → Account & sync: signing in to a server, and what this device syncs with. -->
<script lang="ts">
  import type { CoreClient } from "../core/client";
  import type { AccountStatus, SyncPhase } from "../core/types";
  import Icon from "./ui/Icon.svelte";

  let { client, status }: { client: CoreClient; status: AccountStatus } = $props();

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

  const message = $derived(problem ?? status.message);
</script>

<div class="set-group">
  {#if status.phase === "signedOut"}
    <div class="account-card">
      <span class="account-avatar"><Icon name="cloud-off" size={20} /></span>
      <div class="account-card-text">
        <strong>Not syncing</strong>
        <span class="hint">Your notes are on this device only.</span>
      </div>
    </div>
    <p class="set-desc">
      Sign in to a Drusite server to keep your notes in sync between your devices. Signing in opens the server's
      sign-in page in your browser.
    </p>
    <form class="field" onsubmit={signIn}>
      <label for="server">Server address</label>
      <div class="field-inline">
        <input
          id="server"
          class="input"
          type="text"
          inputmode="url"
          autocapitalize="off"
          autocomplete="url"
          spellcheck="false"
          placeholder="notes.example.com"
          bind:value={server}
          disabled={busy}
        />
        <button class="btn primary" type="submit" disabled={busy || server.trim() === ""}>
          {busy ? "Opening…" : "Sign in"}
        </button>
      </div>
    </form>
  {:else if status.phase === "signingIn"}
    <div class="account-card">
      <span class="account-avatar"><Icon name="loader-circle" size={20} class="spin" /></span>
      <div class="account-card-text">
        <strong>Signing in…</strong>
        <span class="hint">Finish signing in to <strong>{status.server}</strong> in the browser, then come back here.</span>
      </div>
    </div>
    <button class="btn" disabled={busy} onclick={() => act(() => client.cancelSignIn())}>Cancel</button>
  {:else}
    <div class="account-card">
      <span class="account-avatar filled">{(status.email ?? status.server ?? "?").slice(0, 1).toUpperCase()}</span>
      <div class="account-card-text">
        <strong>{status.email ?? "Signed in"}</strong>
        <span class="hint">{status.server}</span>
      </div>
    </div>
    <div class="set-row">
      <div class="set-row-text">
        <span class="set-label">Server</span>
      </div>
      <span class="set-value">{status.server}</span>
    </div>
    {#if status.email}
      <div class="set-row">
        <div class="set-row-text"><span class="set-label">Account</span></div>
        <span class="set-value">{status.email}</span>
      </div>
    {/if}
    <div class="set-row">
      <div class="set-row-text"><span class="set-label">Status</span></div>
      <span class="set-value"><span class="dot sync-dot {status.phase}"></span> {phaseLabel(status.phase)}</span>
    </div>
    <div class="set-row">
      <div class="set-row-text">
        <span class="set-label">Sign out</span>
        <span class="set-desc">Signing out keeps your notes on this device.</span>
      </div>
      <button class="btn danger" disabled={busy} onclick={() => act(() => client.signOut())}>
        <Icon name="log-out" size={15} />Sign out
      </button>
    </div>
  {/if}

  {#if message}
    <p class="field-error" role="status">{message}</p>
  {/if}
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
