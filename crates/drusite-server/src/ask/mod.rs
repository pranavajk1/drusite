// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ask your notes: a question gets an answer written by a chat model from the asker's own
//! pages, with citations to the blocks it came from.
//!
//! Every app uses this one path. The server finds the blocks (full-text and, when an embedding
//! model is configured, semantic search), only in a workspace the asker can read (the Cedar
//! policy, then row-level security on every query), leaves journal entries out unless the
//! workspace allows them, sends the blocks and the question to the configured chat model, and
//! streams the answer back. Conversations are saved in the account. What was sent to which
//! provider is recorded (`ai_audit`), without the text; nothing from the notes is logged.

mod answer;
pub mod audit;
pub mod conversations;
pub mod prompt;
pub mod retrieve;

use std::collections::HashMap;

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use drusite_ai::{AiError, Provider};
use drusite_crdt::{SettingValue, Workspace};
use drusite_model::{PageKind, UserId, WorkspaceId, journal::ASK_JOURNAL_SETTING};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::Authenticated;
use crate::config::Config;
use crate::policy::{Action, Principal, Resource};
use crate::sync::store;

/// The chat model answers are written by.
#[derive(Debug)]
pub struct Chat {
    pub provider: Provider,
    pub model: String,
    /// What the apps show it as.
    pub name: String,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
}

impl Chat {
    pub fn from_config(config: &Config) -> Result<Option<Self>, AiError> {
        let Some(c) = &config.ask.chat else {
            return Ok(None);
        };
        // No overall timeout on the client: an answer streams for as long as it takes, within
        // [ask] timeout_secs, enforced around the stream.
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| AiError::Network(e.to_string()))?;
        let provider = Provider::new(
            http,
            c.provider,
            c.base_url.as_ref(),
            c.api_key
                .as_ref()
                .map(|k| zeroize::Zeroizing::new(k.expose().to_owned())),
        )?;
        Ok(Some(Self {
            provider,
            model: c.model.clone(),
            name: c.name.clone().unwrap_or_else(|| c.model.clone()),
            max_tokens: c.max_tokens,
            temperature: c.temperature,
        }))
    }
}

/// Ask's errors, as JSON: `{"error": "...", "message": "..."}`.
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("{1}")]
    Unavailable(&'static str, String),
    #[error("{0}")]
    Invalid(String),
    #[error("not allowed")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Limited(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

macro_rules! internal_from {
    ($($t:ty),*) => {$(
        impl From<$t> for AskError {
            fn from(e: $t) -> Self {
                Self::Internal(e.into())
            }
        }
    )*};
}
internal_from!(sqlx::Error, crate::db::DbError, serde_json::Error);

impl From<store::DocError> for AskError {
    fn from(e: store::DocError) -> Self {
        match e {
            store::DocError::Forbidden => Self::Forbidden,
            store::DocError::NotFound => Self::NotFound,
            other => Self::Internal(other.into()),
        }
    }
}

impl IntoResponse for AskError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            Self::Unavailable(code, m) => (StatusCode::SERVICE_UNAVAILABLE, *code, m.clone()),
            Self::Invalid(m) => (StatusCode::BAD_REQUEST, "invalid", m.clone()),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden", self.to_string()),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found", self.to_string()),
            Self::Limited(m) => (StatusCode::TOO_MANY_REQUESTS, "rate_limited", m.clone()),
            Self::Internal(error) => {
                // The error names what failed, never what a page says.
                tracing::error!(error = format!("{error:#}"), "ask request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    "something went wrong on the server".to_owned(),
                )
            }
        };
        (
            status,
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({ "error": code, "message": message })),
        )
            .into_response()
    }
}

/// Refuses anyone the policy doesn't let read `ws`. Row-level security still has the last
/// word on every row read after this.
pub async fn require_reader(
    state: &AppState,
    conn: &mut PgConnection,
    user: UserId,
    ws: Uuid,
) -> Result<(), AskError> {
    let role = store::role(conn, ws).await?;
    let allowed = state.policy().is_allowed(
        &Principal {
            user,
            disabled: false,
            role,
            agent: None,
        },
        Action::ReadWorkspace,
        &Resource::Workspace(WorkspaceId::from_uuid(ws)),
    );
    if role.is_none() || !allowed {
        return Err(AskError::Forbidden);
    }
    Ok(())
}

/// The pages of a workspace Ask may read, as the asker sees it.
#[derive(Debug)]
pub struct Readable {
    /// Page → title, for every page Ask may use.
    pub pages: HashMap<Uuid, String>,
    /// Journal entries, when they're left out.
    pub excluded: Vec<Uuid>,
    /// Whether journal entries are in.
    pub journal: bool,
    /// Database rows' values ("Status: Done · Priority: High"), by row.
    pub values: HashMap<Uuid, String>,
}

