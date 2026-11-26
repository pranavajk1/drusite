// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Tauri's isolation pattern: every IPC message from the UI passes through this sandboxed frame
// before it reaches Rust. Only the app's one command and event listening get through.

const ALLOWED = new Set(["core", "plugin:event|listen", "plugin:event|unlisten"]);

window.__TAURI_ISOLATION_HOOK__ = (payload) => {
  if (!payload || !ALLOWED.has(payload.cmd)) {
    throw new Error(`blocked IPC command: ${payload && payload.cmd}`);
  }
  return payload;
};
