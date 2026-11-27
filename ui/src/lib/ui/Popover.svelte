<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  A popover with content of its own (a cell's editor, a property's form), opened from `anchor`.
  Escape or a click outside closes it. On phones it's a sheet from the bottom, as Notion's are.
-->
<script lang="ts">
  import { onMount, tick, type Snippet } from "svelte";

  import { dismissable, placeBelow, portal, type Place } from "./floating";

  let {
    anchor,
    label,
    onclose,
    width = 280,
    children,
  }: {
    anchor: HTMLElement;
    label: string;
    onclose: () => void;
    width?: number;
    children: Snippet;
  } = $props();

  let box: HTMLElement;
  let place = $state<Place>({ top: 0, left: 0 });
  // svelte-ignore state_referenced_locally
  const from = anchor;
  const phone = window.matchMedia?.("(max-width: 700px)").matches ?? false;

  function position() {
    if (!box || phone) return;
    place = placeBelow(from.getBoundingClientRect(), box.getBoundingClientRect());
  }

  onMount(() => {
    void tick().then(() => {
      position();
      const first = box?.querySelector<HTMLElement>("input, textarea, button, [tabindex]");
      first?.focus({ preventScroll: true });
    });
    // A select's list or a menu opened from inside it is outside it in the DOM (portalled), but
    // a click there is still a click inside.
    const undo = dismissable(() => [box, from, ...document.querySelectorAll(".popover:not(.db-popover)")], onclose);
    const observer = new ResizeObserver(position);
    if (box) observer.observe(box);
    return () => {
      undo();
      observer.disconnect();
    };
  });
</script>

{#if phone}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div use:portal class="sheet-backdrop" onclick={onclose}></div>
{/if}
<div
  use:portal
  bind:this={box}
  class="popover db-popover"
  class:db-sheet={phone}
  role="dialog"
  aria-label={label}
  tabindex="-1"
  style:top={phone ? null : `${place.top}px`}
  style:left={phone ? null : `${place.left}px`}
  style:width={phone ? null : `${width}px`}
>
  {@render children()}
</div>
