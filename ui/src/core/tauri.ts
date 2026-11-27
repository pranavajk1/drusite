// SPDX-License-Identifier: AGPL-3.0-or-later

// The Tauri transport: the only file in the UI that imports Tauri.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

import type { Transport } from "./client";
import type { Command, Event, Response } from "./types";

/** The Tauri event the core's events arrive on (see crates/drusite-app). */
const EVENT = "drusite://event";

/** Phones and tablets have no dialog plugin (see crates/drusite-app): files come from an
 *  `<input type="file">` as bytes there. iPadOS calls itself a Mac, but one with a touch screen. */
const mobile =
  typeof navigator !== "undefined" &&
  (/Android|iPhone|iPad/.test(navigator.userAgent) ||
    (/Macintosh/.test(navigator.userAgent) && navigator.maxTouchPoints > 1));

const pickers: Pick<Transport, "pickFolder" | "pickFile"> = {
  async pickFolder(title: string): Promise<string | null> {
    const folder = await open({ directory: true, title });
    return typeof folder === "string" ? folder : null;
  },

  async pickFile(title: string, extensions: string[]): Promise<string | null> {
    const file = await open({ directory: false, title, filters: [{ name: title, extensions }] });
    return typeof file === "string" ? file : null;
  },
};

export const tauriTransport: Transport = {
  ...(mobile ? {} : pickers),

  call(command: Command): Promise<Response> {
    return invoke<Response>("core", { command });
  },

  subscribe(handler: (event: Event) => void): () => void {
    let stop: (() => void) | undefined;
    let stopped = false;
    void listen<Event>(EVENT, (e) => handler(e.payload)).then((unlisten) => {
      if (stopped) unlisten();
      else stop = unlisten;
    });
    return () => {
      stopped = true;
      stop?.();
    };
  },
};
