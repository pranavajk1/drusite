<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { TextSelection } from "prosemirror-state";

  import type { CoreClient } from "../core/client";
  import type { PageId } from "../core/types";
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
        await client.renamePage(page, draft);
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

  onMount(() => {
    let closed = false;
    const stop = client.subscribe((event) => {
      if (!editor) return;
      if (event.type === "pageReloaded" && event.view.id === page) void editor.sync.reload(event.view);
      if (event.type === "pageChanged" && event.page === page) void editor.sync.reload();
    });
    (async () => {
      try {
        const view = await client.openPage(page);
        if (closed) return;
        editor = createEditor(mount, client, view, onerror);
        if (view.title === "") document.querySelector<HTMLInputElement>(".page-title")?.focus();
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
          await client.renamePage(page, draft).catch(onerror);
        }
        await e?.close();
        await client.closePage(page).catch(onerror);
      })();
    };
  });
</script>

<article class="page">
  <input
    class="page-title"
    placeholder="Untitled"
    aria-label="Page title"
    bind:value={draft}
    oninput={rename}
    onkeydown={titleKey}
  />
  <div class="page-body" bind:this={mount}></div>
</article>
