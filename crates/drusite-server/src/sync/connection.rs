// SPDX-License-Identifier: AGPL-3.0-or-later

//! One device's WebSocket.

use std::collections::HashMap;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use drusite_model::UserId;
use drusite_proto::{
    Ack, ClientMessage, DocKind, ErrorCode, MAX_FRAME_BYTES, PROTOCOL_VERSION, Pong, Push,
    SUBPROTOCOL, ServerMessage, Subscribe, Subscribed, Updates, Welcome, client_message,
    server_message::Body,
};
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use super::EVERYTHING;
use super::store::{self, DocError};
use crate::app::AppState;
use crate::auth::Authenticated;
use crate::auth::tokens::{self, GrantId};

/// How often a connection checks its device is still signed in.
const RECHECK: Duration = Duration::from_secs(60);
/// Close codes (4000–4999 are the application's).
const CLOSE_SIGNED_OUT: u16 = 4001;

pub(super) async fn upgrade(
    State(state): State<AppState>,
    auth: Authenticated,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let offered = headers
        .get_all("sec-websocket-protocol")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|p| p.trim() == SUBPROTOCOL);
    if !offered {
        return (
            StatusCode::BAD_REQUEST,
            format!("offer the WebSocket subprotocol {SUBPROTOCOL}"),
        )
            .into_response();
    }
    ws.protocols([SUBPROTOCOL])
        .max_message_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            let mut connection = Connection {
                state,
                user: auth.user,
                grant: auth.grant,
                can_write: auth.can_write(),
                origin: Uuid::now_v7(),
                subscriptions: HashMap::new(),
                socket,
            };
            if let Err(error) = connection.run().await {
                tracing::debug!(%error, "sync connection ended");
            }
        })
}

#[derive(Debug)]
struct Subscription {
    workspace: Uuid,
    /// The highest sequence number sent (or skipped as this connection's own).
    cursor: i64,
    writable: bool,
}

struct Connection {
    state: AppState,
    user: UserId,
    grant: GrantId,
    /// The token's scope allows writing.
    can_write: bool,
    /// Marks this connection's pushes, so they aren't sent back to it.
    origin: Uuid,
    subscriptions: HashMap<Uuid, Subscription>,
    socket: WebSocket,
}

fn error(code: ErrorCode, message: impl Into<String>, doc: &[u8], push_id: u64) -> ServerMessage {
    ServerMessage::new(Body::Error(drusite_proto::Error {
        code: code as i32,
        message: message.into(),
        doc: doc.to_vec(),
        push_id,
    }))
}

fn doc_error(e: &DocError, doc: &[u8], push_id: u64) -> ServerMessage {
    let code = match e {
        DocError::Forbidden => ErrorCode::Forbidden,
        DocError::NotFound => ErrorCode::NotFound,
        DocError::Invalid(_) => ErrorCode::InvalidUpdate,
        DocError::TooLarge => ErrorCode::TooLarge,
        DocError::Server(cause) => {
            tracing::error!(error = format!("{cause:#}"), "sync request failed");
            return error(
                ErrorCode::Internal,
                "something went wrong on the server",
                doc,
                push_id,
            );
        }
    };
    error(code, e.to_string(), doc, push_id)
}

fn uuid(bytes: &[u8]) -> Option<Uuid> {
    Uuid::from_slice(bytes).ok()
}

impl Connection {
    async fn send(&mut self, message: &ServerMessage) -> anyhow::Result<()> {
        self.socket
            .send(Message::Binary(drusite_proto::encode(message).into()))
            .await?;
        Ok(())
    }

