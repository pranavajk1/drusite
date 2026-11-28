// SPDX-License-Identifier: AGPL-3.0-or-later

// The app's version: the workspace's, from Cargo.toml, put in by the build (vite.config.ts).

declare const __APP_VERSION__: string;

export const APP_VERSION: string = __APP_VERSION__;
