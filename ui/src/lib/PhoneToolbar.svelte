<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  The formatting bar a phone shows above its keyboard, since a phone has no shortcuts. It sits
  on top of the on-screen keyboard: iOS keeps the layout viewport when the keyboard opens and
  shrinks only the visual viewport, so the bar follows `visualViewport`; Android resizes the
  window, and the same sum comes out at zero.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import type { Command } from "prosemirror-state";
  import type { EditorView } from "prosemirror-view";

  import { toolbar } from "../editor/commands";
  import Icon from "./ui/Icon.svelte";

  let { view }: { view: EditorView } = $props();

  let keyboard = $state(0);

  onMount(() => {
    const vv = window.visualViewport;
    if (!vv) return;
    const place = () => {
      keyboard = Math.max(0, Math.round(window.innerHeight - vv.height - vv.offsetTop));
      document.documentElement.style.setProperty("--keyboard", `${keyboard}px`);
      // Where the keyboard covers the window rather than shrinking it (Android 15 and later draw
      // edge to edge), the editor has to keep the cursor above the keyboard and this bar itself.
      const bar = 64;
      view.setProps({ scrollMargin: { top: 64, left: 5, right: 5, bottom: keyboard + bar } });
      if (keyboard > 0 && view.hasFocus()) {
        const caret = view.coordsAtPos(view.state.selection.head);
        const limit = vv.offsetTop + vv.height - bar;
        if (caret.bottom > limit) window.scrollBy({ top: caret.bottom - limit + 8 });
      }
    };
    place();
    vv.addEventListener("resize", place);
    vv.addEventListener("scroll", place);
    return () => {
      vv.removeEventListener("resize", place);
      vv.removeEventListener("scroll", place);
      document.documentElement.style.removeProperty("--keyboard");
    };
  });

  // pointerdown, not click: a click would take the focus from the editor and close the keyboard.
  function run(command: Command) {
    return (e: PointerEvent) => {
      e.preventDefault();
      command(view.state, view.dispatch, view);
      view.focus();
    };
  }

  /** Types a `/`, after a space if needed, which opens the block menu. */
  function blocks(e: PointerEvent) {
    e.preventDefault();
    const { state } = view;
    const at = state.selection.$from;
    const before = at.parent.textBetween(Math.max(0, at.parentOffset - 1), at.parentOffset);
    view.dispatch(state.tr.insertText(before === "" || /\s/.test(before) ? "/" : " /"));
    view.focus();
  }

  function hide(e: PointerEvent) {
    e.preventDefault();
    view.dom.blur();
  }
</script>

<div class="phone-toolbar" class:keyboard-up={keyboard > 0} role="toolbar" aria-label="Formatting">
  <button aria-label="Add a block" onpointerdown={blocks}><Icon name="plus" size={20} /></button>
  <span class="tb-sep"></span>
  <button aria-label="Heading" onpointerdown={run(toolbar.heading)}><Icon name="heading" size={20} /></button>
  <button aria-label="Bulleted list" onpointerdown={run(toolbar.toggleKind("bulleted"))}><Icon name="list" size={20} /></button>
  <button aria-label="To-do" onpointerdown={run(toolbar.toggleKind("todo"))}><Icon name="square-check" size={20} /></button>
  <button aria-label="Bold" onpointerdown={run(toolbar.bold)}><Icon name="bold" size={20} /></button>
  <button aria-label="Italic" onpointerdown={run(toolbar.italic)}><Icon name="italic" size={20} /></button>
  <button aria-label="Outdent" onpointerdown={run(toolbar.outdent)}><Icon name="list-indent-decrease" size={20} /></button>
  <button aria-label="Indent" onpointerdown={run(toolbar.indent)}><Icon name="list-indent-increase" size={20} /></button>
  <span class="tb-sep"></span>
  <button aria-label="Undo" onpointerdown={run(toolbar.undo)}><Icon name="undo-2" size={20} /></button>
  <button aria-label="Redo" onpointerdown={run(toolbar.redo)}><Icon name="redo-2" size={20} /></button>
  <button aria-label="Hide keyboard" class="hide" onpointerdown={hide}><Icon name="keyboard-off" size={20} /></button>
</div>
