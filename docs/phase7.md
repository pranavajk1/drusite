# Phase 7: the browser version

The roadmap's entry: *the core as WebAssembly with browser storage, login through a
backend-for-frontend*. Done when **the full app works from a browser on a machine where I can't
install it**.

The UI already reaches the core only through `CoreClient` and a `Transport` (`ui/src/core/`),
so the browser version is a new transport plus the pieces of the core that were native-only.
Everything below is built for any deployment: the web app is served by the Drusite server
itself, at the same origin, with no extra service and nothing assuming one setup.

## 1. What compiles to wasm32, and what's split out

`drusite-model`, `-crdt`, `-db`, `-formula`, `-markdown` and `-proto` (without its C zstd) already
build for `wasm32-unknown-unknown`, as CI checks. `drusite-core` with its `sqlite` feature does
too, unchanged in the parts that matter:

| piece | native | browser |
|---|---|---|
| Encrypted local store | rusqlite + SQLCipher | **the same code**: rusqlite builds on wasm32 through `sqlite-wasm-rs`, which compiles SQLCipher (libtomcrypt) to wasm. Same schema, same migrations, same cipher. Stored in OPFS by `sqlite-wasm-vfs`'s *sahpool* VFS (sync access handles, so it runs in a dedicated worker). `journal_mode = DELETE` there: the VFS has no WAL |
| Full-text search | tantivy, in memory | tantivy's writer needs threads. On wasm a small in-memory inverted index with the same tokenising (lower case, accents folded), the same rules (every word, the last as a prefix) and the same hits; behind the same `Core::search` |
| Clock | `SystemTime` | `Date.now()` (`SystemTime` panics on wasm32). `time` and `uuid` get their wasm features |
| Sync driver (`net`) | tokio + tungstenite + ureq, DPoP | **not compiled**. The sync *session* is already sans-I/O (`sync_start`, `sync_receive`, `sync_outgoing`, `sync_retry`); the worker owns a `WebSocket` and pumps frames through it. Frames over 1 KiB come zstd-compressed from the server: decoded with `ruzstd` (pure Rust); the browser sends plain frames, which the server already takes |
| Attachments up/down | ureq beside the socket | `fetch` beside the socket, same queue in the core |
| Account (sign-in, refresh, workspace to join) | `account::Supervisor` | the worker: the session cookie replaces tokens; "which workspace to join" uses the core's same `choose` rule, now public |
| Store key | Keychain / Keystore / file | see §2 |
| DPoP device key | Secure Enclave, Keystore, Keychain | none: the browser holds no token (§4) |
| Markdown bridge, folder watcher | `watch` | not compiled (no folders in a browser) |
| Local MCP | `mcp` (axum, rmcp) | not compiled (a browser can't listen on a port) |
| Device AI providers | `ai` (reqwest) | not compiled: vendors' APIs refuse browser origins (CORS), and a key in the browser is a key in JavaScript. Ask your notes runs on the server and works |
| Notion import | zip (bytes) and API (ureq) | zip from bytes works as on phones; the API import needs a server-side proxy, so it's left out in the browser for now |

`drusite-wasm` is the adapter: `wasm-bindgen` exports a `WasmCore` with `handle(command) →
response` (the same JSON the Tauri adapter passes), the event queue, and the sync and
attachment hooks. Events and the change listener go through a queue the worker drains after
every call, so nothing JavaScript is held across Rust's `Send` bounds.

## 2. Browser storage and the store key

- **OPFS**, through the sahpool VFS: one SQLCipher database (`/drusite/store.db`), the same
  format as on devices. IndexedDB holds only the wrapped store key. `navigator.storage.persist()`
  is asked for so the browser doesn't evict the store under pressure.
- **The key**: 32 random bytes, as everywhere. It's kept in IndexedDB wrapped (AES-GCM) by a
  **non-extractable** WebCrypto key also kept there: script on the page can use it but never
  read it out, the nearest a browser has to a keychain. Copying the profile's files copies the
  ciphertext, not a key anyone can export.
- **Signing out wipes**: the OPFS store, the wrapped key and the app's caches. So does the server
  answering that this browser was signed out elsewhere (Account → Devices, or an expired
  session) the next time it's online.

## 3. Sync from the browser

The existing protocol, unchanged: `GET /api/v1/sync` upgraded to a WebSocket with the
`drusite.sync` subprotocol, the server speaks first, one subscription per document. The worker
runs the same loop as `sync/net.rs`: ping every 30 s, drop a connection silent for 75 s,
reconnect with the same backoff (1 s doubling to 5 min, with jitter), and reconnect at once on
the browser's `online` event or when the tab becomes visible.

## 4. Login: a backend-for-frontend

The browser never holds an OAuth token. The server is the OAuth client for it:

1. The app, signed out, sends the browser to `GET /web/login`. The server makes a PKCE
   verifier and state (kept in `auth_challenges`, bound to a nonce cookie) and redirects to its
   own `/oauth/authorize` as the built-in client **`drusite-web`**, whose only redirect is the
   server's own `/web/callback`.
2. The usual sign-in page runs: every login method the server has switched on (OIDC, passkeys,
   password with TOTP, magic links), with the same rules for self-signup and who may sign in.
3. `/web/callback` checks the state, spends the code in-process and makes a **grant**, like a
   device's: listed under Account → Devices ("Firefox on macOS"), revocable there, with idle and
   absolute expiry. Instead of tokens it sets one cookie:
   `__Host-drusite_web` (plain `drusite_web` on http): **HttpOnly, SameSite=Strict, Secure,
   Path=/**, an opaque random value whose hash maps to the grant.

**What replaces DPoP.** DPoP binds a token to a key so a stolen token is useless elsewhere; here
there's no token for script to steal. The cookie is HttpOnly (no script reads it), SameSite=Strict
(no cross-site request carries it) and `__Host-` (no other host or path can set or shadow it).
Sibling subdomains are "same site", so every cookie-authenticated request is also checked for
**`Origin` equal to the server's own** (WebSocket upgrades included) and Fetch Metadata that
isn't `cross-site` or `same-site`; and the server never sends CORS headers. The cookie is
accepted by the same `Authenticated` extractor every API route uses, so the browser reaches
exactly what a device can (sync, attachments, Ask, publish, agent activity) under the same
Cedar policy. Device Bound Session Credentials would bind the cookie to a key, but only Chrome
has them; a later step if they spread.

Sign-out (`POST /api/v1/web/logout`) revokes the grant, clears the cookie, and ends the
sign-in pages' own session too, so a shared machine is left signed out everywhere.

## 5. How it's served

- The server serves the web app from **`[web] dir`** (a built `ui/dist-web`): `index.html` at
  `/`, hashed files under `/app/` with `immutable` caching, the service worker at `/sw.js`. With
  `dir` unset there are no web routes, as with any switched-off feature. The page gets a strict
  CSP: `script-src 'self' 'wasm-unsafe-eval'`, `connect-src 'self'`, `worker-src 'self'`,
  `frame-ancestors 'none'`, no inline anything.
- **The image** builds the web app (a Node stage for the UI, the wasm crate with
  `wasm-bindgen`) and ships it at `/usr/share/drusite/web` with `DRUSITE__WEB__DIR` set, so
  every deployment of the image has it.
- **The Helm chart** keeps it on (`web.enabled: true`), on the same host the ingress already
  routes. A sigma-lab deployment gets it with the chart bump that follows the release.
- Published pages on their own public host are untouched: that host serves only them.

## 6. Offline, tabs, budgets

- **Offline**: a service worker keeps the app shell (HTML, JS, CSS, wasm) so a reload works
  without the network; the notes are in OPFS. Edits save locally and sync on reconnect, as on
  devices. Signing in, Ask and publishing need the server and say so.
- **Several tabs**: every tab works. The tabs' workers elect one owner of the store with the
  Web Locks API (the sahpool VFS allows one connection); the others send their commands to it
  over a `BroadcastChannel`. Events go to every tab, and an edit made in one tab reaches the
  others as a page patch, as edits from another device do. When the owning tab closes, the next
  takes the lock, opens the store and the other tabs re-open their pages.
- **Budgets**, checked in CI:
  - download: the wasm module and the app's JS, brotli-compressed, each under a fixed ceiling
    (set from the first build, about 25% above it);
  - memory: a Chromium tab (page + worker heaps + wasm memory) with the 1,000-page workspace
    and one page open, under 250 MB, the desktop's budget.

## 7. What stays desktop- or device-only, and how it's hidden

The core answers `unsupported` or `available: false` for features it isn't built with, and the
UI already hides on that (as on phones): the Markdown folder, local agents (local MCP), device AI
providers, folder pickers. In the browser also: the Notion API import, and the server address
field (the server is where the page came from). Settings → Account shows the account, the
server, sync state and Sign out.

## 8. Tests and the exit test

- **CI**: `cargo build` of the core and `drusite-wasm` for wasm32; a `web` job that builds the
  wasm with `wasm-bindgen`, builds `ui/dist-web`, checks the download budget, and runs Playwright
  against the **real wasm core** in Chromium, WebKit and Firefox (the existing e2e specs that
  don't need a desktop, plus browser-only ones: reload keeps notes, two tabs, offline then
  online); the RAM budget in Chromium. Server tests for the web login and cookie rules.
- **Exit test, by hand**: the server locally with Postgres and S3 in containers and the built
  web app; sign in from a browser; create and edit pages, a journal entry, a database (board
  and table), Ask; sync both ways with the Mac app built from this branch (its own bundle ID and
  data directory); go offline, edit, come back; two tabs at once. The PR lists what was done by
  hand and what only in CI.

## Decisions

| decision | choice | why |
|---|---|---|
| Local copy on a machine I can't install on | **An encrypted copy that opens offline**, wiped on sign-out and when the server says the browser was signed out | The point is a full app on that machine, offline included; the wipe covers leaving it behind |
| How long a browser stays signed in | **Its own limits, 14 days idle and 90 at most** (`[auth] browser_idle_days`, `browser_max_days`) | A browser on someone else's machine is likelier to be left behind than a phone |
| Where the web app lives | **The server's own address, at `/`** | Same origin is what makes an HttpOnly, SameSite=Strict cookie work with no CORS at all; nothing else to deploy |
| Several tabs | **All live**, one owns the store | A second tab that only says "open elsewhere" isn't the full app |
| Device AI providers, Notion API import | **Not in the browser** | Vendors' APIs refuse browser origins, and keys would sit in JavaScript; Ask runs on the server either way |
