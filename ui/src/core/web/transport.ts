// SPDX-License-Identifier: AGPL-3.0-or-later

// The browser's transport: the core as WebAssembly, in a worker (worker.ts). Commands go to it
// and answers and events come back as messages; nothing here touches the store.

import type { Transport } from "../client";
import type { ApiError, Command, Event, Response } from "../types";

/** From a tab to its worker. */
export type FromTab = { kind: "call"; id: number; command: Command } | { kind: "wake" };

/** From a worker to its tab. */
export type ToTab =
  | { kind: "reply"; id: number; result: { ok: Response } | { err: ApiError } }
  | { kind: "event"; event: Event }
  | { kind: "navigate"; url: string }
  | { kind: "ready"; persistent: boolean }
  | { kind: "failed"; message: string };

export interface WebCore {
  transport: Transport;
  /** Settles once the store is open here or in another tab; false if notes are kept in memory only. */
  ready: Promise<boolean>;
}

export function webCore(): WebCore {
  const worker = new Worker(new URL("./worker.ts", import.meta.url), { type: "module", name: "drusite-core" });
  const waiting = new Map<number, { resolve: (r: Response) => void; reject: (e: unknown) => void }>();
  const handlers = new Set<(e: Event) => void>();
  let nextId = 1;
  let settle: (persistent: boolean) => void = () => {};
  let fail: (e: Error) => void = () => {};
  const ready = new Promise<boolean>((resolve, reject) => {
    settle = resolve;
    fail = reject;
  });

  worker.onmessage = (e: MessageEvent<ToTab>) => {
    const m = e.data;
    switch (m.kind) {
      case "reply": {
        const w = waiting.get(m.id);
        if (!w) return;
        waiting.delete(m.id);
        if ("ok" in m.result) w.resolve(m.result.ok);
        else w.reject(m.result.err);
        break;
      }
      case "event":
        for (const h of handlers) h(m.event);
        break;
      case "navigate":
        location.assign(m.url);
        break;
      case "ready":
        settle(m.persistent);
        break;
      case "failed":
        fail(new Error(m.message));
        break;
    }
  };
  const post = (m: FromTab) => worker.postMessage(m);

  // Back online, or back to this tab: sync shouldn't wait for its backoff.
  addEventListener("online", () => post({ kind: "wake" }));
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") post({ kind: "wake" });
  });

  const transport: Transport = {
    web: true,
    call(command: Command): Promise<Response> {
      return new Promise((resolve, reject) => {
        const id = nextId++;
        waiting.set(id, { resolve, reject });
        post({ kind: "call", id, command });
      });
    },
    subscribe(handler: (event: Event) => void): () => void {
      handlers.add(handler);
      return () => handlers.delete(handler);
    },
  };
  return { transport, ready };
}
