// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Drusite app: the Tauri 2 shell around the core, on desktops and phones.
//!
//! This crate is only an adapter. It carries the Core API ([`drusite_core::api`]) over Tauri's
//! IPC as a single command, `core`, and forwards the core's events to the UI as the Tauri event
//! [`EVENT`]. The UI talks to a transport interface and never to Tauri itself
//! (`ui/src/core/tauri.ts` is the one file that does).
//!
//! The account commands (signing in and out, and the account's status) are answered here rather
//! than by the core, since they need the browser and the device's key store
//! (`tauri-plugin-drusite`); the sync connection itself is [`drusite_core::account`]'s.
//!
//! Environment, for benchmarks and tests:
//! - `DRUSITE_DATA_DIR`: the folder holding the store, instead of the app's data folder.
//! - `DRUSITE_BENCH_KEY_FILE`: the store key as hex, instead of the platform's key store. The
//!   device key is then a throwaway one in memory.
//! - `DRUSITE_BENCH_READY_FILE`: created once the UI reports it is ready (`cargo xtask budgets
//!   ram` waits for it).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use drusite_core::account::{SharedKey, Supervisor};
use drusite_core::api::{ApiError, Command, ErrorCode, Event, Response};
use drusite_core::{Core, StoreKey};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_drusite::DrusiteExt;
use tokio::sync::OnceCell;

/// The Tauri event the core's events are sent on.
const EVENT: &str = "drusite://event";

/// The core and the connection, once the store is open.
struct Ready {
    core: Arc<Mutex<Core>>,
    supervisor: Supervisor,
    /// The Markdown bridge, while it's on.
    #[cfg(desktop)]
    bridge: Mutex<Option<drusite_core::bridge::watch::Watch>>,
    /// The sign-in waiting in the browser, if any.
    sign_in: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

struct AppState {
    handle: AppHandle,
    /// Opened off the main thread, since the key stores may call into the platform (which on
    /// phones runs on the main thread). Commands wait for it.
    ready: OnceCell<Ready>,
    ready_file: Option<PathBuf>,
}

fn internal(message: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, message.to_string())
}

fn emit(handle: &AppHandle, event: Event) {
    if let Err(err) = handle.emit(EVENT, event) {
        eprintln!("drusite: can't send an event to the UI: {err}");
    }
}

/// The store key and the device key.
fn keys(handle: &AppHandle) -> Result<(StoreKey, SharedKey), String> {
    if let Some(file) = std::env::var_os("DRUSITE_BENCH_KEY_FILE") {
        let hex = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
        let store = StoreKey::from_hex(&hex).map_err(|e| e.to_string())?;
        let device = drusite_dpop::SoftwareKey::generate().map_err(|e| e.to_string())?;
        return Ok((store, Arc::new(device)));
    }
    let plugin = handle.drusite();
    let store = plugin.store_key().map_err(|e| e.to_string())?;
    let device = plugin.device_key().map_err(|e| e.to_string())?;
    Ok((store, device))
}

async fn open(handle: AppHandle) -> Result<Ready, ApiError> {
    let opening = handle.clone();
    let (core, key, device_name) = tauri::async_runtime::spawn_blocking(move || {
        let handle = opening;
        let data_dir = match std::env::var_os("DRUSITE_DATA_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => handle.path().app_data_dir().map_err(|e| e.to_string())?,
        };
        let (store_key, device_key) = keys(&handle)?;
        let events = handle.clone();
        let core = Core::open(
            &data_dir,
            &store_key,
            Box::new(move |event| emit(&events, event)),
        )
        .map_err(|e| e.to_string())?;
        Ok::<_, String>((core, device_key, handle.drusite().device_name()))
    })
    .await
    .map_err(internal)?
    .map_err(|e| ApiError::new(ErrorCode::Storage, format!("can't open the notes: {e}")))?;

    let core = Arc::new(Mutex::new(core));
    let events = handle.clone();
    let supervisor = Supervisor::start(
        &tokio::runtime::Handle::current(),
        Arc::clone(&core),
        key,
        device_name,
        move |status| {
            emit(
                &events,
                Event::AccountChanged {
                    status: status.clone(),
                },
            );
        },
    );
    let ready = Ready {
        core,
        supervisor,
        #[cfg(desktop)]
        bridge: Mutex::new(None),
        sign_in: Mutex::new(None),
    };
    #[cfg(desktop)]
    ready.start_bridge();
    Ok(ready)
}

impl AppState {
    async fn ready(&self) -> Result<&Ready, ApiError> {
        self.ready
            .get_or_try_init(|| open(self.handle.clone()))
            .await
    }

    fn mark_ready(&self) {
        if let Some(path) = &self.ready_file
            && let Err(err) = std::fs::write(path, b"ready\n")
        {
            eprintln!("drusite: can't write the ready file: {err}");
        }
    }
}

fn account_error(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Account, e.to_string())
}

