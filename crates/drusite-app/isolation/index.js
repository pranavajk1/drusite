// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Tauri's isolation pattern: every IPC message from the UI passes through this sandboxed frame
// before it reaches Rust. Only the app's one command, event listening, and the folder picker
// (Settings: Markdown folder and Import, on desktops; its capability is in capabilities/desktop.json)
// get through, and Tauri's own fetch of a reply it sends over a channel: on Android a large reply
// (a database view, a long page) arrives that way, and without it the call never resolves.

const ALLOWED = new Set([
  "core",
  "plugin:event|listen",
  "plugin:event|unlisten",
  "plugin:dialog|open",
  "plugin:__TAURI_CHANNEL__|fetch",
]);

window.__TAURI_ISOLATION_HOOK__ = (payload) => {
  if (!payload || !ALLOWED.has(payload.cmd)) {
    throw new Error(`blocked IPC command: ${payload && payload.cmd}`);
  }
  return payload;
};
