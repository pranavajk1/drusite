// SPDX-License-Identifier: AGPL-3.0-or-later

//! The encrypted local store: SQLite with SQLCipher, one file per workspace.
//!
//! Each document (the workspace document and every page) is kept as an **update log plus
//! compacted snapshots**:
//!
//! - every edit appends its Loro update to `updates`, in one transaction, so a crash loses at
//!   most the edit being written and never leaves half of one;
//! - once enough updates pile up, the document is written whole to `snapshots` and the updates it
//!   covers are dropped, in one transaction, so a crash during compaction leaves either the old
//!   state or the new one.
//!
//! Loading a document is its newest snapshot plus the updates logged after it.
//!
//! The database runs in WAL mode with `synchronous = FULL`: a committed edit survives the app
//! being killed and the machine losing power.

use std::path::Path;

use drusite_crdt::{Page, VersionVector, Workspace};
use drusite_model::{PageId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::key::StoreKey;
use crate::{Error, Result};

/// The store's file name inside a data directory.
pub const STORE_FILE: &str = "workspace.drusite";

/// The layout of the database itself (not of the documents in it).
const STORE_SCHEMA: i32 = 4;

/// Compact a document after this many logged updates…
pub const COMPACT_AFTER_UPDATES: u64 = 500;
/// …or once its logged updates add up to this many bytes.
pub const COMPACT_AFTER_BYTES: u64 = 1024 * 1024;

/// What kind of document a stored document is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Workspace,
    Page,
}

impl DocKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Page => "page",
        }
    }
}

/// A stored document, ready to load: its newest snapshot and the updates after it.
#[derive(Debug, Clone)]
pub struct StoredDoc {
    pub snapshot: Vec<u8>,
    pub updates: Vec<Vec<u8>>,
    /// The sequence number of the last update logged (or that the snapshot covers).
    pub last_seq: u64,
    /// The bytes of the updates since the snapshot.
    pub pending_bytes: u64,
}

impl StoredDoc {
    pub fn parts(&self) -> Vec<&[u8]> {
        std::iter::once(self.snapshot.as_slice())
            .chain(self.updates.iter().map(Vec::as_slice))
            .collect()
    }

    /// Whether this document has logged enough updates to be worth compacting.
    pub fn wants_compaction(&self) -> bool {
        self.updates.len() as u64 >= COMPACT_AFTER_UPDATES
            || self.pending_bytes >= COMPACT_AFTER_BYTES
    }
}

