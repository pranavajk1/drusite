// SPDX-License-Identifier: AGPL-3.0-or-later

import { mount } from "svelte";

import "prosemirror-view/style/prosemirror.css";

import App from "./App.svelte";
import "./styles/index.css";
import { CoreClient, type Transport } from "./core/client";
import { applyTheme, watchSystemTheme } from "./core/theme";

// Before the first paint, so a dark-mode user never sees a white flash.
applyTheme();
watchSystemTheme();

/** The transport for wherever the UI is running: Tauri's IPC in the app, memory in a browser. */
async function transport(): Promise<Transport> {
  if ("__TAURI_INTERNALS__" in window) return (await import("./core/tauri")).tauriTransport;
  const { MemoryCore } = await import("./core/memory");
  const core = new MemoryCore();
  // The browser tests (`e2e/`) compare what the page shows with what the core holds.
  if (import.meta.env.DEV) (window as { drusiteMemoryCore?: unknown }).drusiteMemoryCore = core;
  return core.transport;
}

const target = document.getElementById("app");
if (target) {
  const client = new CoreClient(await transport());
  mount(App, { target, props: { client } });
}
