<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { MOODS } from "../journal/mood";
  import Icon from "./ui/Icon.svelte";

  let { score, onpick }: { score: number | null; onpick: (score: number | null) => void } = $props();
</script>

<div class="mood-picker" role="radiogroup" aria-label="Mood">
  {#each MOODS as m (m.score)}
    <button
      class="mood mood-{m.score}"
      class:picked={score === m.score}
      role="radio"
      aria-checked={score === m.score}
      aria-label={m.label}
      title={m.label}
      onclick={() => onpick(score === m.score ? null : m.score)}
    >
      <Icon name={m.icon} size={20} />
    </button>
  {/each}
</div>
