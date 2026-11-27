// SPDX-License-Identifier: AGPL-3.0-or-later

// Days as the journal sees them: `YYYY-MM-DD` strings in the person's own time zone. The core
// never guesses a time zone; the UI says which day it is.

const pad = (n: number, width = 2) => String(n).padStart(width, "0");

/** The day `date` falls on, on this device. */
export function dayOf(date: Date): string {
  return `${pad(date.getFullYear(), 4)}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

export function today(): string {
  return dayOf(new Date());
}

/** The local midnight starting `day`. */
export function dateOf(day: string): Date {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(y!, m! - 1, d!);
}

export function addDays(day: string, n: number): string {
  const d = dateOf(day);
  d.setDate(d.getDate() + n);
  return dayOf(d);
}

export const WEEKDAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
export const MONTHS = [
  "January",
  "February",
  "March",
  "April",
  "May",
  "June",
  "July",
  "August",
  "September",
  "October",
  "November",
  "December",
];

/** The title the core gives a new entry, e.g. "Thursday, 26 November 2026". */
export function dayTitle(day: string): string {
  const d = dateOf(day);
  return `${WEEKDAYS[d.getDay()]}, ${d.getDate()} ${MONTHS[d.getMonth()]} ${d.getFullYear()}`;
}

/**
 * The weeks of a month as rows of seven days, starting on `weekStart` (0 Sunday, 1 Monday), with
 * `null` for the days before the 1st and after the last.
 */
export function monthGrid(year: number, month: number, weekStart: number): (string | null)[][] {
  const first = new Date(year, month, 1);
  const days = new Date(year, month + 1, 0).getDate();
  const lead = (first.getDay() - weekStart + 7) % 7;
  const cells: (string | null)[] = Array.from({ length: lead }, () => null);
  for (let d = 1; d <= days; d++) cells.push(dayOf(new Date(year, month, d)));
  while (cells.length % 7 !== 0) cells.push(null);
  const weeks: (string | null)[][] = [];
  for (let i = 0; i < cells.length; i += 7) weeks.push(cells.slice(i, i + 7));
  return weeks;
}
