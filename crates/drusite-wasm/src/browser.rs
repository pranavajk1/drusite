// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the worker calls, through `wasm-bindgen`.

use std::cell::RefCell;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use drusite_core::api::Event;
use drusite_core::sync::Join;
use drusite_core::{Core, StoreKey};
use drusite_model::WorkspaceId;
use drusite_proto::{ClientMessage, Ping, ServerMessage, client_message, server_message};
use js_sys::{Array, Uint8Array};
use rsqlite_vfs::VfsFilesManager as _;
use sqlite_wasm_rs::WasmOsCallback;
use sqlite_wasm_vfs::sahpool::{OpfsSAHPoolCfgBuilder, OpfsSAHPoolUtil, install};
use wasm_bindgen::prelude::*;
// Here only for their browser features (randomness, clock).
use {getrandom as _, getrandom03 as _, uuid as _, wasm_bindgen_futures as _};

/// Where in OPFS the store's files are kept (the VFS's own folder of pooled files).
const OPFS_DIRECTORY: &str = ".drusite";
/// The store's "folder": a name inside the VFS, not a real path.
const DATA_DIR: &str = "/drusite";

thread_local! {
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
    /// The OPFS pool, once installed: kept to clear on sign-out.
    static POOL: RefCell<Option<OpfsSAHPoolUtil>> = const { RefCell::new(None) };
}
static CHANGED: AtomicBool = AtomicBool::new(false);

