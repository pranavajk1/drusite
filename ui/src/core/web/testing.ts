// SPDX-License-Identifier: AGPL-3.0-or-later

// For the browser tests only (built in with VITE_DRUSITE_TEST): the WebAssembly core, reached
// the way `e2e/helpers.ts` reaches the in-memory one, so the same tests compare what the page
// shows with what the core holds.

import type { Transport } from "../client";
import type { Block, Command, PageId, Response } from "../types";

export interface TestCore {
  /** The pages the UI has open. */
  openPages(): PageId[];
  /** A page's blocks as the core holds them. */
  blocks(page: PageId): Promise<Block[]>;
  handle(command: Command): Promise<Response>;
}

/** Wraps `transport`, counting the pages the UI opens through it. */
export function testCore(transport: Transport): { transport: Transport; core: TestCore } {
  const open = new Map<PageId, number>();
  const counted: Transport = {
    ...transport,
    async call(command) {
      const r = await transport.call(command);
      if (command.type === "openPage") open.set(command.page, (open.get(command.page) ?? 0) + 1);
      if (command.type === "closePage") {
        const n = (open.get(command.page) ?? 0) - 1;
        if (n > 0) open.set(command.page, n);
        else open.delete(command.page);
      }
      return r;
    },
  };
  const core: TestCore = {
    openPages: () => [...open.keys()],
    async blocks(page) {
      // Opening it once more and closing again leaves the UI's view of it as it was.
      const r = await transport.call({ type: "openPage", page });
      await transport.call({ type: "closePage", page });
      return r.type === "opened" ? r.view.blocks : [];
    },
    handle: (command) => transport.call(command),
  };
  return { transport: counted, core };
}
