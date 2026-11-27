// SPDX-License-Identifier: AGPL-3.0-or-later

// Dragging cards (a board's, a calendar's) with the pointer, so it works with a mouse, a pen and
// a finger alike. A drag starts after the pointer moves a few pixels (or, on touch, after a short
// press, so a swipe still scrolls); a ghost of the card follows it; dropping asks `ondrop` with
// the target under the pointer: the nearest `[data-drop]` element, and the card it lands before.

export interface Drop {
  /** The `data-drop` value of the target (a group's key, a day). */
  target: string;
  /** The card it lands before in that target (its `data-card`), or null for the end. */
  before: string | null;
}

const MOVE = 4;
const PRESS_MS = 220;
/** How close to a scroller's edge a held card makes it scroll, and how fast at the very edge. */
const EDGE = 48;
const SPEED = 18;

/** The nearest ancestor that scrolls along `axis`: a board's columns, a long page. */
function scrollerOf(node: HTMLElement, axis: "x" | "y"): HTMLElement | null {
  for (let el = node.parentElement; el; el = el.parentElement) {
    const style = getComputedStyle(el);
    const overflow = axis === "x" ? style.overflowX : style.overflowY;
    const room = axis === "x" ? el.scrollWidth > el.clientWidth : el.scrollHeight > el.clientHeight;
    if (room && (overflow === "auto" || overflow === "scroll")) return el;
  }
  return null;
}

/** How far to scroll this frame for a pointer at `at` between `low` and `high`: none outside the
 *  edges, more the closer it gets (a phone's board column fills the screen, so its neighbour is
 *  only reached by holding a card at the edge). */
function step(at: number, low: number, high: number): number {
  if (at < low + EDGE) return -Math.ceil(((low + EDGE - at) / EDGE) * SPEED);
  if (at > high - EDGE) return Math.ceil(((at - (high - EDGE)) / EDGE) * SPEED);
  return 0;
}

