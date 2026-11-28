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

/**
 * The transport for wherever the UI is running: Tauri's IPC in the app; in a browser, the core as
 * WebAssembly (the web build, `vite build --mode web`), or the in-memory core for the UI's own
 * tests and `npm run dev`.
 */
async function transport(): Promise<Transport> {
  if ("__TAURI_INTERNALS__" in window) return (await import("./core/tauri")).tauriTransport;
  if (import.meta.env.MODE === "web" || import.meta.env.VITE_DRUSITE_CORE === "wasm") {
    const { webCore } = await import("./core/web/transport");
    const core = webCore();
    if (import.meta.env.PROD && "serviceWorker" in navigator) void navigator.serviceWorker.register("/sw.js");
    await core.ready;
    // The browser tests (`e2e/`) compare what the page shows with what the core holds.
    if (import.meta.env.VITE_DRUSITE_TEST) {
      const { testCore } = await import("./core/web/testing");
      const t = testCore(core.transport);
      (window as { drusiteWebCore?: unknown }).drusiteWebCore = t.core;
      return t.transport;
    }
    return core.transport;
  }
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
