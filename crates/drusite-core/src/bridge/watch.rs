// SPDX-License-Identifier: AGPL-3.0-or-later

//! Running the bridge on its own: a round whenever the folder or a page changes, and every
//! half minute in case a change went unnoticed.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::new_debouncer;

use super::Report;
use crate::api::{ApiError, ErrorCode};
use crate::engine::Core;

/// How long changes must settle before a round, so a burst of typing is one round.
const SETTLE: Duration = Duration::from_millis(400);
/// A round at least this often.
const AT_LEAST_EVERY: Duration = Duration::from_secs(30);

enum Signal {
    Changed,
    Stop,
}

/// The running bridge. Dropping it stops it.
pub struct Watch {
    signal: mpsc::Sender<Signal>,
    thread: Option<JoinHandle<()>>,
    core: Arc<Mutex<Core>>,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch").finish_non_exhaustive()
    }
}

/// Starts the bridge for the core's folder: a first round now, then one after each change.
/// Each round is recorded in the core ([`Core::bridge_record`]); `report` hears it too.
pub fn watch(
    core: Arc<Mutex<Core>>,
    report: impl Fn(&Result<Report, ApiError>) + Send + 'static,
) -> Result<Watch, ApiError> {
    fn lock(core: &Mutex<Core>) -> Result<std::sync::MutexGuard<'_, Core>, ApiError> {
        core.lock()
            .map_err(|_| ApiError::new(ErrorCode::Internal, "the core is unusable"))
    }
    let folder = lock(&core)?
        .bridge_folder()?
        .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "the Markdown bridge is off"))?;
    let (signal, signals) = mpsc::channel();
    let on_fs = signal.clone();
    let mut debouncer = new_debouncer(SETTLE, None, move |_| {
        let _ = on_fs.send(Signal::Changed);
    })
    .map_err(|e| ApiError::new(ErrorCode::Storage, e.to_string()))?;
    debouncer
        .watch(&folder, RecursiveMode::Recursive)
        .map_err(|e| ApiError::new(ErrorCode::Storage, e.to_string()))?;
    let on_page = signal.clone();
    lock(&core)?.on_bridge_change = Some(Box::new(move || {
        let _ = on_page.send(Signal::Changed);
    }));
    let worker = Arc::clone(&core);
    let thread = std::thread::Builder::new()
        .name("drusite-bridge".into())
        .spawn(move || {
            // Kept alive as long as the thread runs.
            let _debouncer = debouncer;
            loop {
                match worker.lock() {
                    Ok(mut core) => {
                        let result = core.bridge_sync();
                        report(&result);
                        core.bridge_record(result);
                    }
                    Err(_) => return,
                }
                match signals.recv_timeout(AT_LEAST_EVERY) {
                    Ok(Signal::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                    Ok(Signal::Changed) | Err(RecvTimeoutError::Timeout) => {}
                }
                // Let the burst finish.
                loop {
                    match signals.recv_timeout(SETTLE) {
                        Ok(Signal::Changed) => {}
                        Ok(Signal::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                        Err(RecvTimeoutError::Timeout) => break,
                    }
                }
            }
        })
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(Watch {
        signal,
        thread: Some(thread),
        core,
    })
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.signal.send(Signal::Stop);
        if let Ok(mut core) = self.core.lock() {
            core.on_bridge_change = None;
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