export function draggable(node: HTMLElement, params: { id: string; ondrop: (id: string, drop: Drop) => void }) {
  let p = params;
  let start: { x: number; y: number } | null = null;
  let ghost: HTMLElement | null = null;
  let pressTimer: ReturnType<typeof setTimeout> | undefined;
  let armed = false;
  let over: HTMLElement | null = null;
  let last = { x: 0, y: 0 };
  let frame = 0;
  let scrollers: { x: HTMLElement | null; y: HTMLElement | null } = { x: null, y: null };

  function targetAt(x: number, y: number): { el: HTMLElement; before: string | null } | null {
    const hit = document.elementsFromPoint(x, y).find((e) => e instanceof HTMLElement && e.dataset.drop !== undefined) as HTMLElement | undefined;
    if (!hit) return null;
    const cards = [...hit.querySelectorAll<HTMLElement>("[data-card]")].filter((c) => c !== node);
    const before = cards.find((c) => {
      const r = c.getBoundingClientRect();
      return y < r.top + r.height / 2;
    });
    return { el: hit, before: before?.dataset.card ?? null };
  }

  function begin(x: number, y: number) {
    const r = node.getBoundingClientRect();
    ghost = node.cloneNode(true) as HTMLElement;
    ghost.classList.add("drag-ghost");
    ghost.style.width = `${r.width}px`;
    ghost.style.left = `${r.left}px`;
    ghost.style.top = `${r.top}px`;
    document.body.append(ghost);
    node.classList.add("dragging");
    start = { x: x - r.left, y: y - r.top };
    scrollers = { x: scrollerOf(node, "x"), y: scrollerOf(node, "y") };
    // A phone's board snaps to its columns, which would undo each small step of autoscroll; it
    // snaps again once the card is dropped.
    for (const el of [scrollers.x, scrollers.y]) if (el) el.style.scrollSnapType = "none";
  }

  function unsnap() {
    for (const el of [scrollers.x, scrollers.y]) if (el) el.style.scrollSnapType = "";
    scrollers = { x: null, y: null };
  }

  function hover(x: number, y: number) {
    const t = targetAt(x, y);
    if (over !== t?.el) {
      over?.classList.remove("drop-over");
      over = t?.el ?? null;
      over?.classList.add("drop-over");
    }
  }

  // Held near an edge, the board (or page) scrolls under the card, frame by frame.
  function autoscroll() {
    frame = 0;
    if (!ghost) return;
    let moved = false;
    const { x: sx, y: sy } = scrollers;
    if (sx) {
      const r = sx.getBoundingClientRect();
      const dx = step(last.x, Math.max(r.left, 0), Math.min(r.right, window.innerWidth));
      if (dx) {
        const before = sx.scrollLeft;
        sx.scrollLeft += dx;
        moved ||= sx.scrollLeft !== before;
      }
    }
    if (sy) {
      const r = sy.getBoundingClientRect();
      const dy = step(last.y, Math.max(r.top, 0), Math.min(r.bottom, window.innerHeight));
      if (dy) {
        const before = sy.scrollTop;
        sy.scrollTop += dy;
        moved ||= sy.scrollTop !== before;
      }
    }
    if (moved) {
      hover(last.x, last.y);
      frame = requestAnimationFrame(autoscroll);
    }
  }

  function move(e: PointerEvent) {
    if (!ghost && start && armed) {
      const dx = Math.abs(e.clientX - (start.x as number));
      const dy = Math.abs(e.clientY - (start.y as number));
      if (e.pointerType !== "touch" && dx + dy > MOVE) begin(e.clientX, e.clientY);
    }
    if (!ghost || !start) return;
    e.preventDefault();
    ghost.style.left = `${e.clientX - start.x}px`;
    ghost.style.top = `${e.clientY - start.y}px`;
    last = { x: e.clientX, y: e.clientY };
    hover(e.clientX, e.clientY);
    if (!frame) frame = requestAnimationFrame(autoscroll);
  }

  function end(e: PointerEvent) {
    clearTimeout(pressTimer);
    cancelAnimationFrame(frame);
    frame = 0;
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", end);
    window.removeEventListener("pointercancel", end);
    armed = false;
    over?.classList.remove("drop-over");
    over = null;
    if (!ghost) {
      start = null;
      return;
    }
    ghost.remove();
    ghost = null;
    // Where it landed, before the board snaps back to a column and moves under the pointer.
    const t = e.type === "pointercancel" ? null : targetAt(e.clientX, e.clientY);
    unsnap();
    node.classList.remove("dragging");
    start = null;
    // The click that follows a drag isn't a click.
    const swallow = (ev: Event) => {
      ev.stopPropagation();
      ev.preventDefault();
    };
    window.addEventListener("click", swallow, { capture: true, once: true });
    setTimeout(() => window.removeEventListener("click", swallow, { capture: true }), 0);
    if (t) p.ondrop(p.id, { target: t.el.dataset.drop!, before: t.before });
  }

  function down(e: PointerEvent) {
    if (e.button !== 0 || (e.target as HTMLElement).closest("input, button, a, textarea")) return;
    armed = true;
    start = { x: e.clientX, y: e.clientY };
    window.addEventListener("pointermove", move, { passive: false });
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
    if (e.pointerType === "touch") {
      // A press and hold picks the card up; a swipe before then scrolls as usual.
      pressTimer = setTimeout(() => {
        if (armed) begin(e.clientX, e.clientY);
      }, PRESS_MS);
    }
  }

  // On touch, once a card is picked up the page mustn't scroll under the finger.
  function touchMove(e: TouchEvent) {
    if (ghost) e.preventDefault();
  }

  node.addEventListener("pointerdown", down);
  node.addEventListener("touchmove", touchMove, { passive: false });
  return {
    update(next: typeof params) {
      p = next;
    },
    destroy() {
      node.removeEventListener("pointerdown", down);
      node.removeEventListener("touchmove", touchMove);
      cancelAnimationFrame(frame);
      unsnap();
      ghost?.remove();
    },
  };
}
