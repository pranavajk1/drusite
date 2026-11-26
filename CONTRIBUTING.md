# Contributing to Drusite

## Licence and sign-off

Drusite is licensed under AGPL-3.0-or-later ([`LICENSE`](LICENSE)), with the additional permission
in [`APP-STORE-EXCEPTION.md`](APP-STORE-EXCEPTION.md). By contributing, you agree that your
contribution is licensed under those same terms, the additional permission included.

There is no CLA. Instead, every commit carries a sign-off under the
[Developer Certificate of Origin](DCO): a line at the end of the commit message, matching the
commit's author, certifying that you have the right to submit the change under the project's
licence.

```text
Signed-off-by: Your Name <you@example.com>
```

`git commit -s` adds it. CI checks every commit in a pull request and fails without one.

## Checks

CI runs these on macOS, Linux and Windows; run them before you push:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo xtask budgets
cargo xtask bindings --check
(cd ui && npm ci && npm run build && npm run check && npm test)
```

The server's tests need PostgreSQL with pgvector, and two of them an S3-compatible store and
OpenBao; without them those tests are skipped (CI runs them all, on PostgreSQL 16 and the
newest release). To run them locally:

```bash
docker run -d --name drusite-pg -e POSTGRES_PASSWORD=postgres -p 55432:5432 pgvector/pgvector:0.8.7-pg19
DRUSITE_TEST_DATABASE_URL=postgres://postgres:postgres@localhost:55432/postgres cargo test -p drusite-server
```

`.github/workflows/ci.yml` (the `server` job) shows how it starts the store and OpenBao.

Sign-in end to end, in real Chromium against a real OIDC provider (Keycloak) and SMTP server
(Mailpit), with a passkey on Chromium's virtual authenticator. The script starts everything in
Docker and cleans up; it isn't in CI. `HEADED=1` shows the browser.

```bash
(cd ui && npm ci)
ui/server-e2e/run.sh
```

Sync is also tested in a deterministic network simulation (turmoil): real cores against a
model of the server, through partitions, dropped connections, duplicated messages and crashes,
checking that every edit survives. It's too slow for the ordinary test run, so it's ignored
there; CI runs 3 seeds in its own job. Run many more before changing sync:

```bash
DRUSITE_SIM_SEEDS=50 cargo test -p drusite-core --test simulation --release -- --ignored --nocapture
```

## Phones

The same app runs on Android and iOS; the generated projects are committed under
`crates/drusite-app/gen/` and edited by hand in a few places (the sign-in redirect in the Android
manifest, release signing in `app/build.gradle.kts`, and npm running from `ui/`), so don't
regenerate them with `tauri android init` or `tauri ios init`. The platform code (device keys in
StrongBox or the Secure Enclave, the store key, signing in through the system browser) is
`crates/tauri-plugin-drusite`.

Android needs the SDK, NDK 30 and a JDK (`JAVA_HOME`, `ANDROID_HOME`, `NDK_HOME`), and the Rust
target `aarch64-linux-android`. Build through `ui/android.sh`, which sets what the vendored OpenSSL
needs from the NDK:

```bash
ui/android.sh build --debug --apk --target aarch64
adb install -r crates/drusite-app/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
```

iOS, in the Simulator (Xcode, and the Rust target `aarch64-apple-ios-sim`):

```bash
(cd ui && npm run tauri ios build -- --debug --target aarch64-sim)
xcrun simctl install booted crates/drusite-app/gen/apple/build/arm64-sim/Drusite.app
```

To sign in from the emulator to a server on your Mac, run the server with
`public_url = "http://localhost:<port>"` and forward the port (`adb reverse tcp:<port> tcp:<port>`);
the Simulator reaches `localhost` directly.

Android releases: pushing a tag `v<version>`, matching `version` in
`crates/drusite-app/tauri.conf.json`, builds a signed APK and attaches it to a GitHub release
(`.github/workflows/android.yml`, which lists the secrets it needs).

After changing the Core API (`crates/drusite-core/src/api.rs`), regenerate the UI's types with
`cargo xtask bindings` and commit them.

Supply chain (Linux in CI):

```bash
cargo deny check
cargo vet --locked
cargo audit
```

A new or updated dependency needs a `cargo vet` audit or exemption (`cargo vet certify`, or
`cargo vet` and follow its suggestions), and a licence from the allow-list in `deny.toml`.

## Performance budgets

`cargo xtask budgets` fails when the app breaks a budget:

- **Idle RAM**: 250 MiB for the desktop app, every process counted (the webview's too), with a
  1,000-page workspace and one page open (450 MiB on Linux for now; `xtask/src/budgets.rs` says
  why).
- **Keystroke**: the core's cost of typing a character, Enter and Backspace in a 10,000-block page,
  and how much each grows from a 100-block page.

The numbers and the reasons for them are in [`xtask/src/budgets.rs`](xtask/src/budgets.rs).
Changing one is a reviewed change to that file.