/// Opens the workspace document (as the asker) for its pages and settings.
pub async fn readable_pages(
    state: &AppState,
    conn: &mut PgConnection,
    ws: Uuid,
) -> anyhow::Result<Option<Readable>> {
    let Some(doc) = store::load(state, conn, ws).await? else {
        return Ok(None);
    };
    if doc.kind != "workspace" {
        return Ok(None);
    }
    let workspace = Workspace::load_all(&doc.parts())?;
    let journal = workspace
        .settings()?
        .into_iter()
        .any(|(k, v)| k == ASK_JOURNAL_SETTING && v == SettingValue::Bool(true));
    let mut pages = HashMap::new();
    let mut excluded = Vec::new();
    for p in workspace.pages()? {
        if p.kind == PageKind::Journal && !journal {
            excluded.push(*p.id.as_uuid());
        } else {
            pages.insert(*p.id.as_uuid(), p.title);
        }
    }
    let values = crate::search::row_values(&workspace)?
        .into_iter()
        .map(|(k, v)| (*k.as_uuid(), v))
        .collect();
    Ok(Some(Readable {
        pages,
        excluded,
        journal,
        values,
    }))
}

#[derive(Deserialize)]
struct StatusQuery {
    workspace: Option<Uuid>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    /// Whether questions can be asked.
    available: bool,
    /// Why not: `disabled`, `search_off` or `no_chat_model`.
    reason: Option<&'static str>,
    /// In words, with what to do.
    message: Option<String>,
    /// The chat model's name, when there is one.
    model: Option<String>,
    /// Whether meaning is searched too, and with which model.
    semantic: bool,
    embedding_model: Option<String>,
    /// Documents of the workspace waiting to be indexed.
    indexing: i64,
    /// Whether the workspace lets Ask read journal entries.
    journal: bool,
}

/// `GET /api/v1/ask`: whether Ask works on this server, and with what.
async fn status(
    State(state): State<AppState>,
    auth: Authenticated,
    Query(q): Query<StatusQuery>,
) -> Result<Json<Status>, AskError> {
    let config = state.config();
    let (reason, message) = if !config.ask.enabled {
        (
            Some("disabled"),
            Some("Ask is turned off on this server.".to_owned()),
        )
    } else if !config.search.enabled {
        (
            Some("search_off"),
            Some(
                "Ask needs server search, which is turned off on this server ([search] enabled \
                 = false)."
                    .to_owned(),
            ),
        )
    } else if state.chat().is_none() {
        (
            Some("no_chat_model"),
            Some(
                "This server has no chat model yet. Whoever runs it can add one under [ask.chat] \
                 in its config: any vendor, or a self-hosted model behind an OpenAI-compatible \
                 endpoint (see deploy/README.md)."
                    .to_owned(),
            ),
        )
    } else {
        (None, None)
    };
    let mut indexing = 0;
    let mut journal = false;
    if let Some(ws) = q.workspace {
        let mut tx = state.db().as_user(auth.user).await?;
        require_reader(&state, &mut tx, auth.user, ws).await?;
        indexing = sqlx::query_scalar("SELECT count(*) FROM search_queue WHERE workspace_id = $1")
            .bind(ws)
            .fetch_one(&mut *tx)
            .await?;
        journal = readable_pages(&state, &mut tx, ws)
            .await?
            .is_some_and(|r| r.journal);
        tx.commit().await?;
    }
    Ok(Json(Status {
        available: reason.is_none(),
        reason,
        message,
        model: state.chat().map(|c| c.name.clone()),
        semantic: state.embedder().is_some(),
        embedding_model: state.embedder().map(|e| e.model_id().to_owned()),
        indexing,
        journal,
    }))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/ask", get(status))
        .route(
            "/api/v1/workspaces/{ws}/conversations",
            get(conversations::list),
        )
        .route(
            "/api/v1/workspaces/{ws}/conversations/{id}",
            get(conversations::get)
                .patch(conversations::rename)
                .delete(conversations::delete),
        )
        .route("/api/v1/workspaces/{ws}/ai-audit", get(audit::list))
}

/// The answer stream, which outlives the ordinary request timeout.
pub fn streaming_routes() -> Router<AppState> {
    Router::new().route("/api/v1/workspaces/{ws}/ask", post(answer::ask))
}

/// Purges old audit records now and then. Every node runs one; deleting twice is harmless.
pub fn spawn_housekeeping(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match audit::purge(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(records = n, "purged old AI audit records"),
                Err(error) => tracing::warn!(%error, "purging AI audit records"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
        }
    })
}
