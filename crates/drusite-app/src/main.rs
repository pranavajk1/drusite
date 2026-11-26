// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Drusite desktop and mobile app. Phase 1 step 5 replaces this stub with the Tauri 2 shell.
//!
//! The stub already speaks the benchmark contract `cargo xtask budgets ram` relies on, so the
//! harness and CI job are exercised end to end before the real app exists:
//!
//! - `DRUSITE_DATA_DIR`: the workspace to open (the harness points it at a generated fixture).
//! - `DRUSITE_BENCH_READY_FILE`: once the app is idle with a page open, it creates this file.

use std::time::Duration;

fn main() {
    let Some(ready_file) = std::env::var_os("DRUSITE_BENCH_READY_FILE") else {
        eprintln!("drusite-app: the desktop app arrives in Phase 1 step 5");
        std::process::exit(2);
    };
    if let Err(err) = std::fs::write(&ready_file, b"ready\n") {
        eprintln!("drusite-app: cannot write the ready file: {err}");
        std::process::exit(1);
    }
    // Idle until the harness kills us.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
