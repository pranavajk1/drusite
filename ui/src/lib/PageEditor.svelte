<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { TextSelection } from "prosemirror-state";

  import type { CoreClient } from "../core/client";
  import type { PageId } from "../core/types";
  import { focusEnd } from "../editor/commands";
  import { type Editor, createEditor } from "../editor/editor";

  let {
    client,
    page,
    title,
    onopened,
    onerror,
  }: {
    client: CoreClient;
    page: PageId;
    title: string;
    onopened: () => void;
    onerror: (e: unknown) => void;
  } = $props();

  // This editor's page, fixed when it's made. The `page` prop is live: once the app switches to
  // another page, it already names that one while this editor is still saving and closing.
  // svelte-ignore state_referenced_locally
  const id = page;

  let mount: HTMLDivElement;
  let editor: Editor | null = null;
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
      try {
        const view = await client.openPage(id);
        if (closed) return;
        editor = createEditor(mount, client, view, onerror);
        if (view.title === "") titleInput.focus();
        else editor.view.focus();
        onopened();
      } catch (e) {
        onerror(e);
      }
    })();
    return () => {
      closed = true;
      stop();
      const e = editor;
      editor = null;
      void (async () => {
        if (renaming !== undefined) {
          clearTimeout(renaming);
          await client.renamePage(id, draft).catch(onerror);
        }
        await e?.close();
        await client.closePage(id).catch(onerror);
      })();
    };
  });
</script>

<article class="page">
  <input
    bind:this={titleInput}
    class="page-title"
    placeholder="Untitled"
    aria-label="Page title"
    bind:value={draft}
    oninput={rename}
    onkeydown={titleKey}
  />
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="page-body" bind:this={mount} onkeydown={bodyKey}></div>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="page-tail" onmousedown={tailDown}></div>
</article>
