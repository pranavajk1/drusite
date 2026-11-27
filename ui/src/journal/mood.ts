// SPDX-License-Identifier: AGPL-3.0-or-later

// The journal's moods: the `Mood` property, a number from 1 to 5.

import type { PageInfo, PropertyValue } from "../core/types";

export const DATE = "Date";
export const MOOD = "Mood";

export interface MoodLevel {
  score: number;
  label: string;
  emoji: string;
}

export const MOODS: MoodLevel[] = [
  { score: 1, label: "Awful", emoji: "😣" },
  { score: 2, label: "Bad", emoji: "🙁" },
  { score: 3, label: "Okay", emoji: "😐" },
  { score: 4, label: "Good", emoji: "🙂" },
  { score: 5, label: "Great", emoji: "😄" },
];

export function property(page: PageInfo, name: string): PropertyValue | undefined {
  return page.properties.find((p) => p.name === name)?.value;
}

/** A page's mood, rounded into 1–5, if it has one. */
export function moodOf(page: PageInfo): MoodLevel | undefined {
  const v = property(page, MOOD);
  if (v?.type !== "number" || v.value === null || !Number.isFinite(v.value)) return undefined;
  return MOODS[Math.min(5, Math.max(1, Math.round(v.value))) - 1];
}

export function dateOfEntry(page: PageInfo): string | undefined {
  const v = property(page, DATE);
  return v?.type === "date" ? v.value : undefined;
}
