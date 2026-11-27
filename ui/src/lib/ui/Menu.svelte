<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- A popover menu of actions, opened from `anchor`. Arrow keys move, Enter runs, Escape closes. -->
<script lang="ts" module>
  import type { IconName } from "../icons/icons";

  export interface MenuItem {
    label: string;
    icon?: IconName;
    danger?: boolean;
    hint?: string;
    /** A line above it, starting a new group. */
    group?: boolean;
    run: () => void;
  }
</script>

<script lang="ts">
  import { onMount, tick, type Snippet } from "svelte";

  import Icon from "./Icon.svelte";
  import { dismissable, placeBelow, portal, type Place } from "./floating";

  let {
    anchor,
    items,
    label,
    onclose,
    header,
    wide = false,
  }: {
    anchor: HTMLElement;
    items: MenuItem[];
    label: string;
    onclose: () => void;
    /** Shown above the items, as the account menu's who-and-where. */
    header?: Snippet;
    /** At least as wide as the anchor. */
    wide?: boolean;
  } = $props();

  let menu: HTMLElement;
  let active = $state(0);
  let place = $state<Place>({ top: 0, left: 0 });

  // The anchor as it was when the menu opened: the prop can go away once the menu closes.
  // svelte-ignore state_referenced_locally
  const from = anchor;

  onMount(() => {
    let open = true;
    void tick().then(() => {
      if (!open || !menu) return;
      place = placeBelow(from.getBoundingClientRect(), menu.getBoundingClientRect(), wide);
      menu.focus();
    });
    const undo = dismissable(() => [menu, from], close);
    return () => {
      open = false;
      undo();
    };
  });

  function close() {
    onclose();
    from.focus();
  }

  function run(i: number) {
    const item = items[i];
    if (!item) return;
    onclose();
    item.run();
  }

  function key(e: KeyboardEvent) {
    if (e.key === "ArrowDown") active = (active + 1) % items.length;
    else if (e.key === "ArrowUp") active = (active - 1 + items.length) % items.length;
    else if (e.key === "Enter" || e.key === " ") run(active);
    else if (e.key === "Tab") close();
    else return;
    e.preventDefault();
  }
</script>

<div
  use:portal
  bind:this={menu}
  class="popover"
  role="menu"
  aria-label={label}
  tabindex="-1"
  style:top="{place.top}px"
  style:left="{place.left}px"
  style:min-width={place.minWidth ? `${place.minWidth}px` : null}
  onkeydown={key}
>
  {#if header}{@render header()}<div class="menu-sep" role="separator"></div>{/if}
  {#each items as item, i (item.label)}
    {#if item.group && i > 0}<div class="menu-sep" role="separator"></div>{/if}
    <button
      class="menu-item"
      class:active={i === active}
      class:danger={item.danger}
      role="menuitem"
      tabindex="-1"
      onpointerenter={() => (active = i)}
      onclick={() => run(i)}
    >
      {#if item.icon}<Icon name={item.icon} />{/if}
      <span class="menu-item-label">{item.label}</span>
      {#if item.hint}<kbd>{item.hint}</kbd>{/if}
    </button>
  {/each}
</div>
