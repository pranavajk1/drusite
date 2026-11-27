// SPDX-License-Identifier: AGPL-3.0-or-later

//! Attachments over HTTP: `PUT` and `GET /api/v1/workspaces/{ws}/attachments/{blob}`.
//!
//! `blob` is the BLAKE3 hash of the file, as a page's attachment block names it. Uploads are
//! checked against it, so a file can only be stored under its own hash, and storing one twice
//! is harmless.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::put;
use drusite_model::WorkspaceId;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::{AuthError, Authenticated};
use crate::crypto::WorkspaceKey;
use crate::policy::{Action, Principal, Resource};
use crate::storage::{BlobId, StorageError};
use crate::sync::store;

pub fn routes(state: &AppState) -> Router<AppState> {
    let limit = usize::try_from(state.config().attachments.max_bytes).unwrap_or(usize::MAX);
    Router::new()
        .route(
            "/api/v1/workspaces/{ws}/attachments/{blob}",
            put(upload).get(download),
        )
        .layer(DefaultBodyLimit::max(limit))
}

/// The hash a row is found by: the content hash keyed with one version of the workspace key.
fn lookup(key: &WorkspaceKey, blob: &BlobId) -> String {
    let k = key.derive("drusite 2026 attachment lookup");
    blake3::keyed_hash(&k, blob.as_str().as_bytes())
        .to_hex()
        .to_string()
}

fn parse(blob: &str) -> Result<BlobId, AuthError> {
    BlobId::parse(blob).ok_or_else(|| AuthError::bad("bad_blob", "not an attachment ID"))
}

fn storage(e: StorageError) -> AuthError {
    AuthError::internal(e)
}

/// The row for `blob`, under any version of the workspace's key: its key version and address.
async fn find(
    state: &AppState,
    tx: &mut sqlx::PgConnection,
    ws: Uuid,
    blob: &BlobId,
) -> Result<Option<(i32, String)>, AuthError> {
    let newest = state
        .keys()
        .current(tx, WorkspaceId::from_uuid(ws))
        .await
        .map_err(AuthError::internal)?;
    let mut lookups = Vec::new();
    for v in 1..=newest.version() {
        let key = state
            .keys()
            .version(tx, WorkspaceId::from_uuid(ws), v)
            .await
            .map_err(AuthError::internal)?;
        lookups.push(lookup(&key, blob));
    }
    Ok(sqlx::query_as(
        "SELECT key_version, address FROM attachments WHERE workspace_id = $1 AND lookup = ANY($2)",
    )
    .bind(ws)
    .bind(&lookups)
    .fetch_optional(&mut *tx)
    .await?)
}

fn allowed(
    state: &AppState,
    auth: &Authenticated,
    role: Option<crate::policy::Role>,
    action: Action,
    ws: Uuid,
) -> bool {
    state.policy().is_allowed(
        &Principal {
            user: auth.user,
            disabled: false,
            role,
        },
        action,
        &Resource::Workspace(WorkspaceId::from_uuid(ws)),
    )
}

async fn upload(
    State(state): State<AppState>,
    auth: Authenticated,
    Path((ws, blob)): Path<(Uuid, String)>,
    body: Bytes,
) -> Result<StatusCode, AuthError> {
    let blob = parse(&blob)?;
    if !auth.can_write() {
        return Err(AuthError::Forbidden(
            "read_only",
            "this sign-in can't change notes",
        ));
    }
    if blake3::hash(&body).to_hex().as_str() != blob.as_str() {
        return Err(AuthError::bad(
            "hash_mismatch",
            "the file doesn't match its ID",
        ));
    }
    let mut tx = state.db().as_user(auth.user).await?;
    let role = store::role(&mut tx, ws)
        .await
        .map_err(AuthError::internal)?;
    if !allowed(&state, &auth, role, Action::CreateDocument, ws) {
        // Not a member: as if the workspace weren't there.
        return Err(if role.is_none() {
            AuthError::NotFound
        } else {
            AuthError::Forbidden("forbidden", "you can't add files to this workspace")
        });
    }
    if find(&state, &mut tx, ws, &blob).await?.is_some() {
        return Ok(StatusCode::NO_CONTENT);
    }
    let key = state
        .keys()
        .current(&mut tx, WorkspaceId::from_uuid(ws))
        .await
        .map_err(AuthError::internal)?;
    let address = state.blobs().put(&key, &body).await.map_err(storage)?;
    sqlx::query(
        "INSERT INTO attachments (workspace_id, lookup, key_version, address, size, created_by)
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
    )
    .bind(ws)
    .bind(lookup(&key, &blob))
    .bind(i32::try_from(key.version()).unwrap_or(i32::MAX))
    .bind(address.as_str())
    .bind(i64::try_from(body.len()).unwrap_or(i64::MAX))
    .bind(auth.user.as_uuid())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StatusCode::CREATED)
}

async fn download(
    State(state): State<AppState>,
    auth: Authenticated,
    Path((ws, blob)): Path<(Uuid, String)>,
) -> Result<Response, AuthError> {
    let blob = parse(&blob)?;
    let mut tx = state.db().as_user(auth.user).await?;
    let role = store::role(&mut tx, ws)
        .await
        .map_err(AuthError::internal)?;
    if !allowed(&state, &auth, role, Action::ReadWorkspace, ws) {
        return Err(AuthError::NotFound);
    }
    let Some((version, address)) = find(&state, &mut tx, ws, &blob).await? else {
        return Err(AuthError::NotFound);
    };
    let key = state
        .keys()
        .version(
            &mut tx,
            WorkspaceId::from_uuid(ws),
            u32::try_from(version).unwrap_or(0),
        )
        .await
        .map_err(AuthError::internal)?;
    tx.commit().await?;
    let address = BlobId::parse(&address)
        .ok_or_else(|| AuthError::internal(anyhow::anyhow!("bad address")))?;
    let bytes = state.blobs().get(&key, &address).await.map_err(storage)?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            ),
            // The same ID is always the same file.
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=31536000, immutable"),
            ),
        ],
        bytes,
    )
        .into_response())
}
