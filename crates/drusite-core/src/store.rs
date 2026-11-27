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
const STORE_SCHEMA: i32 = 7;

/// A token the person made for an agent on this computer (the token itself isn't kept).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTokenRow {
    pub id: String,
    pub name: String,
    /// `drusite_mcp::Access`, as JSON.
    pub access: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// An AI provider set up on this device.
#[derive(Clone, PartialEq, Eq)]
pub struct AiProviderRow {
    pub id: String,
    /// `drusite_ai::Kind`, as its config name (`openai-compatible`, …).
    pub kind: String,
    pub name: String,
    pub base_url: Option<String>,
    pub api_key: Option<zeroize::Zeroizing<String>>,
    pub created_at: i64,
}

impl std::fmt::Debug for AiProviderRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiProviderRow")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "…"))
            .finish_non_exhaustive()
    }
}

fn ai_provider_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AiProviderRow> {
    Ok(AiProviderRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        name: r.get(2)?,
        base_url: r.get(3)?,
        api_key: r.get::<_, Option<String>>(4)?.map(zeroize::Zeroizing::new),
        created_at: r.get(5)?,
    })
}

/// One agent call that changed something, on this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentActivityRow {
    pub id: String,
    pub agent: String,
    pub tool: String,
    pub summary: String,
    /// `Vec<drusite_mcp::Effect>`, as JSON.
    pub effects: String,
    /// Unix milliseconds.
    pub at: i64,
    pub undone_at: Option<i64>,
}

fn activity_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AgentActivityRow> {
    Ok(AgentActivityRow {
        id: r.get(0)?,
        agent: r.get(1)?,
        tool: r.get(2)?,
        summary: r.get(3)?,
        effects: r.get(4)?,
        at: r.get(5)?,
        undone_at: r.get(6)?,
    })
}

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

