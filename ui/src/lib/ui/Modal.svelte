<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  A dialog over the app. On desktops it's a centred card; on phones a sheet from the bottom
  (`sheet`) or the whole screen (`full`), as the Notion apps do. Escape and the backdrop close it,
  the focus goes into it, and comes back to where it was when it closes.
-->
<script lang="ts">
  import { onMount, type Snippet } from "svelte";

  import { openLayer } from "../back";
  import Icon from "./Icon.svelte";

  let {
    title,
    label,
    onclose,
    size = "md",
    phone = "sheet",
    head = true,
    class: cls = "",
    children,
  }: {
    /** Shown in the head, and names the dialog. */
    title?: string;
    /** The dialog's name when there's no visible title. */
    label?: string;
    onclose: () => void;
    size?: "sm" | "md" | "lg" | "palette";
    phone?: "sheet" | "full";
    head?: boolean;
    class?: string;
    children: Snippet;
  } = $props();

  const uid = `modal-${Math.random().toString(36).slice(2, 9)}`;
  let dialog: HTMLDivElement;

  onMount(() => {
    const before = document.activeElement as HTMLElement | null;
    if (!dialog.contains(document.activeElement)) dialog.focus({ preventScroll: true });
    // The device's Back closes it.
    const dismiss = openLayer(() => onclose());
    return () => {
      dismiss();
      before?.focus?.({ preventScroll: true });
    };
  });

  function key(e: KeyboardEvent) {
    if (e.key === "Escape" && !e.defaultPrevented) {
      e.stopPropagation();
      onclose();
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="modal-backdrop modal-{size}" onclick={onclose} onkeydown={key}>
  <div
    bind:this={dialog}
    class="modal modal-{size} phone-{phone} {cls}"
    role="dialog"
    aria-modal="true"
    aria-labelledby={title ? `${uid}-title` : undefined}
    aria-label={title ? undefined : label}
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
  >
    {#if head && title}
      <div class="modal-head">
        <span class="sheet-grabber" aria-hidden="true"></span>
        <h2 id="{uid}-title" class="modal-title">{title}</h2>
        <button class="icon-btn modal-close" aria-label="Close" onclick={onclose}><Icon name="x" size={18} /></button>
      </div>
    {/if}
    {@render children()}
  </div>
</div>
