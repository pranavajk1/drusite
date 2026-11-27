<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Ask your notes: a panel beside the page (full screen on phones), as in Notion AI. Answers are
  written on the server from the asker's pages and stream in; each citation opens its block.
  Conversations are kept in the account, so every device has them.
-->
<script lang="ts">
  import { onMount, tick } from "svelte";

  import type { CoreClient } from "../core/client";
  import type { AskSource, AskStatus, BlockId, ConversationSummary, PageId, Settings } from "../core/types";
  import { today } from "../journal/dates";
  import { blocks, citedIn, type Inline } from "./ask/render";
  import Icon from "./ui/Icon.svelte";
  import Menu from "./ui/Menu.svelte";

  let {
    client,
    settings,
    signedIn,
    onclose,
    oncite,
    onsettings,
  }: {
    client: CoreClient;
    settings: Settings;
    /** Changes when the account does, so the status is asked again. */
    signedIn: string;
    onclose: () => void;
    oncite: (page: PageId, block: BlockId | null) => void;
    onsettings: (section: "account" | "journal") => void;
  } = $props();

  /** A message as the panel shows it; an answer may still be streaming. */
  type Shown = {
    key: string;
    role: "user" | "assistant";
    text: string;
    /** Every source the answer could cite, by number. */
    sources: AskSource[];
    error: string | null;
    streaming: boolean;
  };

  let status = $state<AskStatus | null>(null);
  let view = $state<"chat" | "history">("chat");
  let list = $state<ConversationSummary[]>([]);
  let conversation = $state<string | null>(null);
  let title = $state("");
  let messages = $state<Shown[]>([]);
  let question = $state("");
  let request = $state<string | null>(null);
  /** Before the first words of an answer. */
  let phase = $state<"searching" | "writing" | null>(null);
  let problem = $state<string | null>(null);
  let renaming = $state<string | null>(null);
  let renameText = $state("");
  let confirming = $state<string | null>(null);
  let rowMenu = $state<{ anchor: HTMLElement; id: string } | null>(null);
  let scroller = $state<HTMLElement | null>(null);
  /**
   * The on-screen keyboard's height, where it covers the window rather than shrinking it (iOS,
   * Android 15 and later): the full-screen panel ends above it, so the question box stays in view
   * and the page underneath isn't scrolled to reach it.
   */
  let keyboard = $state(0);
  let input = $state<HTMLTextAreaElement | null>(null);

  let seq = 0;
  const newRequest = () => `ask-${Date.now().toString(36)}-${(seq += 1)}`;

  async function refreshStatus() {
    try {
      status = await client.askStatus();
    } catch (e) {
      status = { signedIn: false, available: false, message: String(e), reason: "unreachable", model: null, semantic: false, embeddingModel: null, indexing: 0 };
    }
  }

  async function refreshList() {
    if (!status?.available) return;
    try {
      list = await client.conversations();
    } catch (e) {
      problem = e instanceof Error ? e.message : String(e);
    }
  }

  $effect(() => {
    void signedIn;
    void refreshStatus().then(refreshList);
  });

  onMount(() => {
    const stop = client.subscribe((event) => {
      if (event.type !== "ask" || event.request !== request) return;
      const u = event.update;
      const last = messages[messages.length - 1];
      if (u.type === "started") {
        conversation = u.conversation;
        title = u.title;
        phase = "searching";
      } else if (u.type === "sources") {
        if (last?.streaming) last.sources = u.sources;
        phase = "writing";
      } else if (u.type === "text") {
        if (last?.streaming) last.text += u.text;
        phase = null;
        void follow();
      } else if (u.type === "done" || u.type === "failed") {
        if (last?.streaming) {
          last.streaming = false;
          if (u.type === "failed") last.error = u.message;
        }
        request = null;
        phase = null;
        void refreshList();
      }
    });
    const focus = () => void refreshStatus().then(refreshList);
    window.addEventListener("focus", focus);
    const vv = window.visualViewport;
    const measure = () => {
      if (!vv) return;
      keyboard = Math.max(0, Math.round(window.innerHeight - vv.height - vv.offsetTop));
      // The browser may have scrolled the layout to show the focused box; the panel is fixed,
      // so put it back.
      if (keyboard > 0 && window.matchMedia("(max-width: 700px)").matches) window.scrollTo(0, 0);
    };
    vv?.addEventListener("resize", measure);
    vv?.addEventListener("scroll", measure);
    void tick().then(() => input?.focus());
    return () => {
      stop();
      window.removeEventListener("focus", focus);
      vv?.removeEventListener("resize", measure);
      vv?.removeEventListener("scroll", measure);
      if (request) void client.cancelAsk(request);
    };
  });

  /** Keeps the newest words in view, unless the person scrolled up to read. */
  async function follow(force = false) {
    const el = scroller;
    if (!el) return;
    const near = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    await tick();
    if (force || near) el.scrollTop = el.scrollHeight;
  }

  async function send() {
    const q = question.trim();
    if (!q || request || !status?.available) return;
    question = "";
    problem = null;
    const id = newRequest();
    messages.push({ key: `${id}-q`, role: "user", text: q, sources: [], error: null, streaming: false });
    messages.push({ key: `${id}-a`, role: "assistant", text: "", sources: [], error: null, streaming: true });
    request = id;
    phase = "searching";
    void follow(true);
    try {
      await client.ask(id, conversation, q, today());
    } catch (e) {
      const last = messages[messages.length - 1];
      if (last) {
        last.streaming = false;
        last.error = e instanceof Error ? e.message : String(e);
      }
      request = null;
      phase = null;
    }
  }

  async function stop() {
    if (!request) return;
    const r = request;
    request = null;
    phase = null;
    const last = messages[messages.length - 1];
    if (last?.streaming) last.streaming = false;
    await client.cancelAsk(r);
  }

  function key(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      void send();
    }
  }

  function newChat() {
    if (request) void stop();
    conversation = null;
    title = "";
    messages = [];
    view = "chat";
    problem = null;
    void tick().then(() => input?.focus());
  }

  async function open(id: string) {
    if (request) void stop();
    try {
      const c = await client.conversation(id);
      conversation = c.id;
      title = c.title;
      messages = c.messages.map((m) => ({
        key: m.id,
        role: m.role,
        text: m.text,
        sources: m.sources,
        error: m.error,
        streaming: false,
      }));
      view = "chat";
      problem = null;
      void follow(true);
    } catch (e) {
      problem = e instanceof Error ? e.message : String(e);
    }
  }

  async function showHistory() {
    view = view === "history" ? "chat" : "history";
    if (view === "history") await refreshList();
  }

  async function rename(id: string) {
    const t = renameText.trim();
    renaming = null;
    if (!t) return;
    try {
      await client.renameConversation(id, t);
      if (conversation === id) title = t;
      await refreshList();
    } catch (e) {
      problem = e instanceof Error ? e.message : String(e);
    }
  }

  async function remove(id: string) {
    confirming = null;
    try {
      await client.deleteConversation(id);
      if (conversation === id) newChat();
      view = "history";
      await refreshList();
    } catch (e) {
      problem = e instanceof Error ? e.message : String(e);
    }
  }

  function startRename(id: string, current: string) {
    renaming = id;
    renameText = current;
    void tick().then(() => document.getElementById(`rename-${id}`)?.focus());
  }

  function when(ms: number): string {
    const d = new Date(ms);
    const now = new Date();
    if (d.toDateString() === now.toDateString()) return d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
    const days = Math.round((now.getTime() - d.getTime()) / 86_400_000);
    if (days < 7) return d.toLocaleDateString(undefined, { weekday: "short" });
    return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  }

  function source(m: Shown, n: number): AskSource | undefined {
    return m.sources.find((s) => s.n === n);
  }

  function cited(m: Shown): AskSource[] {
    const known = new Set(m.sources.map((s) => s.n));
    return citedIn(m.text, known)
      .map((n) => source(m, n))
      .filter((s): s is AskSource => !!s);
  }

  function cite(s: AskSource | undefined) {
    if (!s || s.missing) return;
    oncite(s.page, s.block);
  }

  function rowItems(id: string) {
    const current = list.find((c) => c.id === id)?.title ?? "";
    return [
      { label: "Rename", icon: "square-pen" as const, run: () => startRename(id, current) },
      { label: "Delete", icon: "trash-2" as const, danger: true, run: () => (confirming = id) },
    ];
  }

  const unavailable = $derived(status !== null && !status.available);
