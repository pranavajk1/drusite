// SPDX-License-Identifier: AGPL-3.0-or-later

//! Saved conversations: kept in the account, so every device the person signs in on has them.
//! Titles and messages are sealed with the workspace's key; only the asker can read them
//! (row-level security), not the workspace's other members.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, State};
use drusite_model::{BlockId, WorkspaceId};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use time::OffsetDateTime;
use uuid::Uuid;

use super::AskError;
use crate::app::AppState;
use crate::auth::Authenticated;

/// The longest title kept.
pub const MAX_TITLE: usize = 120;

/// A source as a message keeps it: its number in the answer, and where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub n: usize,
    pub page: Uuid,
    /// None when the page stood in by its title.
    pub block: Option<BlockId>,
}

fn context(kind: &[u8], id: Uuid) -> Vec<u8> {
    let mut c = b"drusite ask ".to_vec();
    c.extend(kind);
    c.push(0);
    c.extend(id.as_bytes());
    c
}

pub async fn seal(
    state: &AppState,
    conn: &mut PgConnection,
    ws: Uuid,
    kind: &[u8],
    id: Uuid,
    text: &str,
) -> anyhow::Result<Vec<u8>> {
    let key = state
        .keys()
        .current(conn, WorkspaceId::from_uuid(ws))
        .await?;
    Ok(key.encrypt(&context(kind, id), text.as_bytes())?)
}

pub async fn open(
    state: &AppState,
    conn: &mut PgConnection,
    ws: Uuid,
    kind: &[u8],
    id: Uuid,
    sealed: &[u8],
) -> anyhow::Result<String> {
    let version = crate::crypto::sealed_key_version(sealed)?;
    let key = state
        .keys()
        .version(conn, WorkspaceId::from_uuid(ws), version)
        .await?;
    let plain = key.decrypt(&context(kind, id), sealed)?;
    Ok(String::from_utf8_lossy(&plain).into_owned())
}

/// A title from the first question.
pub fn title_of(question: &str) -> String {
    let one_line: String = question.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= 60 {
        return one_line;
    }
    let mut t: String = one_line.chars().take(59).collect();
    if let Some(space) = t.rfind(' ').filter(|&i| i > 30) {
        t.truncate(space);
    }
    t.push('…');
    t
}

/// Makes a conversation; its title is the first question's.
pub async fn create(
    state: &AppState,
    conn: &mut PgConnection,
    ws: Uuid,
    user: Uuid,
    title: &str,
) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    let sealed = seal(state, conn, ws, b"title", id, title).await?;
    sqlx::query(
        "INSERT INTO ask_conversations (id, workspace_id, user_id, title) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(ws)
    .bind(user)
    .bind(sealed)
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

/// Whether `id` is one of the asker's conversations in `ws`, and its title.
pub async fn find(
    state: &AppState,
    conn: &mut PgConnection,
    ws: Uuid,
    id: Uuid,
) -> anyhow::Result<Option<String>> {
    let row: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT title FROM ask_conversations WHERE id = $1 AND workspace_id = $2",
    )
    .bind(id)
    .bind(ws)
    .fetch_optional(&mut *conn)
    .await?;
    match row {
        Some(sealed) => Ok(Some(open(state, conn, ws, b"title", id, &sealed).await?)),
        None => Ok(None),
    }
}

pub struct NewMessage<'a> {
    pub conversation: Uuid,
    pub ws: Uuid,
    pub user: Uuid,
    pub role: &'static str,
    pub text: &'a str,
    pub sources: &'a [SourceRef],
    pub model: Option<&'a str>,
    pub error: Option<&'a str>,
}

pub async fn add_message(
    state: &AppState,
    conn: &mut PgConnection,
    m: &NewMessage<'_>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    let sealed = seal(state, conn, m.ws, b"message", id, m.text).await?;
    sqlx::query(
        "INSERT INTO ask_messages (id, conversation_id, workspace_id, user_id, role, content,
             sources, model, error)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(m.conversation)
    .bind(m.ws)
    .bind(m.user)
    .bind(m.role)
    .bind(sealed)
    .bind(serde_json::to_value(m.sources)?)
    .bind(m.model)
    .bind(m.error)
    .execute(&mut *conn)
    .await?;
    sqlx::query("UPDATE ask_conversations SET updated_at = now() WHERE id = $1")
        .bind(m.conversation)
        .execute(&mut *conn)
        .await?;
    Ok(id)
}

/// A message, opened. Its `Debug` leaves the text out.
pub struct Stored {
    pub id: Uuid,
    pub role: String,
    pub text: String,
    pub sources: Vec<SourceRef>,
    pub model: Option<String>,
    pub error: Option<String>,
    pub at: OffsetDateTime,
}

type MessageRow = (
    Uuid,
    String,
    Vec<u8>,
    serde_json::Value,
    Option<String>,
    Option<String>,
    OffsetDateTime,
);

/// A conversation's messages, oldest first.
pub async fn messages(
    state: &AppState,
    conn: &mut PgConnection,
    ws: Uuid,
    conversation: Uuid,
) -> anyhow::Result<Vec<Stored>> {
    let rows: Vec<MessageRow> = sqlx::query_as(
        "SELECT id, role, content, sources, model, error, created_at FROM ask_messages
         WHERE conversation_id = $1 AND workspace_id = $2 ORDER BY created_at, id",
    )
    .bind(conversation)
    .bind(ws)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, role, sealed, sources, model, error, at) in rows {
        out.push(Stored {
            text: open(state, conn, ws, b"message", id, &sealed).await?,
            id,
            role,
            sources: serde_json::from_value(sources).unwrap_or_default(),
            model,
            error,
            at,
        });
    }
    Ok(out)
}

