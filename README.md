# Drusite

An open-source notes app: Tauri 2 on every platform, a Rust core with Loro CRDT documents, and a
Rust sync server. Notion's block model, real-time sync, and notes that AI tools can read.

**Status:** Phase 1 (foundations), in progress. Nothing here is a usable app yet.

## Layout

| crate | job |
|---|---|
| `drusite-model` | IDs, the page and block schema, schema versioning |
| `drusite-crdt` | The block model on Loro: one document per page, plus the workspace document |
| `drusite-proto` | The sync protocol (Phase 2) |
| `drusite-core` | The client engine and the Core API the UI talks to |
| `drusite-ai` | The AI provider interface and adapters (Phase 3) |
| `drusite-server` | The server binary (Phase 2) |
| `drusite-app` | The Tauri 2 shell, an adapter from the Core API to Tauri |
| `drusite-wasm` | The browser adapter: the core as WebAssembly (Phase 6) |
| `xtask` | Repo tooling: `cargo xtask budgets` enforces the performance budgets |

## Building

The Rust toolchain is pinned in `rust-toolchain.toml`; `rustup` installs it on first use.

```bash
cargo test --workspace
cargo xtask budgets
```

[`CONTRIBUTING.md`](CONTRIBUTING.md) lists every check CI runs.

## Licence

[AGPL-3.0-or-later](LICENSE), with an additional permission for app stores
([`APP-STORE-EXCEPTION.md`](APP-STORE-EXCEPTION.md), a draft until it has been reviewed).
Contributions are signed off under the [Developer Certificate of Origin](DCO); there is no CLA.
