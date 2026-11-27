# Working in Drusite

[`CONTRIBUTING.md`](CONTRIBUTING.md) has the checks, the test setup and the phone builds.

## Clean up build output when your work is done

Builds here are large. One worktree's `target/` reaches 10–40 GB once it has debug, release,
Android and iOS Simulator output, and a few worktrees filled the Mac by over 250 GB in two days.

When your PR merges, or the work is abandoned, and nothing of yours is still running from the
worktree (a test server, an app bundle, a Gradle build in Android Studio):

```bash
cargo clean
rm -rf crates/drusite-app/gen/android/app/build crates/drusite-app/gen/apple/build
```

Leave the worktree itself, `ui/node_modules` and anything outside it alone; removing worktrees
is the weekly `dev-clean` job's work on Pranava's Mac. In the main checkout, never delete
`target/release`: the installed `Drusite.app` runs from `target/release/bundle/macos/`.

Docker containers and volumes you started for tests: remove them when you're done unless
another session uses them, and say in your hand-over note which ones you left running.