/// What the Markdown bridge last wrote for a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeFile {
    pub page: String,
    /// Relative to the folder, with `/` between parts.
    pub path: String,
    /// The page's version the file was written from (a Loro version vector).
    pub version: Vec<u8>,
    /// BLAKE3 of the file's bytes as written.
    pub hash: String,
    /// BLAKE3 of the file's path and frontmatter, to notice a rename or a property change.
    pub meta: String,
    /// The file's modification time (Unix nanoseconds) and size when written, to skip reading
    /// files nobody touched.
    pub mtime: i64,
    pub size: i64,
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
        if version < 5 {
            // The Markdown bridge, on this device only: its folder, and for each page the file
            // it was last written to, from which version, and what the file held then.
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE TABLE bridge (
                     id     INTEGER PRIMARY KEY CHECK (id = 1),
                     folder TEXT NOT NULL
                 ) STRICT;
                 CREATE TABLE bridge_files (
                     page    TEXT PRIMARY KEY,
                     path    TEXT NOT NULL UNIQUE,
                     version BLOB NOT NULL,
                     hash    TEXT NOT NULL,
                     meta    TEXT NOT NULL,
                     mtime   INTEGER NOT NULL,
                     size    INTEGER NOT NULL
                 ) STRICT;
                 PRAGMA user_version = 5;",
            )?;
            tx.commit()?;
        }
        if version < 6 {
            // Agents on this computer: whether the local MCP endpoint is on and where, the
            // tokens the person made for agents (only their hashes), and what agents did.
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE TABLE local_agents (
                     id      INTEGER PRIMARY KEY CHECK (id = 1),
                     enabled INTEGER NOT NULL,
                     port    INTEGER NOT NULL
                 ) STRICT;
                 CREATE TABLE agent_tokens (
                     id           TEXT PRIMARY KEY,
                     name         TEXT NOT NULL,
                     token_hash   BLOB NOT NULL UNIQUE,
                     access       TEXT NOT NULL,
                     created_at   INTEGER NOT NULL,
                     last_used_at INTEGER
                 ) STRICT;
                 CREATE TABLE agent_activity (
                     id        TEXT PRIMARY KEY,
                     agent     TEXT NOT NULL,
                     tool      TEXT NOT NULL,
                     summary   TEXT NOT NULL,
                     effects   TEXT NOT NULL,
                     at        INTEGER NOT NULL,
                     undone_at INTEGER
                 ) STRICT;
                 PRAGMA user_version = 6;",
            )?;
            tx.commit()?;
        }
        if version < 7 {
            // AI providers on this device, their keys included (the store is encrypted, and
            // never synced), and which provider and model each feature uses.
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE TABLE ai_providers (
                     id         TEXT PRIMARY KEY,
                     kind       TEXT NOT NULL,
                     name       TEXT NOT NULL,
                     base_url   TEXT,
                     api_key    TEXT,
                     created_at INTEGER NOT NULL
                 ) STRICT;
                 CREATE TABLE ai_features (
                     feature  TEXT PRIMARY KEY,
                     provider TEXT NOT NULL REFERENCES ai_providers (id) ON DELETE CASCADE,
                     model    TEXT NOT NULL
                 ) STRICT;
                 PRAGMA user_version = 7;",
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
            forget_server_in(&tx)?;
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

    /// Forgets everything this device believed its server had: every document and file is
    /// offered again on the next sync. For a server that turns out not to have this device's
    /// workspace (its data was reset or restored, or it's another account), as for a new server.
    pub fn forget_server(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        forget_server_in(&tx)?;
        tx.commit()?;
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

    // ---- Agents on this computer ----

    /// Whether the local MCP endpoint is on, and its port.
    pub fn local_agents(&self) -> Result<Option<(bool, u16)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT enabled, port FROM local_agents WHERE id = 1",
                [],
                |r| Ok((r.get::<_, i64>(0)? != 0, r.get::<_, i64>(1)?)),
            )
            .optional()?
            .map(|(on, port)| (on, u16::try_from(port).unwrap_or(0))))
    }

    pub fn set_local_agents(&mut self, enabled: bool, port: u16) -> Result<()> {
        self.conn.execute(
            "INSERT INTO local_agents (id, enabled, port) VALUES (1, ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET enabled = excluded.enabled, port = excluded.port",
            params![i64::from(enabled), i64::from(port)],
        )?;
        Ok(())
    }

    pub fn agent_tokens(&self) -> Result<Vec<AgentTokenRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, access, created_at, last_used_at FROM agent_tokens
             ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(AgentTokenRow {
                id: r.get(0)?,
                name: r.get(1)?,
                access: r.get(2)?,
                created_at: r.get(3)?,
                last_used_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn add_agent_token(&mut self, row: &AgentTokenRow, hash: &[u8]) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_tokens (id, name, token_hash, access, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![row.id, row.name, hash, row.access, row.created_at],
        )?;
        Ok(())
    }

    pub fn remove_agent_token(&mut self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM agent_tokens WHERE id = ?1", [id])?
            > 0)
    }

    /// The token with this hash, marked as used at `now`.
    pub fn use_agent_token(&mut self, hash: &[u8], now: i64) -> Result<Option<AgentTokenRow>> {
        let row = self
            .conn
            .query_row(
                "UPDATE agent_tokens SET last_used_at = ?2 WHERE token_hash = ?1
                 RETURNING id, name, access, created_at, last_used_at",
                params![hash, now],
                |r| {
                    Ok(AgentTokenRow {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        access: r.get(2)?,
                        created_at: r.get(3)?,
                        last_used_at: r.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn add_agent_activity(&mut self, row: &AgentActivityRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_activity (id, agent, tool, summary, effects, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row.id,
                row.agent,
                row.tool,
                row.summary,
                row.effects,
                row.at
            ],
        )?;
        Ok(())
    }

    /// The newest `limit` calls, newest first.
    pub fn agent_activity(&self, limit: u32) -> Result<Vec<AgentActivityRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent, tool, summary, effects, at, undone_at FROM agent_activity
             ORDER BY at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], activity_row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn agent_call(&self, id: &str) -> Result<Option<AgentActivityRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, agent, tool, summary, effects, at, undone_at FROM agent_activity
                 WHERE id = ?1",
                [id],
                activity_row,
            )
            .optional()?)
    }

    pub fn mark_agent_call_undone(&mut self, id: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_activity SET undone_at = ?2 WHERE id = ?1 AND undone_at IS NULL",
            params![id, now],
        )?;
        Ok(())
    }

    // ---- AI providers ----

    pub fn ai_providers(&self) -> Result<Vec<AiProviderRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, base_url, api_key, created_at FROM ai_providers
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], ai_provider_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn ai_provider(&self, id: &str) -> Result<Option<AiProviderRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, kind, name, base_url, api_key, created_at FROM ai_providers
                 WHERE id = ?1",
                [id],
                ai_provider_row,
            )
            .optional()?)
    }

    /// Adds a provider, or replaces the one with its ID.
    pub fn put_ai_provider(&mut self, row: &AiProviderRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO ai_providers (id, kind, name, base_url, api_key, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET kind = excluded.kind, name = excluded.name,
                 base_url = excluded.base_url, api_key = excluded.api_key",
            params![
                row.id,
                row.kind,
                row.name,
                row.base_url,
                row.api_key.as_ref().map(|k| k.as_str()),
                row.created_at
            ],
        )?;
        Ok(())
    }

    /// Removes a provider, and the features that used it.
    pub fn remove_ai_provider(&mut self, id: &str) -> Result<bool> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM ai_features WHERE provider = ?1", [id])?;
        let removed = tx.execute("DELETE FROM ai_providers WHERE id = ?1", [id])? > 0;
        tx.commit()?;
        Ok(removed)
    }

    /// Each feature's provider and model: `(feature, provider, model)`.
    pub fn ai_features(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT feature, provider, model FROM ai_features ORDER BY feature")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_ai_feature(&mut self, feature: &str, choice: Option<(&str, &str)>) -> Result<()> {
        match choice {
            Some((provider, model)) => self.conn.execute(
                "INSERT INTO ai_features (feature, provider, model) VALUES (?1, ?2, ?3)
                 ON CONFLICT (feature) DO UPDATE SET provider = excluded.provider,
                     model = excluded.model",
                params![feature, provider, model],
            )?,
            None => self
                .conn
                .execute("DELETE FROM ai_features WHERE feature = ?1", [feature])?,
        };
        Ok(())
    }

    // ---- The Markdown bridge ----

    pub fn bridge_folder(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT folder FROM bridge WHERE id = 1", [], |r| r.get(0))
            .optional()?)
    }

    /// Sets the bridge's folder (or turns it off), forgetting every file of the old one.
    pub fn set_bridge_folder(&mut self, folder: Option<&str>) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM bridge_files", [])?;
        match folder {
            Some(f) => tx.execute(
                "INSERT INTO bridge (id, folder) VALUES (1, ?1)
                 ON CONFLICT (id) DO UPDATE SET folder = excluded.folder",
                [f],
            )?,
            None => tx.execute("DELETE FROM bridge", [])?,
        };
        tx.commit()?;
        Ok(())
    }

    pub fn bridge_files(&self) -> Result<Vec<BridgeFile>> {
        let mut stmt = self
            .conn
            .prepare("SELECT page, path, version, hash, meta, mtime, size FROM bridge_files")?;
        let rows = stmt.query_map([], |r| {
            Ok(BridgeFile {
                page: r.get(0)?,
                path: r.get(1)?,
                version: r.get(2)?,
                hash: r.get(3)?,
                meta: r.get(4)?,
                mtime: r.get(5)?,
                size: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn put_bridge_file(&mut self, f: &BridgeFile) -> Result<()> {
        let tx = self.conn.transaction()?;
        // A path belongs to one page: another that had it has moved on.
        tx.execute(
            "DELETE FROM bridge_files WHERE path = ?1 AND page <> ?2",
            params![f.path, f.page],
        )?;
        tx.execute(
            "INSERT INTO bridge_files (page, path, version, hash, meta, mtime, size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (page) DO UPDATE SET path = excluded.path, version = excluded.version,
                 hash = excluded.hash, meta = excluded.meta, mtime = excluded.mtime,
                 size = excluded.size",
            params![f.page, f.path, f.version, f.hash, f.meta, f.mtime, f.size],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_bridge_file(&mut self, page: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM bridge_files WHERE page = ?1", [page])?;
        Ok(())
    }

    /// The attachment whose ID starts with `prefix`, if exactly one does.
    pub fn blob_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(None);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM blobs WHERE id LIKE ?1 || '%' LIMIT 2")?;
        let ids: Vec<String> = stmt
            .query_map([prefix], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(match ids.as_slice() {
            [one] => Some(one.clone()),
            _ => None,
        })
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

/// See [`Store::forget_server`].
fn forget_server_in(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute("DELETE FROM sync_state", [])?;
    tx.execute("UPDATE blobs SET uploaded = 0", [])?;
    Ok(())
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