</script>

{#snippet line(m: Shown, parts: Inline[])}
  {#each parts as p, i (i)}
    {#if p.kind === "cite"}
      {@const s = source(m, p.n)}
      {#if s}
        <button
          class="ask-cite"
          class:missing={s.missing}
          title={s.missing ? "That page is gone" : s.title || "Untitled"}
          aria-label="Source {p.n}: {s.title || 'Untitled'}"
          disabled={s.missing}
          onclick={() => cite(s)}>{p.n}</button
        >
      {/if}
    {:else if p.kind === "code"}
      <code>{p.text}</code>
    {:else if p.bold}
      <strong>{p.text}</strong>
    {:else if p.italic}
      <em>{p.text}</em>
    {:else}
      {p.text}
    {/if}
  {/each}
{/snippet}

<section class="ask" aria-label="Ask your notes" style:--ask-keyboard="{keyboard}px">
  <header class="ask-head">
    <span class="ask-title">
      {#if view === "history"}Chats{:else}{title || "Ask your notes"}{/if}
    </span>
    <button class="icon-btn" aria-label="Chats" title="Chats" aria-pressed={view === "history"} onclick={showHistory}>
      <Icon name="history" />
    </button>
    <button class="icon-btn" aria-label="New chat" title="New chat" onclick={newChat}>
      <Icon name="square-pen" />
    </button>
    <button class="icon-btn" aria-label="Close Ask" title="Close (Esc)" onclick={onclose}>
      <Icon name="x" />
    </button>
  </header>

  {#if view === "history"}
    <div class="ask-scroll">
      {#if list.length === 0}
        <div class="empty">
          <Icon name="history" size={28} />
          <p>No chats yet.</p>
        </div>
      {:else}
        <ul class="ask-list" aria-label="Chats">
          {#each list as c (c.id)}
            <li class="ask-list-row" class:current={c.id === conversation}>
              {#if renaming === c.id}
                <input
                  id="rename-{c.id}"
                  class="input ask-rename"
                  aria-label="Chat name"
                  maxlength="120"
                  bind:value={renameText}
                  onkeydown={(e) => {
                    if (e.key === "Enter") void rename(c.id);
                    else if (e.key === "Escape") {
                      e.stopPropagation();
                      renaming = null;
                    }
                  }}
                  onblur={() => void rename(c.id)}
                />
              {:else if confirming === c.id}
                <span class="ask-confirm">Delete “{c.title}”?</span>
                <button class="btn small danger solid" onclick={() => remove(c.id)}>Delete</button>
                <button class="btn small ghost" onclick={() => (confirming = null)}>Cancel</button>
              {:else}
                <button class="ask-list-open" onclick={() => open(c.id)}>
                  <Icon name="message-square-text" />
                  <span class="ask-list-title">{c.title || "Untitled chat"}</span>
                  <span class="ask-list-when">{when(c.updatedAt)}</span>
                </button>
                <button
                  class="icon-btn ask-list-more"
                  aria-label="Chat options"
                  aria-haspopup="menu"
                  onclick={(e) => (rowMenu = { anchor: e.currentTarget, id: c.id })}
                >
                  <Icon name="ellipsis" size={14} />
                </button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {:else}
    <div class="ask-scroll" bind:this={scroller}>
      {#if unavailable}
        <div class="ask-empty">
          <Icon name="sparkles" size={28} />
          <p class="ask-empty-title">Ask your notes isn't available</p>
          <p class="ask-empty-text">{status?.message}</p>
          {#if status?.reason === "not_signed_in"}
            <button class="btn primary" onclick={() => onsettings("account")}>Sign in to sync</button>
          {/if}
        </div>
      {:else if messages.length === 0}
        <div class="ask-empty">
          <Icon name="sparkles" size={28} />
          <p class="ask-empty-title">Ask a question about your notes</p>
          <p class="ask-empty-text">
            Answers come from your pages, with a link to each block they use{#if status?.model}, written by {status.model}{/if}.
          </p>
          <p class="ask-empty-text">
            {#if settings.askJournal}Journal entries are included.{:else}Journal entries are left out.{/if}
            <button class="link-btn" onclick={() => onsettings("journal")}>Change</button>
          </p>
          {#if status && !status.semantic}
            <p class="ask-empty-note">This server searches words only, not meaning.</p>
          {/if}
          {#if status && status.indexing > 0}
            <p class="ask-empty-note">Still indexing {status.indexing} {status.indexing === 1 ? "page" : "pages"}.</p>
          {/if}
        </div>
      {:else}
        <ol class="ask-messages">
          {#each messages as m (m.key)}
            <li class="ask-msg {m.role}">
              {#if m.role === "user"}
                <p class="ask-question">{m.text}</p>
              {:else}
                {#if m.streaming && m.text === ""}
                  <p class="ask-phase" role="status">
                    <Icon name="loader-circle" size={14} />
                    {phase === "writing" ? "Writing…" : "Searching your notes…"}
                  </p>
                {/if}
                <div class="ask-answer" aria-live={m.streaming ? "polite" : "off"}>
                  {#each blocks(m.text) as b, i (i)}
                    {#if b.kind === "p"}
                      <p>{@render line(m, b.inlines)}</p>
                    {:else if b.kind === "h"}
                      <p class="ask-h">{@render line(m, b.inlines)}</p>
                    {:else if b.kind === "ul"}
                      <ul>{#each b.items as item, j (j)}<li>{@render line(m, item)}</li>{/each}</ul>
                    {:else}
                      <ol start={b.start}>{#each b.items as item, j (j)}<li>{@render line(m, item)}</li>{/each}</ol>
                    {/if}
                  {/each}
                </div>
                {#if m.error}
                  <p class="ask-error" role="alert"><Icon name="circle-alert" size={14} />{m.error}</p>
                {/if}
                {#if !m.streaming && cited(m).length > 0}
                  <div class="ask-sources" aria-label="Sources">
                    {#each cited(m) as s (s.n)}
                      <button class="ask-source" disabled={s.missing} onclick={() => cite(s)}>
                        <span class="ask-source-n">{s.n}</span>
                        <Icon name="file-text" size={14} />
                        <span class="ask-source-title">{s.missing ? "Page gone" : s.title || "Untitled"}</span>
                      </button>
                    {/each}
                  </div>
                {/if}
              {/if}
            </li>
          {/each}
        </ol>
      {/if}
      {#if problem}
        <p class="ask-error" role="alert"><Icon name="circle-alert" size={14} />{problem}</p>
      {/if}
    </div>

    <form
      class="ask-compose"
      onsubmit={(e) => {
        e.preventDefault();
        void send();
      }}
    >
      <textarea
        bind:this={input}
        bind:value={question}
        class="ask-input"
        rows="1"
        maxlength="4000"
        placeholder={messages.length ? "Ask a follow-up…" : "Ask anything about your notes…"}
        aria-label="Question"
        disabled={unavailable}
        onkeydown={key}
      ></textarea>
      {#if request}
        <button type="button" class="icon-btn ask-send" aria-label="Stop" title="Stop" onclick={stop}>
          <Icon name="square" />
        </button>
      {:else}
        <button type="submit" class="icon-btn ask-send" aria-label="Ask" title="Ask (Enter)" disabled={!question.trim() || unavailable}>
          <Icon name="arrow-up" />
        </button>
      {/if}
    </form>
  {/if}
</section>

{#if rowMenu}
  <Menu anchor={rowMenu.anchor} label="Chat options" onclose={() => (rowMenu = null)} items={rowItems(rowMenu.id)} />
{/if}
