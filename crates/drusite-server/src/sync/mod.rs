// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sync: devices keep documents in step over a WebSocket, one subscription per document.
//!
//! The protocol is in `drusite-proto`. Pushed updates go into each document's log in Postgres
//! ([`store`]). Every node listens on one Postgres channel; a committed push notifies all of them,
//! and each connection then reads from the database whatever it hasn't sent yet. So any node can
//! serve any device, and nothing is skipped or delivered out of order.

mod connection;
pub mod store;

use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::{AuthError, Authenticated};

/// The Postgres channel pushes are announced on. The payload is the document's ID.
pub const CHANNEL: &str = "drusite_sync";

/// Announced after the listener reconnects: notifications may have been missed, so every
/// connection checks all its documents.
pub const EVERYTHING: Uuid = Uuid::nil();

/// Announcements of changed documents, from this node's listener to its connections.
#[derive(Debug)]
pub struct Hub {
    tx: broadcast::Sender<Uuid>,
    compact_tx: tokio::sync::mpsc::Sender<Uuid>,
    compact_rx: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<Uuid>>>,
}

impl Default for Hub {
    fn default() -> Self {
        let (compact_tx, compact_rx) = tokio::sync::mpsc::channel(1024);
        Self {
            tx: broadcast::channel(4096).0,
            compact_tx,
            compact_rx: std::sync::Mutex::new(Some(compact_rx)),
        }
    }
}

impl Hub {
    /// Asks the compactor to fold `doc`'s log. A full queue just means a later push asks again.
    pub fn request_compaction(&self, doc: Uuid) {
        let _ = self.compact_tx.try_send(doc);
    }

    /// Starts the listener and the compactor.
    pub fn spawn_workers(state: &AppState) -> [tokio::task::JoinHandle<()>; 2] {
        [
            spawn_listener(state.clone()),
            spawn_compactor(state.clone()),
        ]
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Uuid> {
        self.tx.subscribe()
    }

    fn announce(&self, doc: Uuid) {
        // No receivers is fine: nobody on this node has the document open.
        let _ = self.tx.send(doc);
    }
}

/// Listens on [`CHANNEL`] for as long as the server runs, reconnecting when the connection
/// drops.
pub fn spawn_listener(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut first = true;
        loop {
            match sqlx::postgres::PgListener::connect_with(state.db().pool()).await {
                Ok(mut listener) => {
                    if let Err(error) = listener.listen(CHANNEL).await {
                        tracing::warn!(%error, "listening for sync announcements");
                    } else {
                        if !first {
                            state.hub().announce(EVERYTHING);
                        }
                        first = false;
                        loop {
                            match listener.try_recv().await {
                                Ok(Some(n)) => {
                                    if let Ok(doc) = n.payload().parse::<Uuid>() {
                                        state.hub().announce(doc);
                                    }
                                }
                                // The connection dropped; sqlx reconnects on the next call, but
                                // anything sent meanwhile is lost, so say so.
                                Ok(None) => state.hub().announce(EVERYTHING),
                                Err(error) => {
                                    tracing::warn!(%error, "sync listener failed");
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(error) => tracing::warn!(%error, "connecting the sync listener"),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
}

/// Compacts documents whose logs have grown, one at a time, in the background.
pub fn spawn_compactor(state: AppState) -> tokio::task::JoinHandle<()> {
    let receiver = state
        .hub()
        .compact_rx
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    tokio::spawn(async move {
        let Some(mut rx) = receiver else { return };
        while let Some(doc) = rx.recv().await {
            if let Err(error) = store::compact(&state, doc).await {
                tracing::error!(%doc, %error, "compacting a document");
            }
        }
    })
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceInfo {
    id: Uuid,
    role: String,
}

/// `GET /api/v1/workspaces`: the signed-in account's workspaces, so a new device syncs the
/// existing one instead of starting its own.
async fn workspaces(
    State(state): State<AppState>,
    auth: Authenticated,
) -> Result<Json<Vec<WorkspaceInfo>>, AuthError> {
    let list = store::workspaces(&state, auth.user)
        .await
        .map_err(|e| AuthError::internal(anyhow::anyhow!("{e}")))?;
    Ok(Json(
        list.into_iter()
            .map(|(id, role)| WorkspaceInfo { id, role })
            .collect(),
    ))
}

pub const SYNC_PATH: &str = "/api/v1/sync";

/// The sync routes. The WebSocket must not sit under the request timeout.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(SYNC_PATH, get(connection::upgrade))
        .route("/api/v1/workspaces", get(workspaces))
}
