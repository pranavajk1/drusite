// SPDX-License-Identifier: AGPL-3.0-or-later

// The journal's moods: the `Mood` property, a number from 1 to 5.

import type { PageInfo, PropertyValue } from "../core/types";
import type { IconName } from "../lib/icons/icons";

export const DATE = "Date";
export const MOOD = "Mood";

export interface MoodLevel {
  score: number;
  label: string;
  icon: IconName;
}

export const MOODS: MoodLevel[] = [
  { score: 1, label: "Awful", icon: "angry" },
  { score: 2, label: "Bad", icon: "frown" },
  { score: 3, label: "Okay", icon: "meh" },
  { score: 4, label: "Good", icon: "smile" },
  { score: 5, label: "Great", icon: "laugh" },
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
