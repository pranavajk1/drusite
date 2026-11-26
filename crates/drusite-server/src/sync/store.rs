// SPDX-License-Identifier: AGPL-3.0-or-later

//! Documents on the server: each one a log of Loro updates and, once compacted, a snapshot.
//!
//! Every read and write runs as the user it's for, so row-level security has the last word
//! after Cedar. Entries are sealed with the workspace's key, bound to their document and
//! sequence number.

use drusite_model::{UserId, WorkspaceId};
use drusite_proto::{DocKind, Entry};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::app::AppState;
use crate::crypto::sealed_key_version;
use crate::policy::{Action, Principal, Resource, Role};

/// Updates since the snapshot before the server compacts a document.
pub const COMPACT_AFTER: i64 = 500;
/// The biggest single update or snapshot accepted.
pub const MAX_UPDATE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("not allowed")]
    Forbidden,
    #[error("no such document")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
    #[error("too large")]
    TooLarge,
    /// The update builds on changes the log doesn't have. Carries what the log does have.
    #[error("the update builds on changes the server doesn't have")]
    Gap(Vec<u8>),
    #[error(transparent)]
    Server(#[from] anyhow::Error),
}

macro_rules! server_error {
    ($($t:ty),*) => {$(
        impl From<$t> for DocError {
            fn from(e: $t) -> Self {
                Self::Server(e.into())
            }
        }
    )*};
}
server_error!(
    sqlx::Error,
    crate::db::DbError,
    crate::crypto::CryptoError,
    crate::keys::KeyError
);

/// What a sealed entry is bound to.
fn context(doc: Uuid, seq: i64, snapshot: bool) -> Vec<u8> {
    let mut c = b"drusite doc entry\0".to_vec();
    c.extend(doc.as_bytes());
    c.extend(seq.to_be_bytes());
    c.push(u8::from(snapshot));
    c
}

/// The user's role in a workspace, read through row-level security.
pub async fn role(conn: &mut PgConnection, ws: Uuid) -> Result<Option<Role>, DocError> {
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM workspace_members WHERE workspace_id = $1 AND user_id = drusite_user_id()",
    )
    .bind(ws)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(role.and_then(|r| r.parse().ok()))
}

fn allowed(
    state: &AppState,
    user: UserId,
    role: Option<Role>,
    action: Action,
    resource: Resource,
) -> bool {
    state.policy().is_allowed(
        &Principal {
            user,
            disabled: false,
            role,
        },
        action,
        &resource,
    )
}

/// Checks a Loro blob is well-formed before it's stored, so nobody can poison a document for
/// everyone else.
pub fn check_blob(bytes: &[u8]) -> Result<loro::ImportBlobMetadata, DocError> {
    if bytes.len() > MAX_UPDATE_BYTES {
        return Err(DocError::TooLarge);
    }
    loro::LoroDoc::decode_import_blob_meta(bytes, true)
        .map_err(|e| DocError::Invalid(format!("not a valid Loro update: {e}")))
}

fn decode_version(bytes: &[u8]) -> Result<loro::VersionVector, DocError> {
    if bytes.is_empty() {
        return Ok(loro::VersionVector::default());
    }
    loro::VersionVector::decode(bytes)
        .map_err(|e| DocError::Server(anyhow::anyhow!("a stored version doesn't decode: {e}")))
}

/// Checks a new document's first snapshot really is that document.
fn check_first_snapshot(doc: Uuid, kind: DocKind, snapshot: &[u8]) -> Result<(), DocError> {
    let _ = check_blob(snapshot)?;
    let id = match kind {
        DocKind::Page => drusite_crdt::Page::load(snapshot)
            .map(|p| *p.id().as_uuid())
            .map_err(|e| DocError::Invalid(format!("not a page: {e}")))?,
        DocKind::Workspace => drusite_crdt::Workspace::load(snapshot)
            .map(|w| *w.id().as_uuid())
            .map_err(|e| DocError::Invalid(format!("not a workspace: {e}")))?,
        DocKind::Unspecified => return Err(DocError::Invalid("no document kind".to_owned())),
    };
    if id != doc {
        return Err(DocError::Invalid(
            "the snapshot is of another document".to_owned(),
        ));
    }
    Ok(())
}

