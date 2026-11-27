// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The device's Back (Android's button or gesture, a mouse's back button, the browser's) steps
// back through the app, as Notion's and Obsidian's apps do: it closes what's open over the page (a
// sheet, a menu, the pages drawer, a Settings section), and then goes back to the page before.
// Only with nothing left to go back to does Android's Back leave the app.
//
// Each page opened and each layer opened over it is an entry in the webview's history, so the
// platform's own Back reaches the app as a `popstate`. `entries` mirrors the entries this module
// pushed, above the one the app started on; each history entry's state is its depth and the page
// that was open.

type Entry = { kind: "page" } | { kind: "layer"; close: () => void; dead: boolean };

interface State {
  drusite: number;
  page: string | null;
}

const entries: Entry[] = [];
let page: string | null = null;
let show: (page: string) => void = () => {};
/** History moves this module made itself, whose `popstate` it then ignores. */
let own = 0;
/** What waits for those moves to land: a push made before then would land in the wrong place. */
let queued: (() => void)[] = [];
let started = false;
/** The move being made lands on a page to open, rather than staying on the one open. */
let follow = false;

function later(f: () => void) {
  if (own > 0) queued.push(f);
  else f();
}

function push() {
  history.pushState({ drusite: entries.length, page } satisfies State, "");
}

/** Starts at `first`; `onpage` opens a page Back (or Forward) went to. */
export function startHistory(first: string, onpage: (page: string) => void): () => void {
  page = first;
  show = onpage;
  entries.length = 0;
  own = 0;
  queued = [];
  history.replaceState({ drusite: 0, page } satisfies State, "");
  if (!started) {
    started = true;
    window.addEventListener("popstate", popped);
  }
  return () => {
    window.removeEventListener("popstate", popped);
    started = false;
  };
}

/** Another page is opened: Back comes back to this one. */
export function pushPage(id: string) {
  later(() => {
    if (!started || id === page) return;
    page = id;
    entries.push({ kind: "page" });
    push();
  });
}

/** The open page is swapped for another (the one open was deleted): Back doesn't return to it. */
export function replacePage(id: string) {
  later(() => {
    page = id;
    if (started) history.replaceState({ drusite: entries.length, page } satisfies State, "");
  });
}

/**
 * Something opens over the page; Back calls `close`. Returns what to call when it closes
 * another way (its own button, Escape), which takes its entry back off the history.
 */
export function openLayer(close: () => void): () => void {
  const entry: Entry = { kind: "layer", close, dead: false };
  later(() => {
    if (!started) return;
    entries.push(entry);
    push();
  });
  return () =>
    later(() => {
      const at = entries.indexOf(entry);
      if (at < 0) return;
      // Under something opened later (a page opened from Settings): taken off when Back gets there.
      if (at !== entries.length - 1) {
        entry.dead = true;
        return;
      }
      entries.pop();
      back(1 + deadOnTop());
    });
}

/** Takes the dead layers now on top off the mirror; how many there were. */
function deadOnTop(): number {
  let n = 0;
  for (let top = entries.at(-1); top?.kind === "layer" && top.dead; top = entries.at(-1)) {
    entries.pop();
    n++;
  }
  return n;
}

function back(n: number) {
  own++;
  history.go(-n);
}

function popped(e: PopStateEvent) {
  const state = e.state as Partial<State> | null;
  if (own > 0) {
    own--;
    // Where a layer closed the page is the one open now, whatever the entry under it had.
    if (own === 0) {
      if (follow) {
        follow = false;
        go(state);
      } else if (state && state.page !== page) {
        history.replaceState({ drusite: state.drusite ?? 0, page } satisfies State, "");
      }
      const run = queued;
      queued = [];
      run.forEach((f) => f());
    }
    return;
  }
  const at = typeof state?.drusite === "number" ? state.drusite : 0;
  if (at > entries.length) {
    // Forward, to an entry this module already took back: a closed layer can't reopen.
    own++;
    history.go(entries.length - at);
    return;
  }
  while (entries.length > at) {
    const top = entries.pop()!;
    if (top.kind === "layer" && !top.dead) top.close();
  }
  const dead = deadOnTop();
  if (dead > 0) {
    // Landed on a layer that already closed: carry on back to what was under it.
    follow = true;
    back(dead);
    return;
  }
  go(state);
}

function go(state: Partial<State> | null) {
  const to = state?.page;
  if (to && to !== page) {
    page = to;
    show(to);
  }
}