impl std::fmt::Debug for Stored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stored")
            .field("id", &self.id)
            .field("role", &self.role)
            .field("chars", &self.text.chars().count())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for NewMessage<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewMessage")
            .field("conversation", &self.conversation)
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

pub fn millis(t: OffsetDateTime) -> i64 {
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap_or(0)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Summary {
    id: Uuid,
    title: String,
    created_at: i64,
    updated_at: i64,
}

/// `GET /api/v1/workspaces/{ws}/conversations`: the asker's, most recent first.
pub(super) async fn list(
    State(state): State<AppState>,
    auth: Authenticated,
    Path(ws): Path<Uuid>,
) -> Result<Json<Vec<Summary>>, AskError> {
    let mut tx = state.db().as_user(auth.user).await?;
    super::require_reader(&state, &mut tx, auth.user, ws).await?;
    let rows: Vec<(Uuid, Vec<u8>, OffsetDateTime, OffsetDateTime)> = sqlx::query_as(
        "SELECT id, title, created_at, updated_at FROM ask_conversations
         WHERE workspace_id = $1 AND user_id = drusite_user_id()
         ORDER BY updated_at DESC LIMIT 500",
    )
    .bind(ws)
    .fetch_all(&mut *tx)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, sealed, created, updated) in rows {
        out.push(Summary {
            title: open(&state, &mut tx, ws, b"title", id, &sealed).await?,
            id,
            created_at: millis(created),
            updated_at: millis(updated),
        });
    }
    tx.commit().await?;
    Ok(Json(out))
}

/// A source as the apps show it: where it is, and what it's called now.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SourceView {
    n: usize,
    page: Uuid,
    block: Option<BlockId>,
    title: String,
    /// The page is gone, or no longer something Ask may read.
    missing: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MessageView {
    id: Uuid,
    role: String,
    text: String,
    sources: Vec<SourceView>,
    model: Option<String>,
    error: Option<String>,
    at: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConversationView {
    id: Uuid,
    title: String,
    messages: Vec<MessageView>,
}

/// `GET /api/v1/workspaces/{ws}/conversations/{id}`.
pub(super) async fn get(
    State(state): State<AppState>,
    auth: Authenticated,
    Path((ws, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ConversationView>, AskError> {
    let mut tx = state.db().as_user(auth.user).await?;
    super::require_reader(&state, &mut tx, auth.user, ws).await?;
    let title = find(&state, &mut tx, ws, id)
        .await?
        .ok_or(AskError::NotFound)?;
    let stored = messages(&state, &mut tx, ws, id).await?;
    let pages: HashMap<Uuid, String> = super::readable_pages(&state, &mut tx, ws)
        .await?
        .map(|r| r.pages)
        .unwrap_or_default();
    tx.commit().await?;
    let messages = stored
        .into_iter()
        .map(|m| MessageView {
            id: m.id,
            role: m.role,
            text: m.text,
            sources: m
                .sources
                .into_iter()
                .map(|s| SourceView {
                    n: s.n,
                    title: pages.get(&s.page).cloned().unwrap_or_default(),
                    missing: !pages.contains_key(&s.page),
                    page: s.page,
                    block: s.block,
                })
                .collect(),
            model: m.model,
            error: m.error,
            at: millis(m.at),
        })
        .collect();
    Ok(Json(ConversationView {
        id,
        title,
        messages,
    }))
}

#[derive(Deserialize)]
pub(super) struct Rename {
    title: String,
}

/// `PATCH /api/v1/workspaces/{ws}/conversations/{id}`: renames it.
pub(super) async fn rename(
    State(state): State<AppState>,
    auth: Authenticated,
    Path((ws, id)): Path<(Uuid, Uuid)>,
    Json(body): Json<Rename>,
) -> Result<Json<serde_json::Value>, AskError> {
    let title: String = body.title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() || title.chars().count() > MAX_TITLE {
        return Err(AskError::Invalid(format!(
            "a title has 1 to {MAX_TITLE} characters"
        )));
    }
    let mut tx = state.db().as_user(auth.user).await?;
    super::require_reader(&state, &mut tx, auth.user, ws).await?;
    if find(&state, &mut tx, ws, id).await?.is_none() {
        return Err(AskError::NotFound);
    }
    let sealed = seal(&state, &mut tx, ws, b"title", id, &title).await?;
    sqlx::query("UPDATE ask_conversations SET title = $2 WHERE id = $1")
        .bind(id)
        .bind(sealed)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(serde_json::json!({ "id": id, "title": title })))
}

/// `DELETE /api/v1/workspaces/{ws}/conversations/{id}`: deletes it and its messages.
pub(super) async fn delete(
    State(state): State<AppState>,
    auth: Authenticated,
    Path((ws, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AskError> {
    let mut tx = state.db().as_user(auth.user).await?;
    super::require_reader(&state, &mut tx, auth.user, ws).await?;
    let gone = sqlx::query("DELETE FROM ask_conversations WHERE id = $1 AND workspace_id = $2")
        .bind(id)
        .bind(ws)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    if gone == 0 {
        return Err(AskError::NotFound);
    }
    Ok(Json(serde_json::json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_from_questions() {
        assert_eq!(title_of("  What did   I bake?\n"), "What did I bake?");
        let long = "When did I last talk to the plumber about the leaking pipe under the sink";
        let t = title_of(long);
        assert!(t.ends_with('…') && t.chars().count() <= 60, "{t}");
    }
}
