// SPDX-License-Identifier: AGPL-3.0-or-later

// The Tauri transport: the only file in the UI that imports Tauri.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

import type { Transport } from "./client";
import type { Command, Event, Response } from "./types";

/** The Tauri event the core's events arrive on (see crates/drusite-app). */
const EVENT = "drusite://event";

export const tauriTransport: Transport = {
  call(command: Command): Promise<Response> {
    return invoke<Response>("core", { command });
  },

  async pickFolder(title: string): Promise<string | null> {
    const folder = await open({ directory: true, title });
    return typeof folder === "string" ? folder : null;
  },

  async pickFile(title: string, extensions: string[]): Promise<string | null> {
    const file = await open({ directory: false, title, filters: [{ name: title, extensions }] });
    return typeof file === "string" ? file : null;
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
