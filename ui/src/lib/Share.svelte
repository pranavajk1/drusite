<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Share to web: a page as a read-only link anyone can open, rendered by the server. A popover
  under the top bar's Share button on desktops, a sheet on phones. The server keeps what counts
  (who may publish, the link, the password); every device shows the state through the
  workspace, so this asks the server when it opens and falls back to what the workspace heard.
-->
<script lang="ts">
  import { onMount, tick } from "svelte";

  import type { CoreClient } from "../core/client";
  import type { PageId, ShareChange, ShareInfo } from "../core/types";
  import Icon from "./ui/Icon.svelte";
  import Modal from "./ui/Modal.svelte";
  import Switch from "./ui/Switch.svelte";
  import { dismissable, placeBelow, portal, type Place } from "./ui/floating";

  let {
    client,
    page,
    title,
    anchor = null,
    phone = false,
    onclose,
  }: {
    client: CoreClient;
    page: PageId;
    title: string;
    /** The button it opened from, on desktops. */
    anchor?: HTMLElement | null;
    phone?: boolean;
    onclose: () => void;
  } = $props();

  let share = $state<ShareInfo | null>(null);
  let busy = $state(false);
  let problem = $state<string | null>(null);
  let copied = $state(false);
  let confirmNewLink = $state(false);
  let settingPassword = $state(false);
  let password = $state("");
  let pop = $state<HTMLElement | null>(null);
  let place = $state<Place>({ top: 0, left: 0 });
  // svelte-ignore state_referenced_locally
  const from = anchor;

  const uid = `share-${Math.random().toString(36).slice(2, 9)}`;
  const MIN_PASSWORD = 4;

  onMount(() => {
    void load();
    if (phone || !from) return;
    let open = true;
    void tick().then(() => {
      if (!open || !pop) return;
      const r = from.getBoundingClientRect();
      const size = pop.getBoundingClientRect();
      // Right edges lined up, as Notion's Share: the button is at the right of the top bar.
      const p = placeBelow(r, size);
      place = { ...p, left: Math.max(8, Math.min(r.right - size.width, window.innerWidth - 8 - size.width)) };
      pop.focus({ preventScroll: true });
    });
    const undo = dismissable(() => [pop, from], close);
    return () => {
      open = false;
      undo();
    };
  });

  function close() {
    onclose();
    from?.focus();
  }

  function words(e: unknown): string {
    if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
    return String(e);
  }

  async function load() {
    try {
      share = await client.share(page);
      problem = share.available ? null : share.message;
    } catch (e) {
      problem = words(e);
    }
  }

  async function run(what: () => Promise<ShareInfo>) {
    busy = true;
    try {
      share = await what();
      problem = null;
    } catch (e) {
      problem = words(e);
    } finally {
      busy = false;
    }
  }

  const change = (c: ShareChange) => run(() => client.publish(page, c));
  const empty: ShareChange = { includeSubpages: null, allowIndexing: null, expiry: null, password: null };

  function toggle(on: boolean) {
    if (on) void change(empty);
    else void run(() => client.unpublish(page));
  }

  async function copy() {
    if (!share?.url) return;
    try {
      await navigator.clipboard.writeText(share.url);
      copied = true;
      setTimeout(() => (copied = false), 2000);
    } catch {
      // No clipboard (an old WebView): select the link so it can be copied by hand.
      const input = document.getElementById(`${uid}-url`) as HTMLInputElement | null;
      input?.select();
    }
  }

  /** The end of the chosen day, on this device, as the server takes it. */
  function endOfDay(date: string): number {
    const [y, m, d] = date.split("-").map(Number);
    return new Date(y!, m! - 1, d!, 23, 59, 59).getTime();
  }

  function dateInput(ms: number): string {
    const d = new Date(ms);
    const pad = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  }

  const tomorrow = () => dateInput(Date.now() + 24 * 60 * 60 * 1000);

  function setExpiry(on: boolean) {
    if (on) void change({ ...empty, expiry: { type: "set", at: endOfDay(dateInput(Date.now() + 7 * 24 * 60 * 60 * 1000)) } });
    else void change({ ...empty, expiry: { type: "clear" } });
  }

  function pickDate(e: Event) {
    const v = (e.currentTarget as HTMLInputElement).value;
    if (v) void change({ ...empty, expiry: { type: "set", at: endOfDay(v) } });
  }

  function setPasswordSwitch(on: boolean) {
    if (on) {
      settingPassword = true;
      void tick().then(() => document.getElementById(`${uid}-password`)?.focus());
    } else {
      settingPassword = false;
      password = "";
      if (share?.hasPassword) void change({ ...empty, password: { type: "clear" } });
    }
  }

  async function savePassword(e: SubmitEvent) {
    e.preventDefault();
    if (password.length < MIN_PASSWORD) {
      problem = `A password needs at least ${MIN_PASSWORD} characters.`;
      return;
    }
    await change({ ...empty, password: { type: "set", password } });
    if (!problem) {
      settingPassword = false;
      password = "";
    }
  }

  async function newLink() {
    confirmNewLink = false;
    await run(() => client.regenerateShareLink(page));
  }

  const can = $derived(!!share?.canPublish && !busy);
</script>