fn js_error(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

fn frames(messages: Vec<ClientMessage>) -> Array {
    messages
        .iter()
        .map(|m| JsValue::from(Uint8Array::from(drusite_proto::encode(m).as_slice())))
        .collect()
}

/// The core, in a worker.
#[wasm_bindgen]
#[derive(Debug)]
pub struct WasmCore {
    core: Core,
    persistent: bool,
}

#[wasm_bindgen]
impl WasmCore {
    /// Opens the store with `key` (32 bytes): in OPFS if `persistent` and the browser allows it,
    /// else in memory (gone when the worker ends). [`Self::persistent`] says which it got.
    pub async fn open(key: Vec<u8>, persistent: bool) -> Result<WasmCore, JsError> {
        let key: [u8; 32] = key
            .try_into()
            .map_err(|_| JsError::new("a store key is 32 bytes"))?;
        let key = StoreKey::from_bytes(key);
        let persistent = persistent && {
            let cfg = OpfsSAHPoolCfgBuilder::new()
                .directory(OPFS_DIRECTORY)
                .build();
            match install::<WasmOsCallback>(&cfg, true).await {
                Ok(util) => {
                    POOL.with(|p| *p.borrow_mut() = Some(util));
                    true
                }
                Err(e) => {
                    web_log(&format!(
                        "drusite: OPFS isn't available ({e}); keeping notes in memory"
                    ));
                    false
                }
            }
        };
        let sink = Box::new(|event: Event| EVENTS.with(|e| e.borrow_mut().push(event)));
        let mut core = Core::open(Path::new(DATA_DIR), &key, sink).map_err(js_error)?;
        core.set_change_listener(Some(Box::new(|| CHANGED.store(true, Ordering::SeqCst))));
        Ok(Self { core, persistent })
    }

    #[wasm_bindgen(getter)]
    pub fn persistent(&self) -> bool {
        self.persistent
    }

    /// Runs one command (JSON) and answers `{"ok": response}` or `{"err": error}`.
    pub fn handle(&mut self, command: &str) -> String {
        let result = crate::command(command).and_then(|c| match crate::unsupported_here(&c) {
            Some(e) => Err(e),
            None => self.core.handle(c),
        });
        crate::answer(result)
    }

    /// The core's events since the last call, as a JSON array.
    #[wasm_bindgen(js_name = takeEvents)]
    pub fn take_events(&mut self) -> String {
        let events = EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()));
        serde_json::to_string(&events).unwrap_or_else(|_| "[]".to_owned())
    }

    /// Whether something changed locally since the last call: then there's sync to push.
    #[wasm_bindgen(js_name = takeChanged)]
    pub fn take_changed(&mut self) -> bool {
        CHANGED.swap(false, Ordering::SeqCst)
    }

    /// Whether this browser holds nothing of its own yet (a first visit, or just signed out).
    #[wasm_bindgen(js_name = isBlank)]
    pub fn is_blank(&self) -> bool {
        self.core.is_blank().unwrap_or(false)
    }

    #[wasm_bindgen(js_name = workspaceId)]
    pub fn workspace_id(&self) -> String {
        self.core.workspace_id().to_string()
    }

    /// Which workspace to sync, from the account's (`[{"id": …, "role": …}]`, as the server lists
    /// them): `"own"`, `"conflict"`, or the id of the account's workspace to take.
    #[wasm_bindgen(js_name = workspaceToJoin)]
    pub fn workspace_to_join(&mut self, account: &str) -> Result<String, JsError> {
        #[derive(serde::Deserialize)]
        struct Info {
            id: WorkspaceId,
            role: String,
        }
        let list: Vec<Info> = serde_json::from_str(account).map_err(js_error)?;
        let ids: Vec<(WorkspaceId, bool)> =
            list.iter().map(|w| (w.id, w.role == "owner")).collect();
        Ok(match self.core.workspace_to_join(&ids).map_err(js_error)? {
            Join::Own => "own".to_owned(),
            Join::Conflict => "conflict".to_owned(),
            Join::Take(id) => id.to_string(),
        })
    }

    /// The server's version from its first frame, which must be a welcome.
    pub fn welcome(frame: &[u8]) -> Result<String, JsError> {
        let message: ServerMessage = drusite_proto::decode(frame).map_err(js_error)?;
        match message.body {
            Some(server_message::Body::Welcome(w)) => Ok(w.server_version),
            _ => Err(JsError::new("the server didn't start with a welcome")),
        }
    }

    /// Starts syncing: the frames to send first. `join` is the account's workspace to take in
    /// place of this blank device's own.
    #[wasm_bindgen(js_name = syncStart)]
    pub fn sync_start(&mut self, join: Option<String>) -> Result<Array, JsError> {
        let messages = match join {
            Some(id) => {
                let id: WorkspaceId = id.parse().map_err(js_error)?;
                self.core.sync_start_join(id)
            }
            None => self.core.sync_start(),
        }
        .map_err(js_error)?;
        Ok(frames(messages))
    }

    /// One frame from the server: the frames to send back. An error ends the connection.
    #[wasm_bindgen(js_name = syncReceive)]
    pub fn sync_receive(&mut self, frame: &[u8]) -> Result<Array, JsError> {
        let message: ServerMessage = drusite_proto::decode(frame).map_err(js_error)?;
        Ok(frames(self.core.sync_receive(message).map_err(js_error)?))
    }

    /// What changed here since: the frames to push.
    #[wasm_bindgen(js_name = syncOutgoing)]
    pub fn sync_outgoing(&mut self) -> Result<Array, JsError> {
        Ok(frames(self.core.sync_outgoing().map_err(js_error)?))
    }

    /// Now and then while connected: anything unanswered, sent again.
    #[wasm_bindgen(js_name = syncRetry)]
    pub fn sync_retry(&mut self) -> Result<Array, JsError> {
        Ok(frames(self.core.sync_retry().map_err(js_error)?))
    }

    #[wasm_bindgen(js_name = syncStop)]
    pub fn sync_stop(&mut self) {
        self.core.sync_stop();
    }

    /// A ping frame, to keep proxies from closing an idle connection.
    pub fn ping(nonce: u64) -> Uint8Array {
        let message = ClientMessage::new(client_message::Body::Ping(Ping { nonce }));
        Uint8Array::from(drusite_proto::encode(&message).as_slice())
    }

    /// How a page is shared as the workspace last heard (when the server can't be asked), as
    /// the JSON of a `ShareInfo`.
    #[wasm_bindgen(js_name = localShare)]
    pub fn local_share(&self, page: &str) -> String {
        let result = page
            .parse()
            .map_err(|_| {
                drusite_core::api::ApiError::new(
                    drusite_core::api::ErrorCode::Invalid,
                    "not a page",
                )
            })
            .and_then(|page| self.core.local_share(page));
        match result {
            Ok(share) => serde_json::to_string(&share).unwrap_or_default(),
            Err(e) => crate::answer(Err(e)),
        }
    }

    // ---- Attachments, which move over HTTP beside the connection ----

    #[wasm_bindgen(js_name = attachmentsToUpload)]
    pub fn attachments_to_upload(&self) -> Vec<String> {
        self.core.attachments_to_upload().unwrap_or_default()
    }

    #[wasm_bindgen(js_name = attachmentsWanted)]
    pub fn attachments_wanted(&self) -> Vec<String> {
        self.core.attachments_wanted()
    }

    pub fn attachment(&self, blob: &str) -> Option<Vec<u8>> {
        self.core.attachment(blob).ok().flatten()
    }

    #[wasm_bindgen(js_name = attachmentUploaded)]
    pub fn attachment_uploaded(&mut self, blob: &str) {
        let _ = self.core.attachment_uploaded(blob);
    }

    #[wasm_bindgen(js_name = attachmentArrived)]
    pub fn attachment_arrived(&mut self, blob: &str, bytes: &[u8]) -> Result<(), JsError> {
        self.core
            .attachment_arrived(blob, bytes)
            .map_err(|e| JsError::new(&e.message))
    }

    #[wasm_bindgen(js_name = attachmentUnavailable)]
    pub fn attachment_unavailable(&mut self, blob: &str) {
        self.core.attachment_unavailable(blob);
    }
}

/// Deletes the store's files from OPFS (signing out). The core must be closed first (`free()`).
#[wasm_bindgen(js_name = wipeStore)]
pub fn wipe_store() -> Result<(), JsError> {
    POOL.with(|p| match p.borrow().as_ref() {
        Some(pool) => pool.clear().map_err(js_error),
        None => Ok(()),
    })
}

fn web_log(message: &str) {
    web_console(message);
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = warn)]
    fn web_console(message: &str);
}
