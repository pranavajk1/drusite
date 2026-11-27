// SPDX-License-Identifier: AGPL-3.0-or-later

// Placing a popover (a menu, a select's list) next to what opened it, inside the window.

export interface Place {
  top: number;
  left: number;
  /** The popover's minimum width: its anchor's, for a select. */
  minWidth?: number;
}

const GAP = 4;
const EDGE = 8;

/**
 * The bottom of what's visible: above the on-screen keyboard (which on phones covers the window
 * rather than shrinking it) and above the phone's formatting bar.
 */
export function visibleBottom(): number {
  const vv = window.visualViewport;
  let bottom = vv ? vv.offsetTop + vv.height : window.innerHeight;
  const bar = document.querySelector(".phone-toolbar")?.getBoundingClientRect();
  if (bar && bar.height > 0) bottom = Math.min(bottom, bar.top);
  return bottom;
}

/** The top of what's visible: iOS moves the visual viewport down when the keyboard opens. */
export function visibleTop(): number {
  return window.visualViewport?.offsetTop ?? 0;
}

/** Below `anchor` (above if there's no room), left edges lined up, kept on screen. */
export function placeBelow(anchor: DOMRect, size: { width: number; height: number }, matchWidth = false): Place {
  const vw = window.innerWidth;
  const vh = visibleBottom();
  let top = anchor.bottom + GAP;
  const vt = visibleTop() + EDGE;
  if (top + size.height > vh - EDGE && anchor.top - GAP - size.height > vt) top = anchor.top - GAP - size.height;
  top = Math.max(vt, Math.min(top, vh - EDGE - size.height));
  const left = Math.max(EDGE, Math.min(anchor.left, vw - EDGE - size.width));
  return { top, left, minWidth: matchWidth ? anchor.width : undefined };
}

/** Closes something when a pointer goes down outside `nodes`, or on Escape. Returns the undo. */
export function dismissable(nodes: () => (Element | null | undefined)[], close: () => void): () => void {
  const down = (e: PointerEvent) => {
    if (!nodes().some((n) => n?.contains(e.target as Node))) close();
  };
  const key = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      close();
    }
  };
  document.addEventListener("pointerdown", down, true);
  document.addEventListener("keydown", key, true);
  return () => {
    document.removeEventListener("pointerdown", down, true);
    document.removeEventListener("keydown", key, true);
  };
}

/** A Svelte action: moves the element to <body>, so no transformed or clipping ancestor holds it. */
export function portal(node: HTMLElement) {
  document.body.append(node);
  return { destroy: () => node.remove() };
}
