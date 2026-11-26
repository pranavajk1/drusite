// SPDX-License-Identifier: AGPL-3.0-or-later

import { mount } from "svelte";

import "prosemirror-view/style/prosemirror.css";

import App from "./App.svelte";
import "./app.css";
import { CoreClient, type Transport } from "./core/client";

/** The transport for wherever the UI is running: Tauri's IPC in the app, memory in a browser. */
async function transport(): Promise<Transport> {
  if ("__TAURI_INTERNALS__" in window) return (await import("./core/tauri")).tauriTransport;
  const { MemoryCore } = await import("./core/memory");
  return new MemoryCore().transport;
}

const target = document.getElementById("app");
if (target) {
  const client = new CoreClient(await transport());
  mount(App, { target, props: { client } });
}
