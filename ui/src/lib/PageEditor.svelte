<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { TextSelection } from "prosemirror-state";

  import { CoreError, type CoreClient } from "../core/client";
  import type { BlockId, PageId, PageInfo, Settings } from "../core/types";
  import { attrsOf } from "../editor/convert";
  import JournalViews from "./JournalViews.svelte";
  import Properties from "./Properties.svelte";
  import { focusEnd } from "../editor/commands";
  import { type Editor, createEditor } from "../editor/editor";
  import PhoneToolbar from "./PhoneToolbar.svelte";

  let {
    client,
    page,
    title,
    info,
    pages,
    settings,
    onday,
    reveal = null,
    onselect,
    onopened,
    onerror,
  }: {
    client: CoreClient;
    page: PageId;
    title: string;
    /** The page's entry in the page list: its kind and properties. */
    info: PageInfo | null;
    pages: PageInfo[];
    settings: Settings;
    onday: (date: string) => void;
    /** A block to scroll to and put the cursor in (from a search hit). */
    reveal?: { block: BlockId; seq: number } | null;
    onselect: (id: PageId) => void;
    onopened: () => void;
    onerror: (e: unknown) => void;
  } = $props();

  // This editor's page, fixed when it's made. The `page` prop is live: once the app switches to
  // another page, it already names that one while this editor is still saving and closing.
  // svelte-ignore state_referenced_locally
  const id = page;

  let mount: HTMLDivElement;
  let editor: Editor | null = null;
  // The phone toolbar shows while the editor has the focus, on touch screens only.
  const touch = window.matchMedia?.("(pointer: coarse)").matches ?? false;
  let editorView = $state<Editor["view"] | null>(null);
  let focused = $state(false);
  /** The page's document hasn't come from the server yet. */
  let arriving = $state(false);
  let draft = $state("");
  let renaming: ReturnType<typeof setTimeout> | undefined;

  // Follow renames from elsewhere unless one is being typed here.
  $effect(() => {
    if (renaming === undefined) draft = title;
  });

  function rename() {
    clearTimeout(renaming);
    renaming = setTimeout(async () => {
      renaming = undefined;
      try {
        await client.renamePage(id, draft);
      } catch (e) {
        onerror(e);
      }
    }, 300);
  }

  function titleKey(e: KeyboardEvent) {
    if (e.key !== "Enter" && e.key !== "ArrowDown") return;
    e.preventDefault();
    if (!editor) return;
    const { view } = editor;
    view.dispatch(view.state.tr.setSelection(TextSelection.atStart(view.state.doc)));
    view.focus();
  }

  let titleInput: HTMLInputElement;

  /** Puts the cursor at the start of `block` and scrolls it into view. */
  function showBlock(block: BlockId): boolean {
    if (!editor) return false;
    const { view } = editor;
    let at = -1;
    view.state.doc.forEach((node, offset) => {
      if (at < 0 && attrsOf(node).id === block) at = offset;
    });
    if (at < 0) return false;
    view.dispatch(view.state.tr.setSelection(TextSelection.create(view.state.doc, at + 1)).scrollIntoView());
    view.focus();
    view.nodeDOM(at)?.parentElement?.querySelector(`[data-block-id="${block}"]`)?.classList.add("revealed");
    return true;
  }

  let revealed = 0;
  $effect(() => {
    if (reveal && reveal.seq !== revealed && editorView && showBlock(reveal.block)) revealed = reveal.seq;
  });

  /** Arrow Up on the first line of the page goes to the title. */
  function bodyKey(e: KeyboardEvent) {
    if (e.key !== "ArrowUp" || e.shiftKey || e.altKey || e.metaKey || e.ctrlKey || !editor) return;
    const { view } = editor;
    const { head, empty } = view.state.selection;
    if (!empty || view.state.doc.resolve(head).index(0) !== 0 || !view.endOfTextblock("up")) return;
    e.preventDefault();
    titleInput.focus();
    titleInput.setSelectionRange(titleInput.value.length, titleInput.value.length);
  }

  /** A click in the space below the last block continues the page there. */
  function tailDown(e: MouseEvent) {
    if (!editor || e.button !== 0) return;
    e.preventDefault();
    const { view } = editor;
    focusEnd(view.state, view.dispatch);
    view.focus();
  }

  onMount(() => {
    let closed = false;
    const stop = client.subscribe((event) => {
      if (!editor) return;
      if (event.type === "pageReloaded" && event.view.id === id) void editor.sync.reload(event.view);
      if (event.type === "pageChanged" && event.page === id) void editor.sync.reload();
    });
    (async () => {
      // A page another device just made can be in the page list before its own document
      // arrives; wait for it rather than fail.
      const deadline = Date.now() + 60_000;
      for (;;) {
        try {
          const view = await client.openPage(id);
          if (closed) {
            await client.closePage(id).catch(() => {});
            return;
          }
          arriving = false;
          editor = createEditor(mount, client, view, onerror, onselect);
          editorView = editor.view;
          if (view.title === "") titleInput.focus();
          else editor.view.focus();
          onopened();
          return;
        } catch (e) {
          if (closed) return;
          if (!(e instanceof CoreError && e.code === "unknownPage") || Date.now() > deadline) {
            arriving = false;
            onerror(e);
            return;
          }
          arriving = true;
          await new Promise((r) => setTimeout(r, 500));
        }
      }
    })();
    return () => {
      closed = true;
      stop();
      const e = editor;
      editor = null;
      editorView = null;
      void (async () => {
        if (renaming !== undefined) {
          clearTimeout(renaming);
          await client.renamePage(id, draft).catch((err) => {
            if (!(err instanceof CoreError && err.code === "unknownPage")) onerror(err);
          });
        }
        if (!e) return;
        await e.close();
        // Gone already (deleted, or given up for the account's notes) is fine.
        await client.closePage(id).catch((err) => {
          if (!(err instanceof CoreError && err.code === "unknownPage")) onerror(err);
        });
      })();
    };
  });
</script>

<article class="page" class:journal-page={info?.kind === "journal"}>
  <input
    bind:this={titleInput}
    class="page-title"
    placeholder="Untitled"
    aria-label="Page title"
    bind:value={draft}
    oninput={rename}
    onkeydown={titleKey}
  />
  {#if info}
    <Properties {client} page={info} {onerror} />
  {/if}
  {#if settings.journalParent === id}
    <JournalViews {pages} weekStart={settings.weekStart} {onday} onselect={(p) => onselect(p.id)} />
  {/if}
  {#if arriving}
    <p class="page-arriving">This page is still on its way from the server…</p>
  {/if}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="page-body"
    bind:this={mount}
    onkeydown={bodyKey}
    onfocusin={() => (focused = true)}
    onfocusout={() => (focused = false)}
  ></div>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="page-tail" onmousedown={tailDown}></div>
</article>
{#if touch && focused && editorView}
  <PhoneToolbar view={editorView} />
{/if}
