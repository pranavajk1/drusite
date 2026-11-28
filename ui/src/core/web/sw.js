// SPDX-License-Identifier: AGPL-3.0-or-later

// The web app's service worker: it keeps the app's own files (the page, its scripts and styles,
// the core's WebAssembly), so the app opens with no network. The notes themselves are in OPFS;
// nothing from the server's API is kept here. The build fills in the file list and version
// (vite.config.ts).

const FILES = self.__FILES__;
const CACHE = `drusite-${self.__VERSION__}`;

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.addAll(FILES))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((names) => Promise.all(names.filter((n) => n.startsWith("drusite-") && n !== CACHE).map((n) => caches.delete(n))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") return;
  const url = new URL(request.url);
  if (url.origin !== location.origin) return;
  // The page: the network's when there is one (a new version, or a sign-in redirect), else the
  // kept copy.
  if (request.mode === "navigate" && url.pathname === "/") {
    event.respondWith(
      fetch(request).catch(() => caches.match("/", { ignoreVary: true }).then((r) => r ?? Response.error())),
    );
    return;
  }
  // The app's files are named by their content: kept ones are always right. Matched by URL
  // alone: module scripts are fetched in CORS mode, and a response that varies on Origin
  // wouldn't match the request that kept it.
  if (url.pathname.startsWith("/app/")) {
    event.respondWith(caches.match(url.pathname, { ignoreVary: true }).then((r) => r ?? fetch(request)));
  }
  // Everything else (the API, sign-in pages, published pages) goes to the network untouched.
});
