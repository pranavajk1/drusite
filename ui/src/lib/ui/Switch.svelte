<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  let {
    checked,
    onchange,
    label,
    id,
    disabled = false,
  }: { checked: boolean; onchange: (checked: boolean) => void; label?: string; id?: string; disabled?: boolean } = $props();
</script>

<button
  {id}
  class="switch"
  type="button"
  role="switch"
  aria-checked={checked}
  aria-label={label}
  {disabled}
  onclick={() => onchange(!checked)}
></button>