{#snippet body()}
  <div class="share">
    <div class="share-row share-main">
      <span class="share-globe" class:on={share?.published} aria-hidden="true"><Icon name="globe" size={18} /></span>
      <div class="share-row-text">
        <label class="share-label" for="{uid}-publish">Publish to web</label>
        <span class="share-desc">
          {#if share?.published}Anyone with the link can read “{title || "Untitled"}”.{:else}Share a read-only link anyone can open.{/if}
        </span>
      </div>
      <Switch id="{uid}-publish" checked={!!share?.published} disabled={!can} onchange={toggle} label="Publish to web" />
    </div>

    {#if problem}
      <p class="share-problem" role="status"><Icon name="info" size={14} />{problem}</p>
    {:else if share && !share.canPublish && share.available}
      <p class="share-note">Only the workspace's editors and owners can publish pages.</p>
    {/if}

    {#if share?.published && share.url}
      {#if share.expired}
        <p class="share-problem" role="status"><Icon name="circle-alert" size={14} />This link has expired. Change the date or turn expiry off to bring it back.</p>
      {/if}
      <div class="share-link">
        <input id="{uid}-url" class="input share-url" readonly value={share.url} aria-label="Public link" onfocus={(e) => e.currentTarget.select()} />
        <button class="btn primary small share-copy" onclick={copy}>
          <Icon name={copied ? "check" : "copy"} size={14} />{copied ? "Copied" : "Copy link"}
        </button>
      </div>

      <div class="share-options" role="group" aria-label="Options">
        <div class="share-row">
          <div class="share-row-text">
            <label class="share-label" for="{uid}-subpages">Include subpages</label>
            <span class="share-desc">The pages inside are published too, as a small site.</span>
          </div>
          <Switch id="{uid}-subpages" checked={share.includeSubpages} disabled={!can} onchange={(v) => change({ ...empty, includeSubpages: v })} />
        </div>
        <div class="share-row">
          <div class="share-row-text">
            <label class="share-label" for="{uid}-index">Search engines</label>
            <span class="share-desc">Let search engines index it{share.hasPassword ? " (not while it has a password)" : ""}.</span>
          </div>
          <Switch id="{uid}-index" checked={share.allowIndexing} disabled={!can} onchange={(v) => change({ ...empty, allowIndexing: v })} />
        </div>
        <div class="share-row">
          <div class="share-row-text">
            <label class="share-label" for="{uid}-expiry">Link expires</label>
            <span class="share-desc">The link stops working after a day you pick.</span>
          </div>
          <Switch id="{uid}-expiry" checked={share.expiresAt !== null} disabled={!can} onchange={setExpiry} />
        </div>
        {#if share.expiresAt !== null}
          <div class="share-sub">
            <label class="share-sub-label" for="{uid}-date">Last day</label>
            <input
              id="{uid}-date"
              class="input share-date"
              type="date"
              min={tomorrow()}
              value={dateInput(share.expiresAt)}
              disabled={!can}
              onchange={pickDate}
            />
          </div>
        {/if}
        <div class="share-row">
          <div class="share-row-text">
            <label class="share-label" for="{uid}-pw">Password</label>
            <span class="share-desc">Readers need a password to open it.</span>
          </div>
          <Switch id="{uid}-pw" checked={share.hasPassword || settingPassword} disabled={!can} onchange={setPasswordSwitch} />
        </div>
        {#if settingPassword || (share.hasPassword && can)}
          <form class="share-sub" onsubmit={savePassword}>
            {#if settingPassword}
              <input
                id="{uid}-password"
                class="input share-password"
                type="password"
                autocomplete="new-password"
                maxlength={256}
                placeholder={share.hasPassword ? "New password" : "Password"}
                aria-label="Password"
                bind:value={password}
              />
              <button class="btn small" type="submit" disabled={busy}>Save</button>
            {:else}
              <span class="share-desc share-lock"><Icon name="lock" size={14} />Password set</span>
              <button class="btn small ghost" type="button" onclick={() => setPasswordSwitch(true)}>Change</button>
            {/if}
          </form>
        {/if}
      </div>

      {#if can}
        <div class="share-foot">
          {#if confirmNewLink}
            <span class="share-desc">The current link will stop working.</span>
            <button class="btn small ghost" onclick={() => (confirmNewLink = false)}>Cancel</button>
            <button class="btn small" onclick={newLink}>Make a new link</button>
          {:else}
            <button class="btn small ghost" onclick={() => (confirmNewLink = true)}><Icon name="refresh-cw" size={14} />New link</button>
            <button class="btn small ghost danger" onclick={() => toggle(false)}>Unpublish</button>
          {/if}
        </div>
      {/if}
    {/if}
  </div>
{/snippet}

{#if phone}
  <Modal title="Share" onclose={onclose} phone="sheet" size="sm" class="share-sheet">
    {@render body()}
  </Modal>
{:else}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    use:portal
    bind:this={pop}
    class="popover share-popover"
    role="dialog"
    aria-label="Share"
    tabindex="-1"
    style:top="{place.top}px"
    style:left="{place.left}px"
    onkeydown={(e) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        close();
      }
    }}
  >
    <div class="share-head">Share</div>
    {@render body()}
  </div>
{/if}
