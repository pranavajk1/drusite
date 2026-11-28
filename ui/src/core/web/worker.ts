// SPDX-License-Identifier: AGPL-3.0-or-later

// One worker per tab. OPFS lets one worker at a time hold the store, so the tabs' workers elect
// one with the Web Locks API: it loads the core (leader.ts) and the others send it their
// commands over a BroadcastChannel. Events reach every tab. When the owning tab closes, its
// lock goes with it, the next worker in line takes over, and each tab opens its pages again.

import type { ApiError, Command, Event, Response } from "../types";
import { Leader } from "./leader";
import type { FromTab, ToTab } from "./transport";

type Result = { ok: Response } | { err: ApiError };

/** Between workers. */
type Wire =
  | { kind: "call"; from: string; to: string; id: number; command: Command }
  | { kind: "reply"; to: string; id: number; result: Result }
  | { kind: "event"; event: Event; except?: string }
  | { kind: "navigate"; url: string; to?: string }
  | { kind: "leader"; id: string }
  | { kind: "who" };

const LOCK = "drusite-store";

const scope = self as unknown as {
  postMessage(message: ToTab): void;
  onmessage: ((e: MessageEvent<FromTab>) => void) | null;
};

const me = crypto.randomUUID();
const channel = new BroadcastChannel("drusite");
let leader: Leader | null = null;
let leaderId: string | null = null;

interface Pending {
  command: Command;
  /** The leader it went to, or null while it waits for one. */
  sentTo: string | null;
  done: (r: Result) => void;
}
let nextId = 1;
const pending = new Map<number, Pending>();
/** The pages this tab has open, and how many views each, to open again under a new leader. */
const open = new Map<string, number>();

const toTab = (m: ToTab) => scope.postMessage(m);
const post = (m: Wire) => channel.postMessage(m);

const host = {
  emit(event: Event, except?: string) {
    if (except !== me) toTab({ kind: "event", event });
    post({ kind: "event", event, except });
  },
  navigate(url: string, to?: string) {
    if (to === undefined || to === me) toTab({ kind: "navigate", url });
    if (to !== me) post({ kind: "navigate", url, to });
  },
};

/** Runs a command wherever the store is. */
function call(command: Command): Promise<Result> {
  if (leader) return leader.handle(me, command);
  return new Promise((done) => {
    const id = nextId++;
    const p: Pending = { command, sentTo: null, done };
    pending.set(id, p);
    send(id, p);
  });
}

function send(id: number, p: Pending) {
  if (!leaderId) return post({ kind: "who" });
  p.sentTo = leaderId;
  post({ kind: "call", from: me, to: leaderId, id, command: p.command });
}

/** A new owner of the store: what was sent to the old one is lost; the tab's pages reopen. */
async function newLeader(id: string) {
  if (id === leaderId) return;
  const before = leaderId;
  leaderId = id;
  for (const [n, p] of pending) {
    if (p.sentTo === null) send(n, p);
    else if (p.sentTo !== id) {
      pending.delete(n);
      p.done({ err: { code: "reloaded", message: "the tab that kept your notes closed; try again" } });
    }
  }
  if (before !== null) await reopen();
}

/** Opens this tab's pages under the new owner, and has the editor show them as saved. */
async function reopen() {
  for (const [page, views] of open) {
    for (let i = 0; i < views; i++) {
      const r = await call({ type: "openPage", page });
      if (i === 0 && "ok" in r && r.ok.type === "opened")
        toTab({ kind: "event", event: { type: "pageReloaded", view: r.ok.view, reason: "another tab closed" } });
    }
  }
}

channel.onmessage = (e: MessageEvent<Wire>) => {
  const m = e.data;
  switch (m.kind) {
    case "call":
      if (leader && m.to === me) {
        void leader.handle(m.from, m.command).then((result) => post({ kind: "reply", to: m.from, id: m.id, result }));
      }
      break;
    case "reply": {
      if (m.to !== me) break;
      const p = pending.get(m.id);
      if (p) {
        pending.delete(m.id);
        p.done(m.result);
      }
      break;
    }
    case "event":
      if (m.except !== me) toTab({ kind: "event", event: m.event });
      break;
    case "navigate":
      if (m.to === undefined || m.to === me) toTab({ kind: "navigate", url: m.url });
      break;
    case "leader":
      void newLeader(m.id);
      break;
    case "who":
      if (leader) post({ kind: "leader", id: me });
      break;
  }
};

scope.onmessage = (e: MessageEvent<FromTab>) => {
  const m = e.data;
  if (m.kind === "call") {
    void call(m.command).then((result) => {
      if ("ok" in result) track(m.command);
      toTab({ kind: "reply", id: m.id, result });
    });
  } else if (m.kind === "wake") {
    leader?.kick();
  }
};

function track(command: Command) {
  if (command.type === "openPage") open.set(command.page, (open.get(command.page) ?? 0) + 1);
  if (command.type === "closePage") {
    const n = (open.get(command.page) ?? 0) - 1;
    if (n > 0) open.set(command.page, n);
    else open.delete(command.page);
  }
}

// Wait in line for the store. Holding the lock is owning it: the promise never settles, so the
// lock is released only when this worker ends (its tab closed).
void navigator.locks.request(LOCK, async () => {
  try {
    leader = await Leader.start(host);
  } catch (e) {
    toTab({ kind: "failed", message: e instanceof Error ? e.message : String(e) });
    return;
  }
  const had = leaderId !== null;
  leaderId = me;
  // What this tab sent to the old owner is lost; what waited goes to this one.
  for (const [n, p] of pending) {
    pending.delete(n);
    if (p.sentTo === null) void leader.handle(me, p.command).then(p.done);
    else p.done({ err: { code: "reloaded", message: "the tab that kept your notes closed; try again" } });
  }
  post({ kind: "leader", id: me });
  toTab({ kind: "ready", persistent: leader.persistent });
  if (had) await reopen();
  await new Promise<never>(() => {});
});

// Find out who owns the store, if anyone does yet; a follower is ready once it knows.
post({ kind: "who" });
const asked = setInterval(() => {
  if (leaderId) {
    clearInterval(asked);
    if (!leader) toTab({ kind: "ready", persistent: true });
  } else post({ kind: "who" });
}, 100);