#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (or creates) the store at `path`, encrypted with `key`. A wrong key is
    /// [`Error::WrongKey`]; nothing is written in that case.
    pub fn open(path: &Path, key: &StoreKey) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(&format!("PRAGMA key = {};", key.pragma_value().as_str()))?;
        // Reading anything checks the key: a wrong one reads as "not a database".
        if let Err(err) = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        }) {
            return Err(match err {
                rusqlite::Error::SqliteFailure(e, _)
                    if e.code == rusqlite::ErrorCode::NotADatabase =>
                {
                    Error::WrongKey
                }
                other => other.into(),
            });
        }
        conn.execute_batch(
            "PRAGMA cipher_memory_security = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             PRAGMA foreign_keys = ON;",
        )?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > STORE_SCHEMA {
            return Err(Error::NewerStore {
                found: version,
                supported: STORE_SCHEMA,
            });
        }
        if version < 1 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE TABLE docs (
                     id   TEXT PRIMARY KEY,
                     kind TEXT NOT NULL CHECK (kind IN ('workspace', 'page'))
                 ) STRICT;
                 CREATE TABLE snapshots (
                     doc   TEXT NOT NULL REFERENCES docs(id) ON DELETE CASCADE,
                     seq   INTEGER NOT NULL,
                     bytes BLOB NOT NULL,
                     PRIMARY KEY (doc, seq)
                 ) STRICT;
                 CREATE TABLE updates (
                     doc   TEXT NOT NULL REFERENCES docs(id) ON DELETE CASCADE,
                     seq   INTEGER NOT NULL,
                     bytes BLOB NOT NULL,
                     PRIMARY KEY (doc, seq)
                 ) STRICT;
                 PRAGMA user_version = 1;",
            )?;
            tx.commit()?;
        }
        if version < 2 {
            // What each document's server has: the last log entry imported from it, and every
            // version it is known to hold (imported from it, or pushed and acknowledged).
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE TABLE sync_state (
                     doc        TEXT PRIMARY KEY REFERENCES docs(id) ON DELETE CASCADE,
                     server_seq INTEGER NOT NULL DEFAULT 0,
                     server_vv  BLOB,
                     on_server  INTEGER NOT NULL DEFAULT 0
                 ) STRICT;
                 PRAGMA user_version = 2;",
            )?;
            tx.commit()?;
        }
        if version < 3 {
            // The server this device syncs with, and its refresh token while signed in. The
            // server stays after signing out, so signing in to another one later knows to forget
            // what the old one had.
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE TABLE account (
                     id            INTEGER PRIMARY KEY CHECK (id = 1),
                     server        TEXT NOT NULL,
                     refresh_token TEXT,
                     email         TEXT
                 ) STRICT;
                 PRAGMA user_version = 3;",
            )?;
            tx.commit()?;
        }
        if version < 4 {
            // Attachments: files on pages, by the BLAKE3 hash of their content, and whether the
            // server has each one yet.
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE TABLE blobs (
                     id       TEXT PRIMARY KEY,
                     bytes    BLOB NOT NULL,
                     uploaded INTEGER NOT NULL DEFAULT 0
                 ) STRICT;
                 PRAGMA user_version = 4;",
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Checks the database and its encryption for damage. Returns the problems found.
    pub fn integrity_problems(&self) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        for pragma in ["PRAGMA integrity_check", "PRAGMA cipher_integrity_check"] {
            let mut stmt = self.conn.prepare(pragma)?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for row in rows {
                let row = row?;
                if row != "ok" {
                    problems.push(row);
                }
            }
        }
        Ok(problems)
    }

    /// Stores a new document, starting from `snapshot` (its creation).
    pub fn create_doc(&mut self, id: &str, kind: DocKind, snapshot: &[u8]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO docs (id, kind) VALUES (?1, ?2)",
            params![id, kind.as_str()],
        )?;
        tx.execute(
            "INSERT INTO snapshots (doc, seq, bytes) VALUES (?1, 0, ?2)",
            params![id, snapshot],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Appends one update to a document's log and returns its sequence number.
    pub fn append(&mut self, id: &str, update: &[u8]) -> Result<u64> {
        let tx = self.conn.transaction()?;
        let last = last_seq(&tx, id)?.ok_or_else(|| Error::UnknownDoc(id.to_owned()))?;
        let seq = last + 1;
        tx.execute(
            "INSERT INTO updates (doc, seq, bytes) VALUES (?1, ?2, ?3)",
            params![id, seq_sql(seq), update],
        )?;
        tx.commit()?;
        Ok(seq)
    }

    /// Replaces everything logged for a document up to `through_seq` with `snapshot`, which
    /// must contain all of it. The previous snapshot is kept too, as a fallback.
    pub fn compact(&mut self, id: &str, snapshot: &[u8], through_seq: u64) -> Result<()> {
        let tx = self.conn.transaction()?;
        let through = seq_sql(through_seq);
        tx.execute(
            "INSERT OR REPLACE INTO snapshots (doc, seq, bytes) VALUES (?1, ?2, ?3)",
            params![id, through, snapshot],
        )?;
        tx.execute(
            "DELETE FROM updates WHERE doc = ?1 AND seq <= ?2",
            params![id, through],
        )?;
        // Keep the newest two snapshots.
        tx.execute(
            "DELETE FROM snapshots WHERE doc = ?1 AND seq < (
                 SELECT min(seq) FROM (SELECT seq FROM snapshots WHERE doc = ?1 ORDER BY seq DESC LIMIT 2)
             )",
            params![id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// A document's newest snapshot and the updates after it, or `None` if it isn't stored.
    pub fn load(&self, id: &str) -> Result<Option<StoredDoc>> {
        let snapshot: Option<(i64, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT seq, bytes FROM snapshots WHERE doc = ?1 ORDER BY seq DESC LIMIT 1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((snap_seq, snapshot)) = snapshot else {
            return Ok(None);
        };
        let mut stmt = self
            .conn
            .prepare("SELECT seq, bytes FROM updates WHERE doc = ?1 AND seq > ?2 ORDER BY seq")?;
        let mut updates = Vec::new();
        let mut last_seq = seq_from_sql(snap_seq);
        let mut pending_bytes = 0;
        for row in stmt.query_map(params![id, snap_seq], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })? {
            let (seq, bytes) = row?;
            last_seq = seq_from_sql(seq);
            pending_bytes += bytes.len() as u64;
            updates.push(bytes);
        }
        Ok(Some(StoredDoc {
            snapshot,
            updates,
            last_seq,
            pending_bytes,
        }))
    }

    /// The IDs of every stored document of a kind.
    pub fn docs(&self, kind: DocKind) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM docs WHERE kind = ?1 ORDER BY id")?;
        let ids = stmt
            .query_map(params![kind.as_str()], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    // ---- Sync state ----

    /// What a document's server is known to have.
    pub fn sync_state(&self, id: &str) -> Result<SyncState> {
        let row: Option<(i64, Option<Vec<u8>>, i64)> = self
            .conn
            .query_row(
                "SELECT server_seq, server_vv, on_server FROM sync_state WHERE doc = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((seq, vv, on_server)) = row else {
            return Ok(SyncState::default());
        };
        let server_vv = match vv {
            Some(bytes) => VersionVector::decode(&bytes)
                .map_err(|e| Error::Corrupt(format!("sync state of {id}: {e}")))?,
            None => VersionVector::default(),
        };
        Ok(SyncState {
            server_seq: seq_from_sql(seq),
            server_vv,
            on_server: on_server != 0,
        })
    }

    pub fn set_sync_state(&mut self, id: &str, state: &SyncState) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_state (doc, server_seq, server_vv, on_server) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (doc) DO UPDATE SET server_seq = excluded.server_seq,
                 server_vv = excluded.server_vv, on_server = excluded.on_server",
            params![
                id,
                seq_sql(state.server_seq),
                state.server_vv.encode(),
                i64::from(state.on_server)
            ],
        )?;
        Ok(())
    }

    // ---- Account ----

    /// The server this device syncs with, if it ever signed in.
    pub fn account(&self) -> Result<Option<StoredAccount>> {
        Ok(self
            .conn
            .query_row(
                "SELECT server, refresh_token, email FROM account WHERE id = 1",
                [],
                |r| {
                    Ok(StoredAccount {
                        server: r.get(0)?,
                        refresh_token: r.get(1)?,
                        email: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Records a sign-in to `server`. Signing in to another server than the last one forgets
    /// what the last one had, so everything is offered to the new one.
    pub fn sign_in(&mut self, server: &str, refresh_token: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let previous: Option<String> = tx
            .query_row("SELECT server FROM account WHERE id = 1", [], |r| r.get(0))
            .optional()?;
        if previous.as_deref().is_some_and(|p| p != server) {
            tx.execute("DELETE FROM sync_state", [])?;
        }
        tx.execute(
            "INSERT INTO account (id, server, refresh_token) VALUES (1, ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET server = excluded.server,
                 refresh_token = excluded.refresh_token,
                 email = CASE WHEN account.server = excluded.server THEN account.email END",
            params![server, refresh_token],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Replaces the refresh token after a refresh (each one is good once).
    pub fn set_refresh_token(&mut self, refresh_token: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE account SET refresh_token = ?1 WHERE id = 1",
            params![refresh_token],
        )?;
        Ok(())
    }

    pub fn set_account_email(&mut self, email: &str) -> Result<()> {
        self.conn
            .execute("UPDATE account SET email = ?1 WHERE id = 1", params![email])?;
        Ok(())
    }

    /// Forgets the refresh token: signed out. The server is kept (see [`Self::sign_in`]).
    pub fn sign_out(&mut self) -> Result<()> {
        self.conn
            .execute("UPDATE account SET refresh_token = NULL WHERE id = 1", [])?;
        Ok(())
    }

    /// Removes every document: for a device that hasn't synced yet giving up its own empty
    /// workspace for its account's.
    pub fn clear(&mut self) -> Result<()> {
        self.conn.execute("DELETE FROM docs", [])?;
        Ok(())
    }

    /// Removes a document and everything kept about it. Its log, snapshots and sync state go
    /// with it (foreign keys cascade).
    pub fn delete_doc(&mut self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM docs WHERE id = ?1", [id])?;
        Ok(())
    }

    // ---- Attachments ----

    /// Keeps a file. `uploaded` when it came from the server, so it needn't go back.
    pub fn put_blob(&mut self, id: &str, bytes: &[u8], uploaded: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO blobs (id, bytes, uploaded) VALUES (?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET uploaded = max(uploaded, excluded.uploaded)",
            rusqlite::params![id, bytes, i64::from(uploaded)],
        )?;
        Ok(())
    }

    pub fn blob(&self, id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row("SELECT bytes FROM blobs WHERE id = ?1", [id], |r| r.get(0))
            .optional()?)
    }

    pub fn has_blob(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM blobs WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Files the server doesn't have yet.
    pub fn blobs_to_upload(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM blobs WHERE uploaded = 0 ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn blob_uploaded(&mut self, id: &str) -> Result<()> {
        self.conn
            .execute("UPDATE blobs SET uploaded = 1 WHERE id = ?1", [id])?;
        Ok(())
    }

    // ---- Drusite documents on top of the raw log ----

    /// Saves a newly created page.
    pub fn create_page(&mut self, page: &Page) -> Result<SavePoint> {
        self.create_doc(
            &page.id().to_string(),
            DocKind::Page,
            &page.export_snapshot()?,
        )?;
        Ok(SavePoint::created(page.version()))
    }

    /// Loads a page and the save point to continue saving it from.
    pub fn load_page(&self, id: PageId) -> Result<(Page, SavePoint)> {
        let stored = self
            .load(&id.to_string())?
            .ok_or_else(|| Error::UnknownDoc(id.to_string()))?;
        let page = Page::load_all(&stored.parts())?;
        if page.id() != id {
            return Err(Error::Corrupt(format!(
                "page {id} holds page {}",
                page.id()
            )));
        }
        let at = SavePoint::loaded(page.version(), &stored);
        Ok((page, at))
    }

    /// Appends whatever changed in `page` since `at`, compacting when the log has grown. Does
    /// nothing if nothing changed.
    pub fn save_page(&mut self, page: &Page, at: &mut SavePoint) -> Result<()> {
        let id = page.id().to_string();
        self.save(
            &id,
            at,
            page.version(),
            |since| page.export_updates(since),
            || Ok((page.export_snapshot()?, page.export_pending()?)),
        )
    }

    pub fn create_workspace(&mut self, ws: &Workspace) -> Result<SavePoint> {
        self.create_doc(
            &ws.id().to_string(),
            DocKind::Workspace,
            &ws.export_snapshot()?,
        )?;
        Ok(SavePoint::created(ws.version()))
    }

    pub fn load_workspace(&self, id: WorkspaceId) -> Result<(Workspace, SavePoint)> {
        let stored = self
            .load(&id.to_string())?
            .ok_or_else(|| Error::UnknownDoc(id.to_string()))?;
        let ws = Workspace::load_all(&stored.parts())?;
        if ws.id() != id {
            return Err(Error::Corrupt(format!(
                "workspace {id} holds workspace {}",
                ws.id()
            )));
        }
        let at = SavePoint::loaded(ws.version(), &stored);
        Ok((ws, at))
    }

    pub fn save_workspace(&mut self, ws: &Workspace, at: &mut SavePoint) -> Result<()> {
        let id = ws.id().to_string();
        self.save(
            &id,
            at,
            ws.version(),
            |since| ws.export_updates(since),
            || Ok((ws.export_snapshot()?, ws.export_pending()?)),
        )
    }

    /// The one workspace in this store, if it has been created.
    pub fn workspace_id(&self) -> Result<Option<WorkspaceId>> {
        let ids = self.docs(DocKind::Workspace)?;
        match ids.as_slice() {
            [] => Ok(None),
            [id] => id
                .parse()
                .map(Some)
                .map_err(|_| Error::Corrupt(format!("bad workspace ID {id:?}"))),
            _ => Err(Error::Corrupt(
                "more than one workspace in the store".into(),
            )),
        }
    }

    fn save(
        &mut self,
        id: &str,
        at: &mut SavePoint,
        now: VersionVector,
        updates: impl FnOnce(&VersionVector) -> drusite_crdt::Result<Vec<u8>>,
        // The snapshot, and any updates still waiting for another writer's changes, which a
        // snapshot can't hold.
        snapshot: impl FnOnce() -> drusite_crdt::Result<(Vec<u8>, Option<Vec<u8>>)>,
    ) -> Result<()> {
        if now == at.version {
            return Ok(());
        }
        let bytes = updates(&at.version)?;
        at.seq = self.append(id, &bytes)?;
        at.version = now;
        at.since_compaction += 1;
        at.bytes_since_compaction += bytes.len() as u64;
        if at.since_compaction >= COMPACT_AFTER_UPDATES
            || at.bytes_since_compaction >= COMPACT_AFTER_BYTES
        {
            let (snapshot, waiting) = snapshot()?;
            self.compact(id, &snapshot, at.seq)?;
            at.since_compaction = 0;
            at.bytes_since_compaction = 0;
            // Kept after the snapshot, so they apply once what they wait for arrives.
            if let Some(waiting) = waiting {
                at.seq = self.append(id, &waiting)?;
                at.since_compaction = 1;
                at.bytes_since_compaction = waiting.len() as u64;
            }
        }
        Ok(())
    }
}

/// The account row: the server, and the refresh token while signed in.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredAccount {
    pub server: String,
    pub refresh_token: Option<String>,
    pub email: Option<String>,
}

impl std::fmt::Debug for StoredAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredAccount")
            .field("server", &self.server)
            .field("signed_in", &self.refresh_token.is_some())
            .field("email", &self.email)
            .finish()
    }
}

/// What a document's server is known to have.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncState {
    /// The last server log entry imported.
    pub server_seq: u64,
    /// Every version the server is known to hold.
    pub server_vv: VersionVector,
    /// The server has the document (it was created there, or came from there).
    pub on_server: bool,
}

/// How far a document has been saved: its version and the last sequence number logged.
#[derive(Debug, Clone)]
pub struct SavePoint {
    version: VersionVector,
    seq: u64,
    since_compaction: u64,
    bytes_since_compaction: u64,
}

impl SavePoint {
    fn created(version: VersionVector) -> Self {
        Self {
            version,
            seq: 0,
            since_compaction: 0,
            bytes_since_compaction: 0,
        }
    }

    fn loaded(version: VersionVector, stored: &StoredDoc) -> Self {
        Self {
            version,
            seq: stored.last_seq,
            since_compaction: stored.updates.len() as u64,
            bytes_since_compaction: stored.pending_bytes,
        }
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }
}

fn last_seq(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<Option<u64>> {
    let exists: Option<i64> = tx
        .query_row("SELECT 1 FROM docs WHERE id = ?1", params![id], |r| {
            r.get(0)
        })
        .optional()?;
    if exists.is_none() {
        return Ok(None);
    }
    let last: i64 = tx.query_row(
        "SELECT max(m) FROM (SELECT max(seq) AS m FROM updates WHERE doc = ?1 UNION ALL SELECT max(seq) FROM snapshots WHERE doc = ?1)",
        params![id],
        |r| r.get::<_, Option<i64>>(0).map(Option::unwrap_or_default),
    )?;
    Ok(Some(seq_from_sql(last)))
}

fn seq_sql(seq: u64) -> i64 {
    i64::try_from(seq).unwrap_or(i64::MAX)
}

fn seq_from_sql(seq: i64) -> u64 {
    u64::try_from(seq).unwrap_or(0)
}
