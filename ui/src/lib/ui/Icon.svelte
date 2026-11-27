<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- One stroke icon from the set in ../icons. Decorative: whatever holds it carries the label. -->
<script lang="ts">
  import { ICONS, type IconName } from "../icons/icons";

  let { name, size = 16, class: cls = "" }: { name: IconName; size?: number; class?: string } = $props();
</script>

<svg
  class="icon {cls}"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.75"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true">{@html ICONS[name]}</svg
>
