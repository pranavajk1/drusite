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

  let { view }: { view: EditorView } = $props();

  let keyboard = $state(0);

  onMount(() => {
    const vv = window.visualViewport;
    if (!vv) return;
    const place = () => {
      keyboard = Math.max(0, Math.round(window.innerHeight - vv.height - vv.offsetTop));
      document.documentElement.style.setProperty("--keyboard", `${keyboard}px`);
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

  function hide(e: PointerEvent) {
    e.preventDefault();
    view.dom.blur();
  }
</script>

<div class="phone-toolbar" class:keyboard-up={keyboard > 0} role="toolbar" aria-label="Formatting">
  <button aria-label="Heading" onpointerdown={run(toolbar.heading)}>H</button>
  <button aria-label="Bulleted list" onpointerdown={run(toolbar.toggleKind("bulleted"))}>•</button>
  <button aria-label="To-do" onpointerdown={run(toolbar.toggleKind("todo"))}>☐</button>
  <button aria-label="Outdent" onpointerdown={run(toolbar.outdent)}>⇤</button>
  <button aria-label="Indent" onpointerdown={run(toolbar.indent)}>⇥</button>
  <button aria-label="Bold" class="bold" onpointerdown={run(toolbar.bold)}>B</button>
  <button aria-label="Italic" class="italic" onpointerdown={run(toolbar.italic)}>I</button>
  <button aria-label="Undo" onpointerdown={run(toolbar.undo)}>↶</button>
  <button aria-label="Redo" onpointerdown={run(toolbar.redo)}>↷</button>
  <button aria-label="Hide keyboard" class="hide" onpointerdown={hide}>⌄</button>
</div>
