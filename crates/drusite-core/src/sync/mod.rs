// SPDX-License-Identifier: AGPL-3.0-or-later

//! Keeping this device's documents in step with the server: the client side of
//! `drusite-proto`.
//!
//! This is only the logic. It turns server messages into imports and local changes into
//! messages, and never touches a network itself, so the app's driver ([`net`] on native
//! platforms) and the simulation tests can both run it.
//!
//! What it keeps per document, in the store, survives restarts:
//! - the last server log entry imported, so a reconnect asks only for what's new;
//! - every version the server is known to hold (imported from it, or pushed and acknowledged),
//!   so a reconnect pushes exactly what the server may lack. Pushing something twice is
//!   harmless: Loro ignores what it already has.
//!
//! A document is created on the server once, by the subscription that carries its first
//! snapshot; every other copy comes from there.

#[cfg(feature = "net")]
pub mod net;

use std::collections::{HashMap, HashSet};

use drusite_crdt::{Page, VersionVector};
use drusite_model::{PageId, WorkspaceId};
use drusite_proto::{
    ClientMessage, DocKind as WireKind, Entry, ErrorCode, Push, ServerMessage, Subscribe,
    client_message, server_message,
};
use uuid::Uuid;

use crate::api::Event;
use crate::engine::{Core, patch_for};
use crate::store::{DocKind, SyncState};
use crate::{Error, Result};

/// One connection's worth of sync state; a new connection starts a new session.
#[derive(Debug, Default)]
pub struct Session {
    /// Documents subscribed on this connection, and whether this device may write them.
    subscribed: HashMap<Uuid, Subscription>,
    /// Pushes waiting for their `Ack`: the version each one brings the server to.
    in_flight: HashMap<u64, (Uuid, VersionVector)>,
    next_push: u64,
    /// Documents that may have changed beyond what was pushed: changed locally, or just
    /// subscribed. Only these are looked at for pushing.
    dirty: HashSet<Uuid>,
    /// Joining this workspace from the server in place of this blank device's own.
    joining: Option<Uuid>,
    /// Documents whose push failed on the server, to push again on the next retry tick.
    retry: HashSet<Uuid>,
}

#[derive(Debug)]
struct Subscription {
    kind: WireKind,
    /// The server has answered `Subscribed`.
    ready: bool,
    writable: bool,
    /// Pushed on this connection, acknowledged or not: the next push starts from here.
    pushed: VersionVector,
    /// The version of the first snapshot this subscription offered to create the document with.
    create_version: Option<VersionVector>,
}

/// How one document stands with the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocSyncStatus {
    pub doc: String,
    /// The last server log entry imported.
    pub server_seq: u64,
    pub on_server: bool,
    /// Subscribed on the current connection.
    pub subscribed: bool,
    /// The server has answered the subscription.
    pub ready: bool,
    pub writable: bool,
}

/// The server refused something that sync can't get past by itself.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("the server refused access to this workspace: {0}")]
    Forbidden(String),
    #[error(transparent)]
    Core(#[from] Error),
}

fn subscribe(
    doc: Uuid,
    workspace: WorkspaceId,
    kind: WireKind,
    after: u64,
    create: Vec<u8>,
) -> ClientMessage {
    ClientMessage::new(client_message::Body::Subscribe(Subscribe {
        doc: doc.as_bytes().to_vec(),
        workspace: workspace.as_uuid().as_bytes().to_vec(),
        kind: kind as i32,
        after_seq: after,
        create,
    }))
}

