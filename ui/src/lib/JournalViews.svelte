<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { PageInfo } from "../core/types";
  import { MONTHS, WEEKDAYS, addDays, dateOf, monthGrid, today } from "../journal/dates";
  import { MOODS, dateOfEntry, moodOf } from "../journal/mood";
  import { ICONS } from "./icons/icons";
  import Icon from "./ui/Icon.svelte";
  import Select from "./ui/Select.svelte";

  let {
    pages,
    weekStart,
    onday,
    onselect,
  }: {
    pages: PageInfo[];
    weekStart: number;
    /** Opens a day's entry, making it if there isn't one. */
    onday: (date: string) => void;
    onselect: (page: PageInfo) => void;
  } = $props();

  const now = today();
  let year = $state(dateOf(now).getFullYear());
  let month = $state(dateOf(now).getMonth());
  let range = $state(30);

  /** Each day's entry (the oldest, when two devices made one each). */
  const byDay = $derived.by(() => {
    const map = new Map<string, PageInfo>();
    for (const p of pages) {
      if (p.kind !== "journal") continue;
      const d = dateOfEntry(p);
      if (!d) continue;
      const have = map.get(d);
      if (!have || p.id < have.id) map.set(d, p);
    }
    return map;
  });

  const weeks = $derived(monthGrid(year, month, weekStart));
  const heads = $derived(Array.from({ length: 7 }, (_, i) => WEEKDAYS[(i + weekStart) % 7]!.slice(0, 3)));

  function step(n: number) {
    const d = new Date(year, month + n, 1);
    year = d.getFullYear();
    month = d.getMonth();
  }

  /** Entries on today's month and day in earlier years, newest first. */
  const onThisDay = $derived.by(() => {
    const md = now.slice(5);
    return [...byDay.entries()]
      .filter(([d]) => d.slice(5) === md && d < now)
      .sort(([a], [b]) => (a < b ? 1 : -1))
      .map(([d, p]) => ({ day: d, page: p, years: Number(now.slice(0, 4)) - Number(d.slice(0, 4)) }));
  });

  // Mood over time: one point per day with a mood, over the last `range` days.
  const W = 600;
  const H = 120;
  const points = $derived.by(() => {
    const start = addDays(now, -(range - 1));
    const out: { x: number; y: number; day: string; score: number }[] = [];
    for (const [d, p] of byDay) {
      const mood = moodOf(p);
      if (!mood || d < start || d > now) continue;
      const i = Math.round((dateOf(d).getTime() - dateOf(start).getTime()) / 86_400_000);
      out.push({ x: (i / Math.max(1, range - 1)) * W, y: H - ((mood.score - 1) / 4) * H, day: d, score: mood.score });
    }
    return out.sort((a, b) => a.x - b.x);
  });
  const average = $derived(points.length ? points.reduce((s, p) => s + p.score, 0) / points.length : null);
</script>

<section class="journal-views" aria-label="Journal">
  <div class="jv-calendar">
    <div class="jv-head">
      <h3>{MONTHS[month]} {year}</h3>
      <div class="jv-nav">
        <button class="btn small ghost" aria-label="Go to this month" onclick={() => {
          const d = dateOf(now);
          year = d.getFullYear();
          month = d.getMonth();
        }}>Today</button>
        <button class="icon-btn" aria-label="Previous month" onclick={() => step(-1)}><Icon name="chevron-left" /></button>
        <button class="icon-btn" aria-label="Next month" onclick={() => step(1)}><Icon name="chevron-right" /></button>
      </div>
    </div>
    <table>
      <thead>
        <tr>{#each heads as h (h)}<th scope="col">{h}</th>{/each}</tr>
      </thead>
      <tbody>
        {#each weeks as week, w (w)}
          <tr>
            {#each week as day, i (i)}
              <td>
                {#if day}
                  {@const entry = byDay.get(day)}
                  {@const mood = entry ? moodOf(entry) : undefined}
                  <button
                    class="jv-day {mood ? `mood-${mood.score}` : ''}"
                    class:has-entry={entry}
                    class:today={day === now}
                    disabled={day > now && !entry}
                    aria-label="{dateOf(day).toLocaleDateString(undefined, { dateStyle: 'full' })}{entry ? ', has an entry' : ''}{mood ? `, ${mood.label}` : ''}"
                    onclick={() => onday(day)}
                  >
                    {Number(day.slice(8))}
                  </button>
                {/if}
              </td>
            {/each}
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

  <div class="jv-side">
    <div class="jv-head"><h3>On this day</h3></div>
    {#if onThisDay.length === 0}
      <p class="hint jv-empty">Nothing from this day in earlier years yet.</p>
    {:else}
      <ul class="jv-list">
        {#each onThisDay as e (e.page.id)}
          {@const mood = moodOf(e.page)}
          <li>
            <button onclick={() => onselect(e.page)}>
              <Icon name="calendar-days" />
              <span class="jv-entry">
                <span class="jv-entry-title">{e.page.title || "Untitled"}</span>
                <span class="jv-ago">{e.years} {e.years === 1 ? "year" : "years"} ago</span>
              </span>
              {#if mood}<span class="jv-entry-mood mood-{mood.score}" role="img" aria-label={mood.label}><Icon name={mood.icon} /></span>{/if}
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </div>

  <div class="jv-mood">
    <div class="jv-head">
      <h3>Mood over time</h3>
      <Select
        label="Range"
        value={range}
        options={[
          { value: 30, label: "30 days" },
          { value: 90, label: "90 days" },
          { value: 365, label: "A year" },
        ]}
        onchange={(r) => (range = r)}
      />
    </div>
    {#if points.length === 0}
      <p class="hint">Pick a mood on your entries to see it here.</p>
    {:else}
      <svg class="jv-chart" viewBox="-30 -8 {W + 38} {H + 16}" role="img" aria-label="Mood over the last {range} days, averaging {average?.toFixed(1)} out of 5">
        {#each MOODS as m (m.score)}
          <line class="jv-grid" x1="0" x2={W} y1={H - ((m.score - 1) / 4) * H} y2={H - ((m.score - 1) / 4) * H} />
          <svg
            class="jv-axis mood-{m.score}"
            x="-26"
            y={H - ((m.score - 1) / 4) * H - 8}
            width="16"
            height="16"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.75"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true">{@html ICONS[m.icon]}</svg
          >
        {/each}
        <polyline class="jv-line" points={points.map((p) => `${p.x},${p.y}`).join(" ")} />
        {#each points as p (p.day)}
          <circle class="jv-dot mood-{p.score}" cx={p.x} cy={p.y} r="4"><title>{p.day}</title></circle>
        {/each}
      </svg>
      <p class="hint">{points.length} {points.length === 1 ? "day" : "days"} with a mood · average {average?.toFixed(1)} of 5</p>
    {/if}
  </div>
</section>
