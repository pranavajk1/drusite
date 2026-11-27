<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  A select drawn by the app: a button showing the choice, and a listbox popover. Keyboard as a
  native select: arrows, Home/End, Enter or Space to pick, Escape to close, a letter to jump.
-->
<script lang="ts" generics="T extends string | number">
  import { tick } from "svelte";

  import Icon from "./Icon.svelte";
  import { dismissable, placeBelow, portal, type Place } from "./floating";

  let {
    value,
    options,
    onchange,
    id,
    label,
    boxed = false,
    disabled = false,
  }: {
    value: T;
    options: { value: T; label: string }[];
    onchange: (value: T) => void;
    id?: string;
    /** The accessible name, when no <label for={id}> gives one. */
    label?: string;
    /** Drawn as a form field (Settings), not a quiet inline control. */
    boxed?: boolean;
    disabled?: boolean;
  } = $props();

  const uid = `select-${Math.random().toString(36).slice(2, 9)}`;
  let button: HTMLButtonElement;
  let list = $state<HTMLElement | null>(null);
  let open = $state(false);
  let active = $state(0);
  let place = $state<Place>({ top: 0, left: 0 });

  const current = $derived(options.find((o) => o.value === value));

  async function show() {
    if (disabled) return;
    open = true;
    active = Math.max(0, options.findIndex((o) => o.value === value));
    await tick();
    if (!list) return;
    place = placeBelow(button.getBoundingClientRect(), list.getBoundingClientRect(), true);
    list.focus();
    list.querySelector(".active")?.scrollIntoView({ block: "nearest" });
  }

  function hide(refocus = true) {
    open = false;
    if (refocus) button.focus();
  }

  $effect(() => {
    if (!open) return;
    return dismissable(() => [button, list], () => hide(false));
  });

  function pick(i: number) {
    const o = options[i];
    if (!o) return;
    hide();
    if (o.value !== value) onchange(o.value);
  }

  function key(e: KeyboardEvent) {
    const last = options.length - 1;
    if (e.key === "ArrowDown") active = Math.min(last, active + 1);
    else if (e.key === "ArrowUp") active = Math.max(0, active - 1);
    else if (e.key === "Home") active = 0;
    else if (e.key === "End") active = last;
    else if (e.key === "Enter" || e.key === " ") pick(active);
    else if (e.key === "Tab") return hide(false);
    else if (e.key.length === 1) {
      const k = e.key.toLowerCase();
      const from = options.findIndex((o, i) => i > active && o.label.toLowerCase().startsWith(k));
      const at = from >= 0 ? from : options.findIndex((o) => o.label.toLowerCase().startsWith(k));
      if (at >= 0) active = at;
    } else return;
    e.preventDefault();
    list?.querySelector(".active")?.scrollIntoView({ block: "nearest" });
  }

  function buttonKey(e: KeyboardEvent) {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      void show();
    }
  }
</script>

<button
  bind:this={button}
  {id}
  type="button"
  class="select"
  class:boxed
  aria-haspopup="listbox"
  aria-expanded={open}
  aria-controls={open ? uid : undefined}
  aria-label={label}
  {disabled}
  onclick={() => (open ? hide() : show())}
  onkeydown={buttonKey}
>
  <span class="select-value">{current?.label ?? ""}</span>
  <Icon name="chevron-down" size={14} />
</button>

{#if open}
  <div
    use:portal
    bind:this={list}
    id={uid}
    class="popover"
    role="listbox"
    tabindex="-1"
    aria-label={label}
    aria-activedescendant="{uid}-{active}"
    style:top="{place.top}px"
    style:left="{place.left}px"
    style:min-width={place.minWidth ? `${place.minWidth}px` : undefined}
    onkeydown={key}
  >
    {#each options as o, i (o.value)}
      <div
        id="{uid}-{i}"
        class="menu-item"
        class:active={i === active}
        role="option"
        aria-selected={o.value === value}
        tabindex="-1"
        onpointerenter={() => (active = i)}
        onclick={() => pick(i)}
        onkeydown={() => {}}
      >
        <span class="menu-item-label">{o.label}</span>
        {#if o.value === value}<span class="menu-item-check"><Icon name="check" size={14} /></span>{/if}
      </div>
    {/each}
  </div>
{/if}