/// A subscription the server has accepted.
#[derive(Debug, Clone)]
pub struct Opened {
    pub workspace: Uuid,
    pub writable: bool,
    pub created: bool,
    /// Every version the server's log holds; empty for a page that doesn't exist yet.
    pub version: Vec<u8>,
}

/// Opens a document for `user`, creating it (and, for a workspace document, the workspace) from
/// `create` if it doesn't exist yet.
pub async fn open(
    state: &AppState,
    user: UserId,
    doc: Uuid,
    workspace: Uuid,
    kind: DocKind,
    create: &[u8],
    origin: Uuid,
) -> Result<Opened, DocError> {
    if kind == DocKind::Workspace && doc != workspace {
        return Err(DocError::Invalid(
            "a workspace document's ID is its workspace's".to_owned(),
        ));
    }
    let mut tx = state.db().as_user(user).await?;
    let mut role = role(&mut tx, workspace).await?;
    let existing: Option<(Uuid, String)> =
        sqlx::query_as("SELECT workspace_id, kind FROM documents WHERE id = $1")
            .bind(doc)
            .fetch_optional(&mut *tx)
            .await?;
    let created = match existing {
        Some((ws, stored_kind)) => {
            let expected = if kind == DocKind::Workspace {
                "workspace"
            } else {
                "page"
            };
            if ws != workspace || stored_kind != expected {
                return Err(DocError::Invalid(
                    "the document belongs elsewhere".to_owned(),
                ));
            }
            false
        }
        // A page this device has seen in the tree before its creator has put it here: the
        // subscription waits for it, and the page arrives when it's created. A workspace that
        // doesn't exist is something to join, so that's an error.
        None if create.is_empty() && kind == DocKind::Page && role.is_some() => false,
        None if create.is_empty() => {
            return Err(if role.is_some() {
                DocError::NotFound
            } else {
                DocError::Forbidden
            });
        }
        None => {
            check_first_snapshot(doc, kind, create)?;
            if kind == DocKind::Workspace {
                // The first device to sync a workspace claims it for its user.
                claim_workspace(state, &mut tx, user, workspace).await?;
                role = Some(Role::Owner);
            } else if !allowed(
                state,
                user,
                role,
                Action::CreateDocument,
                Resource::Workspace(WorkspaceId::from_uuid(workspace)),
            ) {
                return Err(DocError::Forbidden);
            }
            let inserted = sqlx::query(
                "INSERT INTO documents (id, workspace_id, kind, created_by)
                 VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            )
            .bind(doc)
            .bind(workspace)
            .bind(if kind == DocKind::Workspace {
                "workspace"
            } else {
                "page"
            })
            .bind(user.as_uuid())
            .execute(&mut *tx)
            .await?;
            if inserted.rows_affected() == 0 {
                // Taken by a document this user can't see.
                return Err(DocError::Forbidden);
            }
            append_in(state, &mut tx, user, doc, workspace, create, Some(origin)).await?;
            true
        }
    };
    let resource = Resource::Document {
        workspace: WorkspaceId::from_uuid(workspace),
        document: doc,
    };
    if !allowed(state, user, role, Action::ReadDocument, resource) {
        return Err(DocError::Forbidden);
    }
    let writable = allowed(state, user, role, Action::WriteDocument, resource);
    let version: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT known_version FROM documents WHERE id = $1")
            .bind(doc)
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(Opened {
        workspace,
        writable,
        created,
        version: version.unwrap_or_default(),
    })
}

