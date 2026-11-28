<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  A panel at the right of the page, beside it rather than over it (desktops and the browser):
  its left edge drags to resize it, and the width is the caller's to keep. On phones there's no
  room beside the page, so the panel's contents are laid out by themselves (full screen).

  Generic: Ask is its first panel; whatever else goes beside the page later goes in one of these.
-->
<script lang="ts">
  import type { Snippet } from "svelte";

  let {
    label,
    width,
    min = 300,
    max = 720,
    onwidth,
    children,
  }: {
    /** What the panel holds, for the resize handle's name. */
    label: string;
    /** The width it was last left at. */
    width: number;
    min?: number;
    max?: number;
    /** A new width, once a drag (or a key) has finished changing it. */
    onwidth: (width: number) => void;
    children: Snippet;
  } = $props();

  const clamp = (w: number) => Math.round(Math.max(min, Math.min(max, w)));
  let dragged = $state<number | null>(null);
  const shown = $derived(dragged ?? clamp(width));

  /** Dragging the left edge: left makes it wider. */
  function start(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    const startX = e.clientX;
    const startW = shown;
    dragged = startW;
    document.documentElement.classList.add("panel-resizing");
    const move = (ev: PointerEvent) => (dragged = clamp(startW - (ev.clientX - startX)));
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      document.documentElement.classList.remove("panel-resizing");
      if (dragged !== null && dragged !== startW) onwidth(dragged);
      dragged = null;
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  }

  function key(e: KeyboardEvent) {
    if (e.key === "ArrowLeft") onwidth(clamp(shown + 16));
    else if (e.key === "ArrowRight") onwidth(clamp(shown - 16));
    else return;
    e.preventDefault();
  }
</script>

<aside class="right-panel" aria-label={label} style:--right-width="{shown}px">
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div
    class="right-panel-resize"
    role="separator"
    aria-orientation="vertical"
    aria-label="Resize {label}"
    aria-valuemin={min}
    aria-valuemax={max}
    aria-valuenow={shown}
    tabindex="0"
    title="Drag to resize"
    onpointerdown={start}
    onkeydown={key}
  ></div>
  {@render children()}
</aside>
