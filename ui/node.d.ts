// SPDX-License-Identifier: AGPL-3.0-or-later

// The one piece of Node that vite.config.ts uses (the UI's types leave Node's out: its code runs
// in a browser).

declare module "node:fs" {
  export function readFileSync(path: URL, encoding: "utf8"): string;
}