    async fn run(&mut self) -> anyhow::Result<()> {
        self.send(&ServerMessage::new(Body::Welcome(Welcome {
            protocol_version: PROTOCOL_VERSION,
            server_version: env!("CARGO_PKG_VERSION").to_owned(),
            max_frame_bytes: u32::try_from(MAX_FRAME_BYTES).unwrap_or(u32::MAX),
        })))
        .await?;
        let mut announcements = self.state.hub().subscribe();
        let mut recheck = tokio::time::interval(RECHECK);
        recheck.tick().await;
        loop {
            tokio::select! {
                incoming = self.socket.recv() => {
                    match incoming {
                        Some(Ok(Message::Binary(frame))) => self.handle_frame(&frame).await?,
                        Some(Ok(Message::Close(_))) | None => return Ok(()),
                        Some(Ok(_)) => {} // text, ping and pong carry nothing for us
                        Some(Err(e)) => return Err(e.into()),
                    }
                }
                announced = announcements.recv() => {
                    match announced {
                        Ok(doc) if doc == EVERYTHING => self.deliver_all().await?,
                        Ok(doc) => {
                            if self.subscriptions.contains_key(&doc) {
                                self.deliver(doc).await?;
                            }
                        }
                        // Fell behind: some announcements are gone, so check everything.
                        Err(RecvError::Lagged(_)) => self.deliver_all().await?,
                        Err(RecvError::Closed) => return Ok(()),
                    }
                }
                _ = recheck.tick() => {
                    if !tokens::grant_is_live(&self.state, self.grant).await.unwrap_or(true) {
                        let _ = self.socket.send(Message::Close(Some(CloseFrame {
                            code: CLOSE_SIGNED_OUT,
                            reason: "signed out".into(),
                        }))).await;
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn handle_frame(&mut self, frame: &[u8]) -> anyhow::Result<()> {
        let message: ClientMessage = match drusite_proto::decode(frame) {
            Ok(m) => m,
            Err(e) => {
                return self
                    .send(&error(ErrorCode::BadRequest, e.to_string(), &[], 0))
                    .await;
            }
        };
        match message.body {
            Some(client_message::Body::Subscribe(s)) => self.subscribe(s).await,
            Some(client_message::Body::Unsubscribe(u)) => {
                if let Some(doc) = uuid(&u.doc) {
                    self.subscriptions.remove(&doc);
                }
                Ok(())
            }
            Some(client_message::Body::Push(p)) => self.push(p).await,
            Some(client_message::Body::Ping(p)) => {
                self.send(&ServerMessage::new(Body::Pong(Pong { nonce: p.nonce })))
                    .await
            }
            None => {
                self.send(&error(ErrorCode::BadRequest, "empty message", &[], 0))
                    .await
            }
        }
    }

    async fn subscribe(&mut self, s: Subscribe) -> anyhow::Result<()> {
        let (Some(doc), Some(workspace)) = (uuid(&s.doc), uuid(&s.workspace)) else {
            return self
                .send(&error(ErrorCode::BadRequest, "IDs are 16 bytes", &s.doc, 0))
                .await;
        };
        let kind = DocKind::try_from(s.kind).unwrap_or(DocKind::Unspecified);
        if kind == DocKind::Unspecified {
            return self
                .send(&error(ErrorCode::BadRequest, "no document kind", &s.doc, 0))
                .await;
        }
        let create = if self.can_write {
            s.create.as_slice()
        } else {
            &[]
        };
        let opened = match store::open(
            &self.state,
            self.user,
            doc,
            workspace,
            kind,
            create,
            self.origin,
        )
        .await
        {
            Ok(o) => o,
            Err(e) => return self.send(&doc_error(&e, &s.doc, 0)).await,
        };
        let after = i64::try_from(s.after_seq).unwrap_or(i64::MAX);
        self.subscriptions.insert(
            doc,
            Subscription {
                workspace: opened.workspace,
                cursor: after,
                writable: opened.writable && self.can_write,
            },
        );
        // The catch-up includes the creating snapshot only if the device didn't send it.
        let skip = opened.created.then_some(self.origin);
        let head = self.deliver_from(doc, skip).await?;
        self.send(&ServerMessage::new(Body::Subscribed(Subscribed {
            doc: s.doc,
            head_seq: u64::try_from(head).unwrap_or_default(),
            created: opened.created,
            writable: opened.writable && self.can_write,
        })))
        .await
    }

    async fn push(&mut self, p: Push) -> anyhow::Result<()> {
        let Some(doc) = uuid(&p.doc) else {
            return self
                .send(&error(
                    ErrorCode::BadRequest,
                    "IDs are 16 bytes",
                    &p.doc,
                    p.push_id,
                ))
                .await;
        };
        let Some(sub) = self.subscriptions.get(&doc) else {
            return self
                .send(&error(
                    ErrorCode::NotSubscribed,
                    "subscribe first",
                    &p.doc,
                    p.push_id,
                ))
                .await;
        };
        if !sub.writable {
            return self
                .send(&doc_error(&DocError::Forbidden, &p.doc, p.push_id))
                .await;
        }
        let workspace = sub.workspace;
        match store::append(
            &self.state,
            self.user,
            doc,
            workspace,
            &p.update,
            self.origin,
        )
        .await
        {
            Ok((seq, compact)) => {
                if compact {
                    self.state.hub().request_compaction(doc);
                }
                self.send(&ServerMessage::new(Body::Ack(Ack {
                    doc: p.doc,
                    push_id: p.push_id,
                    seq: u64::try_from(seq).unwrap_or_default(),
                })))
                .await
            }
            Err(e) => self.send(&doc_error(&e, &p.doc, p.push_id)).await,
        }
    }

    async fn deliver_all(&mut self) -> anyhow::Result<()> {
        let docs: Vec<Uuid> = self.subscriptions.keys().copied().collect();
        for doc in docs {
            self.deliver(doc).await?;
        }
        Ok(())
    }

    async fn deliver(&mut self, doc: Uuid) -> anyhow::Result<()> {
        self.deliver_from(doc, Some(self.origin)).await.map(|_| ())
    }

    /// Sends the document's entries after the subscription's cursor; returns the new cursor.
    async fn deliver_from(&mut self, doc: Uuid, skip: Option<Uuid>) -> anyhow::Result<i64> {
        let Some(sub) = self.subscriptions.get(&doc) else {
            return Ok(0);
        };
        let (workspace, after) = (sub.workspace, sub.cursor);
        let (entries, high) =
            match store::entries_after(&self.state, self.user, doc, workspace, after, skip).await {
                Ok(r) => r,
                Err(e) => {
                    self.send(&doc_error(&e, doc.as_bytes(), 0)).await?;
                    return Ok(after);
                }
            };
        if let Some(sub) = self.subscriptions.get_mut(&doc) {
            sub.cursor = sub.cursor.max(high);
        }
        // Keep each frame well under the limit: a big catch-up goes in several messages.
        let mut batch = Vec::new();
        let mut size = 0;
        for entry in entries {
            if size + entry.data.len() > MAX_FRAME_BYTES / 2 && !batch.is_empty() {
                self.send_updates(doc, std::mem::take(&mut batch)).await?;
                size = 0;
            }
            size += entry.data.len();
            batch.push(entry);
        }
        if !batch.is_empty() {
            self.send_updates(doc, batch).await?;
        }
        Ok(high)
    }

    async fn send_updates(
        &mut self,
        doc: Uuid,
        entries: Vec<drusite_proto::Entry>,
    ) -> anyhow::Result<()> {
        self.send(&ServerMessage::new(Body::Updates(Updates {
            doc: doc.as_bytes().to_vec(),
            entries,
        })))
        .await
    }
}