impl Core {
    /// The workspace this device holds.
    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace.id()
    }

    /// Whether this device has never synced and holds nothing of its own: it can take its
    /// account's existing workspace instead of its own empty one.
    pub fn is_blank(&self) -> Result<bool> {
        let id = self.workspace.id().to_string();
        if self.store.sync_state(&id)?.on_server {
            return Ok(false);
        }
        // Nothing worth keeping: no pages, or only untitled, empty ones (a new install opens on
        // one, so there is somewhere to type).
        for node in self.workspace.pages()? {
            if !node.title.is_empty() {
                return Ok(false);
            }
            let (page, _) = self.store.load_page(node.id)?;
            let blocks = page.blocks()?;
            if blocks.len() > 1 || blocks.iter().any(|b| !b.text().is_empty()) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Replaces this blank device's workspace with the account's, from the server's entries for
    /// it. See [`Self::is_blank`].
    pub fn adopt_workspace(&mut self, id: WorkspaceId, entries: &[Entry]) -> Result<()> {
        if !self.is_blank()? {
            return Err(Error::Corrupt(
                "this device already has notes of its own; it can't take another workspace".into(),
            ));
        }
        let parts: Vec<&[u8]> = entries.iter().map(|e| e.data.as_slice()).collect();
        let workspace = drusite_crdt::Workspace::load_all(&parts)?;
        if workspace.id() != id {
            return Err(Error::Corrupt("the server sent another workspace".into()));
        }
        self.store.clear()?;
        self.open.clear();
        self.workspace_at = self.store.create_workspace(&workspace)?;
        self.keep_waiting(&id.to_string(), entries, &workspace.version())?;
        let state = SyncState {
            server_seq: entries.last().map_or(0, |e| e.seq),
            server_vv: workspace.version(),
            on_server: true,
        };
        self.store.set_sync_state(&id.to_string(), &state)?;
        self.workspace = workspace;
        self.search.reset();
        self.announce_workspace()
            .map_err(|e| Error::Corrupt(e.message))?;
        Ok(())
    }

    /// How each document stands with the server, for a sync indicator (and for tests).
    pub fn sync_status(&self) -> Result<Vec<DocSyncStatus>> {
        let mut docs = vec![self.workspace.id().to_string()];
        docs.extend(self.store.docs(DocKind::Page)?);
        let mut out = Vec::new();
        for id in docs {
            let state = self.store.sync_state(&id)?;
            let sub = id
                .parse::<Uuid>()
                .ok()
                .and_then(|u| self.sync.as_ref().and_then(|s| s.subscribed.get(&u)));
            out.push(DocSyncStatus {
                doc: id,
                server_seq: state.server_seq,
                on_server: state.on_server,
                subscribed: sub.is_some(),
                ready: sub.is_some_and(|s| s.ready),
                writable: sub.is_some_and(|s| s.writable),
            });
        }
        Ok(out)
    }

    /// Lets a sync driver know when something changes locally, so it can push.
    pub fn set_change_listener(&mut self, listener: Option<Box<dyn Fn() + Send>>) {
        self.on_change = listener;
    }

    /// Starts a session on a new connection: subscribes to the workspace and every page.
    pub fn sync_start(&mut self) -> Result<Vec<ClientMessage>> {
        self.sync = Some(Session::default());
        self.sync_subscribe_missing()
    }

    /// Starts a session that takes the account's workspace `id` from the server in place of this
    /// blank device's own (see [`Self::is_blank`]), then syncs it as usual.
    pub fn sync_start_join(&mut self, id: WorkspaceId) -> Result<Vec<ClientMessage>> {
        if id == self.workspace.id() {
            return self.sync_start();
        }
        if !self.is_blank()? {
            return Err(Error::Corrupt(
                "this device already has notes of its own; it can't take another workspace".into(),
            ));
        }
        let doc = *id.as_uuid();
        let mut session = Session {
            joining: Some(doc),
            ..Session::default()
        };
        session.subscribed.insert(
            doc,
            Subscription {
                kind: WireKind::Workspace,
                ready: false,
                writable: false,
                pushed: VersionVector::default(),
                create_version: None,
            },
        );
        self.sync = Some(session);
        Ok(vec![subscribe(doc, id, WireKind::Workspace, 0, Vec::new())])
    }

    /// The connection is gone. Unacknowledged pushes are pushed again next time.
    pub fn sync_stop(&mut self) {
        self.sync = None;
    }

    /// Subscriptions for documents this device has, or the workspace lists, that the session
    /// hasn't subscribed to yet.
    fn sync_subscribe_missing(&mut self) -> Result<Vec<ClientMessage>> {
        let Some(session) = &self.sync else {
            return Ok(Vec::new());
        };
        if session.joining.is_some() {
            // Nothing else until the account's workspace is here.
            return Ok(Vec::new());
        }
        let ws = self.workspace.id();
        let mut wanted: Vec<(Uuid, WireKind)> = vec![(*ws.as_uuid(), WireKind::Workspace)];
        let mut pages: HashSet<Uuid> = HashSet::new();
        for id in self.store.docs(DocKind::Page)? {
            if let Ok(uuid) = id.parse::<Uuid>() {
                pages.insert(uuid);
            }
        }
        // Pages another device made: in the tree, not here yet.
        for node in self.workspace.pages()? {
            pages.insert(*node.id.as_uuid());
        }
        wanted.extend(pages.into_iter().map(|p| (p, WireKind::Page)));

        let mut out = Vec::new();
        let mut new_subs = Vec::new();
        for (doc, kind) in wanted {
            if session.subscribed.contains_key(&doc) {
                continue;
            }
            let key = doc.to_string();
            let state = self.store.sync_state(&key)?;
            let created = if state.on_server {
                None
            } else {
                match kind {
                    WireKind::Workspace => {
                        Some((self.workspace.export_snapshot()?, self.workspace.version()))
                    }
                    // `None` for a page that isn't here yet: it comes from the server.
                    _ => self.page_snapshot(PageId::from_uuid(doc))?,
                }
            };
            let (create, create_version) = match created {
                Some((bytes, version)) => (bytes, Some(version)),
                None => (Vec::new(), None),
            };
            out.push(subscribe(doc, ws, kind, state.server_seq, create));
            new_subs.push((doc, kind, state.server_vv, create_version));
        }
        let session = self
            .sync
            .as_mut()
            .ok_or_else(|| Error::Corrupt("no session".into()))?;
        for (doc, kind, vv, create_version) in new_subs {
            session.subscribed.insert(
                doc,
                Subscription {
                    kind,
                    ready: false,
                    writable: false,
                    pushed: vv,
                    create_version,
                },
            );
        }
        Ok(out)
    }

    /// A page's snapshot and the version it holds, if this device has the page.
    fn page_snapshot(&self, id: PageId) -> Result<Option<(Vec<u8>, VersionVector)>> {
        if let Some(open) = self.open.get(&id) {
            return Ok(Some((open.page.export_snapshot()?, open.page.version())));
        }
        if self.store.load(&id.to_string())?.is_none() {
            return Ok(None);
        }
        let (page, _) = self.store.load_page(id)?;
        Ok(Some((page.export_snapshot()?, page.version())))
    }

    /// Notes that a document changed here, so the next [`Self::sync_outgoing`] looks at it.
    pub(crate) fn mark_changed(&mut self, doc: Uuid) {
        if let Some(session) = self.sync.as_mut() {
            session.dirty.insert(doc);
        }
    }

    /// The version of a document this device has and the updates since `since`, without
    /// keeping it open. `None` if the device doesn't have it.
    fn local_changes(
        &self,
        doc: Uuid,
        kind: WireKind,
        since: &VersionVector,
    ) -> Result<Option<(VersionVector, Vec<u8>)>> {
        if kind == WireKind::Workspace {
            // Only this device's own workspace: never another one's document under its ID.
            if doc != *self.workspace.id().as_uuid() {
                return Ok(None);
            }
            let v = self.workspace.version();
            return Ok(Some((v, self.workspace.export_updates(since)?)));
        }
        let id = PageId::from_uuid(doc);
        if let Some(open) = self.open.get(&id) {
            return Ok(Some((
                open.page.version(),
                open.page.export_updates(since)?,
            )));
        }
        if self.store.load(&doc.to_string())?.is_none() {
            return Ok(None);
        }
        let (page, _) = self.store.load_page(id)?;
        Ok(Some((page.version(), page.export_updates(since)?)))
    }

    /// Pushes again what failed on the server. Drivers call this now and then (with their ping),
    /// so a push that met a passing server error goes through without waiting for the next edit.
    pub fn sync_retry(&mut self) -> Result<Vec<ClientMessage>> {
        if let Some(session) = self.sync.as_mut() {
            let retry: Vec<Uuid> = session.retry.drain().collect();
            session.dirty.extend(retry);
        }
        self.sync_outgoing()
    }

    /// Pushes for every subscribed, writable document that has changed beyond what the
    /// server has or has been sent, and subscriptions for any new documents.
    pub fn sync_outgoing(&mut self) -> Result<Vec<ClientMessage>> {
        let mut out = self.sync_subscribe_missing()?;
        let Some(session) = &self.sync else {
            return Ok(out);
        };
        // While joining, this device's documents aren't the account's yet: push nothing.
        if session.joining.is_some() {
            return Ok(out);
        }
        let ready: Vec<(Uuid, WireKind, VersionVector)> = session
            .dirty
            .iter()
            .filter_map(|d| session.subscribed.get(d).map(|s| (d, s)))
            .filter(|(_, s)| s.ready && s.writable)
            .map(|(d, s)| (*d, s.kind, s.pushed.clone()))
            .collect();
        let mut pushes = Vec::new();
        for (doc, kind, pushed) in ready {
            let Some((version, updates)) = self.local_changes(doc, kind, &pushed)? else {
                continue;
            };
            if !pushed.includes_vv(&version) {
                pushes.push((doc, version, updates));
            }
        }
        let session = self
            .sync
            .as_mut()
            .ok_or_else(|| Error::Corrupt("no session".into()))?;
        // Ready documents have been looked at; ones still waiting for `Subscribed` stay dirty.
        session
            .dirty
            .retain(|d| session.subscribed.get(d).is_some_and(|s| !s.ready));
        for (doc, version, update) in pushes {
            session.next_push += 1;
            let push_id = session.next_push;
            if let Some(sub) = session.subscribed.get_mut(&doc) {
                sub.pushed.merge(&version);
            }
            session.in_flight.insert(push_id, (doc, version));
            out.push(ClientMessage::new(client_message::Body::Push(Push {
                doc: doc.as_bytes().to_vec(),
                push_id,
                update,
            })));
        }
        Ok(out)
    }

    /// Handles one message from the server; returns what to send back.
    pub fn sync_receive(
        &mut self,
        message: ServerMessage,
    ) -> Result<Vec<ClientMessage>, SyncError> {
        use server_message::Body;
        match message.body {
            Some(Body::Updates(u)) => {
                let Ok(doc) = Uuid::from_slice(&u.doc) else {
                    return Ok(Vec::new());
                };
                self.import_entries(doc, &u.entries)?;
                // Pages another device made appear in the workspace: subscribe to them.
                Ok(self.sync_subscribe_missing()?)
            }
            Some(Body::Subscribed(s)) => {
                let Ok(doc) = Uuid::from_slice(&s.doc) else {
                    return Ok(Vec::new());
                };
                let key = doc.to_string();
                let create_version = self
                    .sync
                    .as_mut()
                    .and_then(|x| x.subscribed.get_mut(&doc))
                    .and_then(|sub| {
                        sub.ready = true;
                        sub.writable = s.writable;
                        sub.create_version.take()
                    });
                if self.store.load(&key)?.is_some() {
                    let mut state = self.store.sync_state(&key)?;
                    state.on_server = true;
                    if !s.version.is_empty() {
                        // The server says exactly what its log holds: that, not this device's
                        // own bookkeeping, is what the next push starts from.
                        state.server_vv = VersionVector::decode(&s.version)
                            .map_err(|e| Error::Corrupt(format!("the server's version: {e}")))?;
                    } else if s.created
                        && let Some(version) = &create_version
                    {
                        // An older server that doesn't say: it made the document from this
                        // device's snapshot, so it holds that version.
                        state.server_vv.merge(version);
                    }
                    if let Some(sub) = self.sync.as_mut().and_then(|x| x.subscribed.get_mut(&doc)) {
                        sub.pushed = state.server_vv.clone();
                    }
                    self.store.set_sync_state(&key, &state)?;
                }
                // Push whatever the server lacks.
                self.mark_changed(doc);
                Ok(self.sync_outgoing()?)
            }
            Some(Body::Ack(a)) => {
                let Some((doc, version)) = self
                    .sync
                    .as_mut()
                    .and_then(|s| s.in_flight.remove(&a.push_id))
                else {
                    return Ok(Vec::new());
                };
                let key = doc.to_string();
                if self.store.load(&key)?.is_none() {
                    return Ok(Vec::new());
                }
                let mut state = self.store.sync_state(&key)?;
                state.server_vv.merge(&version);
                // The server's sequence number for our own push needn't come back to us. Only
                // move forward over it if nothing from others is in between, which the next
                // `Updates` would otherwise fill in.
                if a.seq == state.server_seq + 1 {
                    state.server_seq = a.seq;
                }
                self.store.set_sync_state(&key, &state)?;
                Ok(Vec::new())
            }
            Some(Body::Error(e)) => {
                let code = ErrorCode::try_from(e.code).unwrap_or(ErrorCode::Unspecified);
                let doc = Uuid::from_slice(&e.doc).ok();
                let joining = self.sync.as_ref().and_then(|s| s.joining);
                let is_workspace = doc == Some(*self.workspace.id().as_uuid())
                    || (doc.is_some() && doc == joining);
                // Without the workspace there is nothing to sync: end, and say why.
                if is_workspace && matches!(code, ErrorCode::Forbidden | ErrorCode::NotFound) {
                    return Err(SyncError::Forbidden(e.message));
                }
                let Some(doc) = doc else {
                    return Ok(Vec::new());
                };
                if e.push_id != 0 {
                    return Ok(self.push_failed(doc, code, &e.version)?);
                }
                if matches!(code, ErrorCode::Forbidden)
                    && let Some(sub) = self.sync.as_mut().and_then(|s| s.subscribed.get_mut(&doc))
                {
                    sub.writable = false;
                }
                Ok(Vec::new())
            }
            Some(Body::Welcome(_) | Body::Pong(_)) | None => Ok(Vec::new()),
        }
    }

    /// Stores, as they came, the entries `local` doesn't include: updates waiting for another
    /// writer's changes that haven't arrived. Loro holds those in memory only, and saving a
    /// document saves what's applied, so without this a restart (or closing the page) would
    /// lose them. Loading the document imports them again, and they apply once what they wait
    /// for is here.
    fn keep_waiting(&mut self, key: &str, entries: &[Entry], local: &VersionVector) -> Result<()> {
        for entry in entries {
            if !local.includes_vv(&drusite_crdt::blob_version(&entry.data)?) {
                self.store.append(key, &entry.data)?;
            }
        }
        Ok(())
    }

    /// A push was refused. Every push for the document sent after it built on it, so none of
    /// them count: start again from what the server is known to have.
    fn push_failed(
        &mut self,
        doc: Uuid,
        code: ErrorCode,
        version: &[u8],
    ) -> Result<Vec<ClientMessage>> {
        let key = doc.to_string();
        let mut state = self.store.sync_state(&key)?;
        if code == ErrorCode::MissingDependencies && !version.is_empty() {
            state.server_vv = VersionVector::decode(version)
                .map_err(|e| Error::Corrupt(format!("the server's version: {e}")))?;
            if self.store.load(&key)?.is_some() {
                self.store.set_sync_state(&key, &state)?;
            }
        }
        let Some(session) = self.sync.as_mut() else {
            return Ok(Vec::new());
        };
        session.in_flight.retain(|_, (d, _)| *d != doc);
        if let Some(sub) = session.subscribed.get_mut(&doc) {
            sub.pushed = state.server_vv.clone();
            match code {
                // Pushing again won't help.
                ErrorCode::Forbidden | ErrorCode::InvalidUpdate | ErrorCode::TooLarge => {
                    sub.writable = false;
                }
                _ => {}
            }
        }
        match code {
            // Push what the server lacks, now.
            ErrorCode::MissingDependencies => {
                self.mark_changed(doc);
                self.sync_outgoing()
            }
            // Try again on the driver's next retry tick (see [`Self::sync_retry`]), so a failing
            // server isn't hammered.
            ErrorCode::Internal | ErrorCode::NotSubscribed | ErrorCode::Unspecified => {
                if let Some(session) = self.sync.as_mut() {
                    session.retry.insert(doc);
                }
                Ok(Vec::new())
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Imports a server's entries for one document, creating it here if it's new to this
    /// device, and records how far the server is known to be.
    fn import_entries(&mut self, doc: Uuid, entries: &[Entry]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let joining = self.sync.as_ref().and_then(|s| s.joining);
        if joining == Some(doc) {
            self.adopt_workspace(WorkspaceId::from_uuid(doc), entries)?;
            if let Some(session) = self.sync.as_mut() {
                session.joining = None;
                if let Some(sub) = session.subscribed.get_mut(&doc) {
                    sub.pushed = self.workspace.version();
                }
            }
            return Ok(());
        }
        let key = doc.to_string();
        let mut state = self.store.sync_state(&key)?;
        let parts: Vec<&[u8]> = entries.iter().map(|e| e.data.as_slice()).collect();
        let mut reached = VersionVector::default();
        for part in &parts {
            reached.merge(&drusite_crdt::blob_version(part)?);
        }

        // The version this device holds once the entries are in. An update that needs another
        // writer's changes not here yet waits in memory, unapplied: see `keep_waiting`.
        let local: VersionVector;
        self.search_changed(doc);
        if doc == *self.workspace.id().as_uuid() {
            for part in &parts {
                self.workspace.import(part)?;
            }
            local = self.workspace.version();
            self.store
                .save_workspace(&self.workspace, &mut self.workspace_at)?;
            self.announce_workspace()
                .map_err(|e| Error::Corrupt(e.message))?;
        } else {
            let id = PageId::from_uuid(doc);
            if let Some(open) = self.open.get_mut(&id) {
                let mut updated = Vec::new();
                let mut removed = Vec::new();
                for part in &parts {
                    let patch = open.page.import(part)?;
                    updated.extend(patch.updated);
                    removed.extend(patch.removed);
                }
                local = open.page.version();
                self.store.save_page(&open.page, &mut open.at)?;
                let patch = patch_for(&open.page, &updated, &removed)
                    .map_err(|e| Error::Corrupt(e.message))?;
                if !patch.is_empty() {
                    (self.events)(Event::PageChanged { page: id, patch });
                }
            } else if self.store.load(&key)?.is_some() {
                let (mut page, mut at) = self.store.load_page(id)?;
                for part in &parts {
                    page.import(part)?;
                }
                local = page.version();
                self.store.save_page(&page, &mut at)?;
            } else {
                // New to this device: made from the server's copy.
                let page = Page::load_all(&parts).map_err(|e| {
                    let seqs: Vec<(u64, bool)> =
                        entries.iter().map(|e| (e.seq, e.snapshot)).collect();
                    Error::Corrupt(format!("page {id} from the server's entries {seqs:?}: {e}"))
                })?;
                if page.id() != id {
                    return Err(Error::Corrupt(format!(
                        "the server sent page {} as {id}",
                        page.id()
                    )));
                }
                local = page.version();
                self.store.create_page(&page)?;
                state.on_server = true;
            }
        }
        self.keep_waiting(&key, entries, &local)?;
        // What the server has, it has, applied here or not.
        state.server_vv.merge(&reached);
        state.server_seq = state
            .server_seq
            .max(entries.iter().map(|e| e.seq).max().unwrap_or(0));
        self.store.set_sync_state(&key, &state)?;
        if let Some(sub) = self.sync.as_mut().and_then(|s| s.subscribed.get_mut(&doc)) {
            sub.pushed.merge(&reached);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::StoreKey;
    use crate::api::Command;
    use drusite_proto::{Subscribed, server_message::Body};

    fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let core =
            Core::open(dir.path(), &StoreKey::generate().unwrap(), Box::new(|_| {})).unwrap();
        (core, dir)
    }

    fn pushes(messages: &[ClientMessage]) -> usize {
        messages
            .iter()
            .filter(|m| matches!(m.body, Some(client_message::Body::Push(_))))
            .count()
    }

    /// A device joining its account's workspace must never push its own workspace's history
    /// into the account's, even if the server answers `Subscribed` without sending anything.
    #[test]
    fn a_joining_device_never_pushes_its_own_workspace_as_the_accounts() {
        let (mut phone, _d) = core();
        let account = WorkspaceId::new();
        phone.sync_start_join(account).unwrap();
        let replies = phone
            .sync_receive(ServerMessage::new(Body::Subscribed(Subscribed {
                doc: account.as_uuid().as_bytes().to_vec(),
                head_seq: 0,
                created: false,
                writable: true,
                version: Vec::new(),
            })))
            .unwrap();
        assert_eq!(pushes(&replies), 0);
        phone.mark_changed(*account.as_uuid());
        assert_eq!(pushes(&phone.sync_outgoing().unwrap()), 0);
    }

    /// The server refusing the workspace ends sync, rather than leaving a dead connection.
    #[test]
    fn a_refused_workspace_ends_sync() {
        let (mut phone, _d) = core();
        let account = WorkspaceId::new();
        phone.sync_start_join(account).unwrap();
        let refused = phone.sync_receive(ServerMessage::new(Body::Error(drusite_proto::Error {
            code: ErrorCode::Forbidden as i32,
            message: "no".into(),
            doc: account.as_uuid().as_bytes().to_vec(),
            push_id: 0,
            version: Vec::new(),
        })));
        assert!(matches!(refused, Err(SyncError::Forbidden(_))));
    }

    fn push_ids(messages: &[ClientMessage]) -> Vec<u64> {
        messages
            .iter()
            .filter_map(|m| match &m.body {
                Some(client_message::Body::Push(p)) => Some(p.push_id),
                _ => None,
            })
            .collect()
    }

    /// A refused push takes every later in-flight push of the same document with it: they
    /// built on it, so an `Ack` for one of them mustn't mark the refused changes as stored.
    #[test]
    fn a_refused_push_voids_the_pushes_built_on_it() {
        let (mut mac, _d) = core();
        let ws = mac.workspace_id();
        let doc = ws.as_uuid().as_bytes().to_vec();
        mac.sync_start().unwrap();
        let subscribed = |version: Vec<u8>| {
            ServerMessage::new(Body::Subscribed(Subscribed {
                doc: doc.clone(),
                head_seq: 1,
                created: true,
                writable: true,
                version,
            }))
        };
        mac.sync_receive(subscribed(VersionVector::default().encode()))
            .unwrap();
        mac.handle(Command::CreatePage {
            parent: None,
            title: "one".into(),
        })
        .unwrap();
        let first = push_ids(&mac.sync_outgoing().unwrap());
        mac.handle(Command::CreatePage {
            parent: None,
            title: "two".into(),
        })
        .unwrap();
        let second = push_ids(&mac.sync_outgoing().unwrap());
        assert_eq!((first.len(), second.len()), (1, 1));

        // The first fails on the server; the second, built on it, is stored and acknowledged.
        mac.sync_receive(ServerMessage::new(Body::Error(drusite_proto::Error {
            code: ErrorCode::Internal as i32,
            message: "database hiccup".into(),
            doc: doc.clone(),
            push_id: first[0],
            version: Vec::new(),
        })))
        .unwrap();
        mac.sync_receive(ServerMessage::new(Body::Ack(drusite_proto::Ack {
            doc: doc.clone(),
            push_id: second[0],
            seq: 2,
        })))
        .unwrap();
        // So the server isn't known to have either page yet: the next push carries both.
        let state = mac.store.sync_state(&ws.to_string()).unwrap();
        assert!(!state.server_vv.includes_vv(&mac.workspace.version()));
        mac.mark_changed(*ws.as_uuid());
        assert_eq!(push_ids(&mac.sync_outgoing().unwrap()).len(), 1);

        // "Missing dependencies" says what the server has, and pushes again from there at once.
        let again = mac
            .sync_receive(ServerMessage::new(Body::Error(drusite_proto::Error {
                code: ErrorCode::MissingDependencies as i32,
                message: "gap".into(),
                doc: doc.clone(),
                push_id: 99,
                version: VersionVector::default().encode(),
            })))
            .unwrap();
        assert_eq!(push_ids(&again).len(), 1);
    }

    /// An update that needs another writer's change this device hasn't got yet is kept on disk
    /// until that change arrives, page closed or not.
    #[test]
    fn an_update_waiting_for_its_dependencies_is_kept_until_it_applies() {
        let dir = tempfile::tempdir().unwrap();
        let key = StoreKey::generate().unwrap();
        let open =
            |dir: &tempfile::TempDir| Core::open(dir.path(), &key, Box::new(|_| {})).unwrap();
        let mut device = open(&dir);
        let crate::api::Response::PageCreated { page: info } = device
            .handle(Command::CreatePage {
                parent: None,
                title: "p".into(),
            })
            .unwrap()
        else {
            panic!()
        };
        let id = info.id;
        let snapshot = device.page_snapshot(id).unwrap().unwrap().0;
        // Two other writers: B's edit builds on A's.
        let mut a = Page::load(&snapshot).unwrap();
        let mut b = Page::load(&snapshot).unwrap();
        let block = a.order()[0];
        let start = a.version();
        a.insert_text(block, 0, "alpha").unwrap();
        let from_a = a.export_updates(&start).unwrap();
        b.import(&from_a).unwrap();
        let before_b = b.version();
        b.insert_text(block, 0, "beta ").unwrap();
        let from_b = b.export_updates(&before_b).unwrap();

        let doc = id.as_uuid().as_bytes().to_vec();
        let deliver = |device: &mut Core, seq: u64, data: Vec<u8>| {
            device
                .sync_receive(ServerMessage::new(Body::Updates(drusite_proto::Updates {
                    doc: doc.clone(),
                    entries: vec![Entry {
                        seq,
                        data,
                        snapshot: false,
                    }],
                })))
                .unwrap();
        };
        let seq = |device: &Core| {
            device
                .sync_status()
                .unwrap()
                .into_iter()
                .find(|d| d.doc == id.to_string())
                .unwrap()
                .server_seq
        };
        device.sync_start().unwrap();
        // B's arrives first: it can't apply yet, but it's kept on disk.
        deliver(&mut device, 3, from_b.clone());
        assert_eq!(seq(&device), 3);
        let text =
            |device: &Core| device.store.load_page(id).unwrap().0.blocks().unwrap()[0].text();
        assert_eq!(text(&device), "");
        // The device restarts while it waits; then A's arrives, and both apply.
        drop(device);
        let mut device = open(&dir);
        device.sync_start().unwrap();
        deliver(&mut device, 2, from_a);
        assert_eq!(text(&device), "beta alpha");
    }

    /// A device with notes of its own can't take another workspace.
    #[test]
    fn only_a_blank_device_joins() {
        let (mut mac, _d) = core();
        mac.handle(Command::CreatePage {
            parent: None,
            title: "Mine".into(),
        })
        .unwrap();
        assert!(!mac.is_blank().unwrap());
        assert!(mac.sync_start_join(WorkspaceId::new()).is_err());
    }

    /// The empty, untitled page a new install opens on isn't notes of its own; typing in it is.
    #[test]
    fn an_empty_first_page_is_still_blank() {
        let (mut phone, _d) = core();
        let crate::api::Response::PageCreated { page } = phone
            .handle(Command::CreatePage {
                parent: None,
                title: String::new(),
            })
            .unwrap()
        else {
            panic!()
        };
        assert!(phone.is_blank().unwrap());
        let crate::api::Response::Opened { view } =
            phone.handle(Command::OpenPage { page: page.id }).unwrap()
        else {
            panic!()
        };
        phone
            .handle(Command::Edit {
                page: page.id,
                edits: vec![crate::api::Edit::InsertText {
                    block: view.blocks[0].id,
                    pos: 0,
                    text: "x".into(),
                }],
            })
            .unwrap();
        assert!(!phone.is_blank().unwrap());
    }
}