async fn claim_workspace(
    state: &AppState,
    tx: &mut PgConnection,
    user: UserId,
    workspace: Uuid,
) -> Result<(), DocError> {
    let inserted = sqlx::query(
        "INSERT INTO workspaces (id, created_by) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(workspace)
    .bind(user.as_uuid())
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() == 0 {
        // Someone else's: this user isn't a member, or the document row would be visible.
        return Err(DocError::Forbidden);
    }
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(workspace)
    .bind(user.as_uuid())
    .execute(&mut *tx)
    .await?;
    state
        .keys()
        .create(tx, WorkspaceId::from_uuid(workspace))
        .await?;
    tracing::info!(%workspace, %user, "workspace claimed");
    Ok(())
}

/// Appends an update inside the caller's transaction; returns its sequence number and whether
/// the document is due for compaction.
async fn append_in(
    state: &AppState,
    tx: &mut PgConnection,
    user: UserId,
    doc: Uuid,
    workspace: Uuid,
    update: &[u8],
    origin: Option<Uuid>,
) -> Result<(i64, bool), DocError> {
    let meta = check_blob(update)?;
    let row: Option<(i64, i64, Vec<u8>)> = sqlx::query_as(
        "SELECT head_seq, snapshot_seq, known_version FROM documents
         WHERE id = $1 AND workspace_id = $2 FOR UPDATE",
    )
    .bind(doc)
    .bind(workspace)
    .fetch_optional(&mut *tx)
    .await?;
    let (head, snapshot, known) = row.ok_or(DocError::NotFound)?;
    // A log with a hole can't be compacted without losing what's after the hole, and nobody
    // could import it whole. Refuse, and say what the log has.
    let mut known = decode_version(&known)?;
    if !known.includes_vv(&meta.partial_start_vv) {
        return Err(DocError::Gap(known.encode()));
    }
    known.merge(&meta.partial_end_vv);
    let seq = head + 1;
    let key = state
        .keys()
        .current(tx, WorkspaceId::from_uuid(workspace))
        .await?;
    let sealed = key.encrypt(&context(doc, seq, false), update)?;
    sqlx::query(
        "INSERT INTO document_updates (doc_id, workspace_id, seq, data, origin, created_by)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(doc)
    .bind(workspace)
    .bind(seq)
    .bind(sealed)
    .bind(origin)
    .bind(user.as_uuid())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE documents SET head_seq = $2, known_version = $3, updated_at = now() WHERE id = $1",
    )
    .bind(doc)
    .bind(seq)
    .bind(known.encode())
    .execute(&mut *tx)
    .await?;
    // Delivered to every node's listener when the transaction commits.
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(super::CHANNEL)
        .bind(doc.to_string())
        .execute(&mut *tx)
        .await?;
    Ok((seq, seq - snapshot >= COMPACT_AFTER))
}

/// Appends a pushed update. The caller has checked the subscription may write.
pub async fn append(
    state: &AppState,
    user: UserId,
    doc: Uuid,
    workspace: Uuid,
    update: &[u8],
    origin: Uuid,
) -> Result<(i64, bool), DocError> {
    let mut tx = state.db().as_user(user).await?;
    let result = append_in(state, &mut tx, user, doc, workspace, update, Some(origin)).await?;
    tx.commit().await?;
    Ok(result)
}

/// Everything after `after`, as `user` sees it: the snapshot first if the log has been
/// compacted past `after`, then the updates. Entries from `skip_origin` are left out but still
/// count, so the returned high-water mark moves past them.
pub async fn entries_after(
    state: &AppState,
    user: UserId,
    doc: Uuid,
    workspace: Uuid,
    after: i64,
    skip_origin: Option<Uuid>,
) -> Result<(Vec<Entry>, i64), DocError> {
    // One consistent view, so a compaction in between can't drop updates from under us.
    let mut tx = state.db().as_user_consistent(user).await?;
    let mut entries = Vec::new();
    let mut high = after;
    let snapshot: Option<(i64, Vec<u8>)> = sqlx::query_as(
        "SELECT through_seq, data FROM document_snapshots WHERE doc_id = $1 AND workspace_id = $2",
    )
    .bind(doc)
    .bind(workspace)
    .fetch_optional(&mut *tx)
    .await?;
    let mut from = after;
    if let Some((through, sealed)) = snapshot
        && through > after
    {
        let data = open_sealed(state, &mut tx, workspace, doc, through, true, &sealed).await?;
        entries.push(Entry {
            seq: u64::try_from(through).unwrap_or_default(),
            data,
            snapshot: true,
        });
        from = through;
        high = through;
    }
    let rows: Vec<(i64, Vec<u8>, Option<Uuid>)> = sqlx::query_as(
        "SELECT seq, data, origin FROM document_updates
         WHERE doc_id = $1 AND workspace_id = $2 AND seq > $3 ORDER BY seq",
    )
    .bind(doc)
    .bind(workspace)
    .bind(from)
    .fetch_all(&mut *tx)
    .await?;
    for (seq, sealed, origin) in rows {
        high = seq;
        if origin.is_some() && origin == skip_origin {
            continue;
        }
        let data = open_sealed(state, &mut tx, workspace, doc, seq, false, &sealed).await?;
        entries.push(Entry {
            seq: u64::try_from(seq).unwrap_or_default(),
            data,
            snapshot: false,
        });
    }
    tx.commit().await?;
    Ok((entries, high))
}

async fn open_sealed(
    state: &AppState,
    conn: &mut PgConnection,
    workspace: Uuid,
    doc: Uuid,
    seq: i64,
    snapshot: bool,
    sealed: &[u8],
) -> Result<Vec<u8>, DocError> {
    let version = sealed_key_version(sealed)?;
    let key = state
        .keys()
        .version(conn, WorkspaceId::from_uuid(workspace), version)
        .await?;
    Ok(key.decrypt(&context(doc, seq, snapshot), sealed)?)
}

/// The workspaces `user` belongs to, and their role in each.
pub async fn workspaces(state: &AppState, user: UserId) -> Result<Vec<(Uuid, String)>, DocError> {
    let mut tx = state.db().as_user(user).await?;
    let rows = sqlx::query_as(
        "SELECT workspace_id, role FROM workspace_members WHERE user_id = drusite_user_id()
         ORDER BY added_at",
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// Folds a document's log into one snapshot and drops the updates it covers. Runs as the
/// system, with the document locked, so pushes wait for it rather than race it.
pub async fn compact(state: &AppState, doc: Uuid) -> Result<(), DocError> {
    let mut tx = state.db().as_system().await?;
    let row: Option<(Uuid, i64, i64)> = sqlx::query_as(
        "SELECT workspace_id, head_seq, snapshot_seq FROM documents WHERE id = $1 FOR UPDATE",
    )
    .bind(doc)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((workspace, head, snapshot_seq)) = row else {
        return Ok(());
    };
    if head <= snapshot_seq {
        return Ok(());
    }
    let loro = loro::LoroDoc::new();
    let snapshot: Option<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT through_seq, data FROM document_snapshots WHERE doc_id = $1")
            .bind(doc)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((through, sealed)) = snapshot {
        let data = open_sealed(state, &mut tx, workspace, doc, through, true, &sealed).await?;
        loro.import(&data)
            .map_err(|e| DocError::Invalid(format!("stored snapshot: {e}")))?;
    }
    let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
        "SELECT seq, data FROM document_updates WHERE doc_id = $1 AND seq <= $2 ORDER BY seq",
    )
    .bind(doc)
    .bind(head)
    .fetch_all(&mut *tx)
    .await?;
    let mut batch = Vec::with_capacity(rows.len());
    for (seq, sealed) in rows {
        batch.push(open_sealed(state, &mut tx, workspace, doc, seq, false, &sealed).await?);
    }
    let status = loro
        .import_batch(&batch)
        .map_err(|e| DocError::Invalid(format!("stored update: {e}")))?;
    if let Some(pending) = status.pending {
        // Never: pushes that would leave a hole are refused. If it happens anyway, keep the
        // log as it is rather than lose what the snapshot can't hold.
        return Err(DocError::Server(anyhow::anyhow!(
            "document {doc} has updates missing their dependencies ({pending:?}); not compacting"
        )));
    }
    let snapshot = loro
        .export(loro::ExportMode::Snapshot)
        .map_err(|e| DocError::Server(anyhow::anyhow!("exporting a snapshot: {e}")))?;
    let key = state
        .keys()
        .current(&mut tx, WorkspaceId::from_uuid(workspace))
        .await?;
    let sealed = key.encrypt(&context(doc, head, true), &snapshot)?;
    sqlx::query(
        "INSERT INTO document_snapshots (doc_id, workspace_id, through_seq, data)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (doc_id) DO UPDATE
             SET through_seq = excluded.through_seq, data = excluded.data, created_at = now()",
    )
    .bind(doc)
    .bind(workspace)
    .bind(head)
    .bind(sealed)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM document_updates WHERE doc_id = $1 AND seq <= $2")
        .bind(doc)
        .bind(head)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE documents SET snapshot_seq = $2 WHERE id = $1")
        .bind(doc)
        .bind(head)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::debug!(%doc, through = head, "compacted");
    Ok(())
}
