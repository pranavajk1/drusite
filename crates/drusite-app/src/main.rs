// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Drusite app: the Tauri 2 shell around the core.
//!
//! This crate is only an adapter. It carries the Core API ([`drusite_core::api`]) over Tauri's
//! IPC as a single command, `core`, and forwards the core's events to the UI as the Tauri event
//! [`EVENT`]. The UI talks to a transport interface and never to Tauri itself
//! (`ui/src/core/tauri.ts` is the one file that does).
//!
//! Environment, for benchmarks and tests:
//! - `DRUSITE_DATA_DIR`: the folder holding the store, instead of the app's data folder.
//! - `DRUSITE_BENCH_KEY_FILE`: the store key as hex, instead of the Keychain.
//! - `DRUSITE_BENCH_READY_FILE`: created once the UI reports it is ready (`cargo xtask budgets
//!   ram` waits for it).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Mutex;

use drusite_core::api::{ApiError, Command, ErrorCode, Response};
use drusite_core::{Core, StoreKey};
use tauri::{Emitter, Manager};

/// The Tauri event the core's events are sent on.
const EVENT: &str = "drusite://event";

struct AppState {
    core: Mutex<Core>,
    ready_file: Option<PathBuf>,
}

impl AppState {
    fn mark_ready(&self) {
        if let Some(path) = &self.ready_file
            && let Err(err) = std::fs::write(path, b"ready\n")
        {
            eprintln!("drusite: can't write the ready file: {err}");
        }
    }
}

/// The whole Core API, as one IPC command.
#[tauri::command]
async fn core(state: tauri::State<'_, AppState>, command: Command) -> Result<Response, ApiError> {
    let ready = matches!(command, Command::Ready);
    let response = state
        .core
        .lock()
        .map_err(|_| {
            ApiError::new(
                ErrorCode::Internal,
                "the core crashed earlier; restart Drusite",
            )
        })?
        .handle(command);
    if ready {
        state.mark_ready();
    }
    response
}

/// The store key: from the Keychain, or a file for benchmarks.
fn store_key() -> Result<StoreKey, Box<dyn std::error::Error>> {
    if let Some(file) = std::env::var_os("DRUSITE_BENCH_KEY_FILE") {
        return Ok(StoreKey::from_hex(&std::fs::read_to_string(file)?)?);
    }
    #[cfg(target_os = "macos")]
    {
        Ok(drusite_core::key::keychain::load_or_create(
            drusite_core::key::keychain::SERVICE,
            "workspace",
        )?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("this platform has no store key support yet (Phase 1 is macOS only)".into())
    }
}

fn main() {
    let result = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![core])
        .setup(|app| {
            let data_dir = match std::env::var_os("DRUSITE_DATA_DIR") {
                Some(dir) => PathBuf::from(dir),
                None => app.path().app_data_dir()?,
            };
            let key = store_key()?;
            let handle = app.handle().clone();
            let core = Core::open(
                &data_dir,
                &key,
                Box::new(move |event| {
                    if let Err(err) = handle.emit(EVENT, event) {
                        eprintln!("drusite: can't send an event to the UI: {err}");
                    }
                }),
            )?;
            app.manage(AppState {
                core: Mutex::new(core),
                ready_file: std::env::var_os("DRUSITE_BENCH_READY_FILE").map(PathBuf::from),
            });
            Ok(())
        })
        .run(tauri::generate_context!());
    if let Err(err) = result {
        eprintln!("drusite: {err}");
        std::process::exit(1);
    }
}