impl Ready {
    /// Runs the Markdown bridge if it has a folder.
    #[cfg(desktop)]
    fn start_bridge(&self) {
        let on = self
            .core
            .lock()
            .ok()
            .and_then(|c| c.bridge_folder().ok().flatten())
            .is_some();
        let watch = if on {
            match drusite_core::bridge::watch::watch(Arc::clone(&self.core), |_| {}) {
                Ok(w) => Some(w),
                Err(e) => {
                    eprintln!("drusite: can't watch the Markdown folder: {}", e.message);
                    None
                }
            }
        } else {
            None
        };
        if let Ok(mut current) = self.bridge.lock() {
            *current = watch;
        }
    }

    #[cfg(desktop)]
    fn stop_bridge(&self) {
        let old = self.bridge.lock().ok().and_then(|mut b| b.take());
        drop(old);
    }

    fn replace_sign_in(&self, task: Option<tauri::async_runtime::JoinHandle<()>>) {
        if let Ok(mut current) = self.sign_in.lock()
            && let Some(old) = std::mem::replace(&mut *current, task)
        {
            old.abort();
        }
    }

    async fn sign_in(&self, handle: &AppHandle, server: &str) -> Result<Response, ApiError> {
        self.replace_sign_in(None);
        handle
            .drusite()
            .prepare_to_reach(server)
            .await
            .map_err(account_error)?;
        let redirect = handle.drusite().redirect().await.map_err(account_error)?;
        let url = self
            .supervisor
            .begin_sign_in(server, redirect.uri())
            .await
            .map_err(account_error)?;
        let supervisor = self.supervisor.clone();
        let task = tauri::async_runtime::spawn(async move {
            match redirect.open(&url).await {
                Ok(callback) => {
                    // A failure is reported in the account status.
                    let _ = supervisor.finish_sign_in(&callback).await;
                }
                Err(tauri_plugin_drusite::Error::Cancelled) => supervisor.cancel_sign_in(),
                Err(e) => supervisor.sign_in_failed(&e.to_string()),
            }
        });
        self.replace_sign_in(Some(task));
        Ok(Response::Done)
    }
}

/// The whole Core API, as one IPC command.
#[tauri::command]
async fn core(state: tauri::State<'_, AppState>, command: Command) -> Result<Response, ApiError> {
    let ready = state.ready().await?;
    match command {
        Command::AccountStatus => Ok(Response::Account {
            status: ready.supervisor.status(),
        }),
        Command::SignIn { server } => ready.sign_in(&state.handle, &server).await,
        Command::CancelSignIn => {
            ready.replace_sign_in(None);
            state.handle.drusite().cancel_sign_in();
            ready.supervisor.cancel_sign_in();
            Ok(Response::Done)
        }
        Command::SignOut => {
            ready.supervisor.sign_out().await.map_err(account_error)?;
            Ok(Response::Done)
        }
        #[cfg(mobile)]
        Command::BridgeStatus | Command::SetBridgeFolder { .. } => Err(ApiError::new(
            ErrorCode::Unsupported,
            "the Markdown folder is on desktops only",
        )),
        #[cfg(desktop)]
        Command::SetBridgeFolder { folder } => {
            ready.stop_bridge();
            let core = Arc::clone(&ready.core);
            // The first round reads and writes every file: off the async threads.
            let response = tauri::async_runtime::spawn_blocking(move || {
                core.lock()
                    .map_err(|_| internal("the core crashed earlier; restart Drusite"))?
                    .handle(Command::SetBridgeFolder { folder })
            })
            .await
            .map_err(internal)?;
            ready.start_bridge();
            response
        }
        command => {
            let is_ready = matches!(command, Command::Ready);
            let response = ready
                .core
                .lock()
                .map_err(|_| internal("the core crashed earlier; restart Drusite"))?
                .handle(command);
            if is_ready {
                state.mark_ready();
            }
            response
        }
    }
}

/// The folder picker, on desktops; nothing on phones.
#[cfg(desktop)]
fn dialog() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_dialog::init()
}

#[cfg(mobile)]
fn dialog() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("no-dialog").build()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_drusite::init())
        .plugin(dialog())
        .invoke_handler(tauri::generate_handler![core])
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(AppState {
                handle: handle.clone(),
                ready: OnceCell::new(),
                ready_file: std::env::var_os("DRUSITE_BENCH_READY_FILE").map(PathBuf::from),
            });
            // Start opening now rather than at the UI's first command.
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                if let Err(err) = state.ready().await {
                    eprintln!("drusite: {err}");
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Back in the foreground: the connection likely died while the phone slept.
            #[cfg(mobile)]
            if matches!(event, tauri::WindowEvent::Resumed)
                && let Some(ready) = window.state::<AppState>().ready.get()
            {
                ready.supervisor.kick();
            }
            #[cfg(desktop)]
            let _ = (window, event);
        })
        .run(tauri::generate_context!());
    if let Err(err) = result {
        eprintln!("drusite: {err}");
        std::process::exit(1);
    }
}
