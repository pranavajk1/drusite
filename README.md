# Drusite

An open-source notes app: Tauri 2 on every platform, a Rust core with Loro CRDT documents, and a
Rust sync server. Notion's block model, real-time sync, and notes that AI tools can read.

**Status:** Phase 2 (server, sync, login), in progress. Nothing here is a usable app yet.

## Layout

| crate | job |
|---|---|
| `drusite-model` | IDs, the page and block schema, schema versioning |
| `drusite-crdt` | The block model on Loro: one document per page, plus the workspace document |
| `drusite-proto` | The sync protocol (Phase 2) |
| `drusite-core` | The client engine and the Core API the UI talks to |
| `drusite-ai` | The AI provider interface and adapters (Phase 3) |
| `drusite-server` | The server binary: PostgreSQL 16+ with pgvector, any S3-compatible store. [`drusite.example.toml`](crates/drusite-server/drusite.example.toml) shows its configuration |
| `drusite-app` | The Tauri 2 shell, an adapter from the Core API to Tauri |
| `drusite-wasm` | The browser adapter: the core as WebAssembly (Phase 7) |
| `xtask` | Repo tooling: `cargo xtask budgets` enforces the performance budgets; `cargo xtask bindings` writes the UI's TypeScript types |
| `ui/` | The frontend (TypeScript). It reaches the core only through `ui/src/core/client.ts` |

## Building

The Rust toolchain is pinned in `rust-toolchain.toml`; `rustup` installs it on first use. The app
embeds the built UI, so build the UI once before building the workspace:

```bash
(cd ui && npm ci && npm run build)
cargo test --workspace
cargo xtask budgets
```

Run the desktop app (macOS for now), with the UI reloading as you edit it:

```bash
(cd ui && npm run tauri dev)
```

It keeps its workspace in `~/Library/Application Support/dev.drusite.app/` and the key that
encrypts it in the login Keychain (`dev.drusite.store`). `cd ui && npm run dev` runs the UI alone
in a browser, against an in-memory core.

[`CONTRIBUTING.md`](CONTRIBUTING.md) lists every check CI runs.

## Licence

[AGPL-3.0-or-later](LICENSE), with an additional permission for app stores
([`APP-STORE-EXCEPTION.md`](APP-STORE-EXCEPTION.md), a draft until it has been reviewed).
Contributions are signed off under the [Developer Certificate of Origin](DCO); there is no CLA.
