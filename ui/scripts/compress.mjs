// SPDX-License-Identifier: AGPL-3.0-or-later
//
// After `vite build --mode web`: writes brotli and gzip copies of the web app's files beside
// them (drusite-server sends them to browsers that take them), and checks the download budget:
// what a first visit fetches, brotli-compressed, for the core's WebAssembly and for the app's
// JavaScript. Fails past either.
//
//   node scripts/compress.mjs [dist-web]

import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";

/** Bytes, brotli-compressed: set from the first build, about a quarter above it. */
const BUDGET = {
  wasm: 2_150_000,
  js: 185_000,
};

const root = process.argv[2] ?? new URL("../dist-web", import.meta.url).pathname;
const files = (dir) =>
  readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? files(path) : [path];
  });

const sizes = { wasm: 0, js: 0 };
for (const path of files(root)) {
  if (/\.(br|gz)$/.test(path)) continue;
  if (!/\.(js|css|wasm|html|svg|json)$/.test(path)) continue;
  const body = readFileSync(path);
  const br = brotliCompressSync(body, { params: { [constants.BROTLI_PARAM_QUALITY]: 11, [constants.BROTLI_PARAM_SIZE_HINT]: body.length } });
  writeFileSync(`${path}.br`, br);
  writeFileSync(`${path}.gz`, gzipSync(body, { level: 9 }));
  if (path.endsWith(".wasm")) sizes.wasm += br.length;
  else if (path.endsWith(".js")) sizes.js += br.length;
}

let over = false;
for (const [kind, size] of Object.entries(sizes)) {
  const limit = BUDGET[kind];
  const ok = size <= limit;
  over ||= !ok;
  console.log(`${ok ? "ok  " : "OVER"} ${kind.padEnd(4)} ${(size / 1024).toFixed(0).padStart(6)} KiB brotli (budget ${(limit / 1024).toFixed(0)} KiB)`);
}
if (over) {
  console.error("The web app's download is over budget (ui/scripts/compress.mjs).");
  process.exit(1);
}
