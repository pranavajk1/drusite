// SPDX-License-Identifier: AGPL-3.0-or-later

//! The browser adapter: the same core as WebAssembly, its encrypted store in OPFS.
//!
//! It runs in a dedicated worker (OPFS's synchronous file handles exist only there), and is
//! driven from JavaScript (`ui/src/core/web/`): commands in and responses out as the same JSON
//! the Tauri adapter passes, and the sync session's frames in and out of a `WebSocket` the
//! worker owns. Nothing here does I/O of its own beyond the store.
//!
//! The core's events and its "something changed" signal are queued and drained by the worker
//! after every call, rather than calling into JavaScript from inside the core.

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod browser;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub use browser::{WasmCore, wipe_store};

use drusite_core::api::{ApiError, Command, Response};

/// What a command answers, as the worker passes it on: the response, or the core's error.
pub fn answer(result: Result<Response, ApiError>) -> String {
    let value = match result {
        Ok(response) => serde_json::json!({ "ok": response }),
        Err(error) => serde_json::json!({ "err": error }),
    };
    value.to_string()
}

/// A command from its JSON, or the error to answer with.
pub fn command(json: &str) -> Result<Command, ApiError> {
    serde_json::from_str(json).map_err(|e| {
        ApiError::new(
            drusite_core::api::ErrorCode::Invalid,
            format!("not a command: {e}"),
        )
    })
}

/// The commands the browser answers itself, or that it has no way to carry out: everything that
/// needs the server's account (the worker does that over the session cookie) or the device.
pub fn unsupported_here(command: &Command) -> Option<ApiError> {
    use drusite_core::api::ErrorCode::Unsupported;
    let message = match command {
        Command::BridgeStatus | Command::ImportFolder { .. } | Command::SetBridgeFolder { .. } => {
            "a browser has no folders to keep Markdown in"
        }
        Command::ImportNotion { .. } => "choose the export's .zip file instead",
        Command::ImportNotionApi { .. } => "importing from Notion's API needs the Drusite app",
        _ => return None,
    };
    Some(ApiError::new(Unsupported, message))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn commands_and_answers_are_the_apps_json() {
        let c = command(r#"{"type":"listPages"}"#).unwrap();
        assert!(matches!(c, Command::ListPages));
        assert!(command("{}").is_err());
        let a = answer(Ok(Response::Done));
        assert_eq!(a, r#"{"ok":{"type":"done"}}"#);
        let e = answer(Err(ApiError::new(
            drusite_core::api::ErrorCode::Invalid,
            "no",
        )));
        assert!(e.starts_with(r#"{"err":{"code":"invalid""#), "{e}");
        assert!(
            unsupported_here(&Command::ImportFolder {
                folder: "/x".into()
            })
            .is_some()
        );
        assert!(unsupported_here(&Command::ListPages).is_none());
    }
}
