// SPDX-License-Identifier: AGPL-3.0-or-later

//! The record of what was sent to which AI provider (`ai_audit`), and the per-person limits it
//! counts. It names blocks by ID and counts characters and tokens; it never holds text.

use axum::Json;
use axum::extract::{Path, Query, State};
use drusite_ai::{AiError, Kind, Provider};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use time::OffsetDateTime;
use uuid::Uuid;

use super::AskError;
use crate::app::AppState;
use crate::auth::Authenticated;

/// One call about to be made.
#[derive(Debug)]
pub struct Call<'a> {
    pub workspace: Option<Uuid>,
    pub user: Option<Uuid>,
    pub feature: &'static str,
    pub provider: &'a Provider,
    pub model: &'a str,
    /// `[[page, block], …]`, or the page indexed.
    pub blocks: serde_json::Value,
    pub items: usize,
    pub chars: usize,
}

/// The name a provider is recorded by.
pub fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Anthropic => "anthropic",
        Kind::OpenAi => "openai",
        Kind::Google => "google",
        Kind::Mistral => "mistral",
        Kind::OpenRouter => "openrouter",
        Kind::OpenAiCompatible => "openai-compatible",
    }
}

/// What went wrong, as a category: provider messages can quote the request, so they aren't
/// stored.
pub fn outcome_of(e: &AiError) -> &'static str {
    match e {
        AiError::Network(_) => "network",
        AiError::Unauthorized(_) => "unauthorized",
        AiError::NotFound(_) => "not_found",
        AiError::RateLimited(_) => "rate_limited",
        AiError::Refused { .. } => "refused",
        AiError::Malformed(_) => "malformed",
        AiError::Unsupported(_) => "unsupported",
        AiError::Invalid(_) => "invalid",
    }
}

fn clamp(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// Records a call before it's made, as `pending`; returns its ID for [`finish`].
pub async fn begin(conn: &mut PgConnection, call: &Call<'_>) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO ai_audit (id, workspace_id, user_id, feature, provider, host, model, blocks,
             items, chars)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(call.workspace)
    .bind(call.user)
    .bind(call.feature)
    .bind(kind_name(call.provider.kind()))
    .bind(call.provider.base().host_str().unwrap_or(""))
    .bind(call.model)
    .bind(&call.blocks)
    .bind(clamp(call.items))
    .bind(clamp(call.chars))
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

/// Records how a call went.
pub async fn finish(
    state: &AppState,
    id: Uuid,
    outcome: &str,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
) {
    let result = async {
        let mut tx = state.db().as_system().await?;
        sqlx::query(
            "UPDATE ai_audit SET outcome = $2, input_tokens = $3, output_tokens = $4 WHERE id = $1",
        )
        .bind(id)
        .bind(outcome)
        .bind(input_tokens.map(|t| i32::try_from(t).unwrap_or(i32::MAX)))
        .bind(output_tokens.map(|t| i32::try_from(t).unwrap_or(i32::MAX)))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "recording an AI call");
    }
}

/// Records a whole call at once, as the system (the indexer).
pub async fn record(state: &AppState, call: &Call<'_>, outcome: &str) {
    let result = async {
        let mut tx = state.db().as_system().await?;
        let id = begin(&mut tx, call).await?;
        sqlx::query("UPDATE ai_audit SET outcome = $2 WHERE id = $1")
            .bind(id)
            .bind(outcome)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "recording an AI call");
    }
}

/// Refuses a question past the person's limits, or while two of theirs are still running.
pub async fn check_limits(
    state: &AppState,
    conn: &mut PgConnection,
    user: Uuid,
) -> Result<(), AskError> {
    let config = &state.config().ask;
    let (minute, day, running): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE at > now() - interval '1 minute'),
                count(*),
                count(*) FILTER (WHERE outcome = 'pending' AND at > now() - make_interval(secs => $2))
         FROM ai_audit WHERE user_id = $1 AND feature = 'ask' AND at > now() - interval '1 day'",
    )
    .bind(user)
    .bind(config.timeout_secs as f64)
    .fetch_one(&mut *conn)
    .await?;
    if minute >= i64::from(config.per_minute) {
        return Err(AskError::Limited(
            "That's a lot of questions in a minute. Try again shortly.".to_owned(),
        ));
    }
    if day >= i64::from(config.per_day) {
        return Err(AskError::Limited(
            "You've reached today's limit of questions on this server.".to_owned(),
        ));
    }
    if running >= 2 {
        return Err(AskError::Limited(
            "Two answers are still being written. Wait for one to finish.".to_owned(),
        ));
    }
    Ok(())
}

/// Forgets records older than `[ask] audit_days`. Run now and then.
pub async fn purge(state: &AppState) -> anyhow::Result<u64> {
    let mut tx = state.db().as_system().await?;
    let n = sqlx::query("DELETE FROM ai_audit WHERE at < now() - make_interval(days => $1)")
        .bind(i32::try_from(state.config().ask.audit_days).unwrap_or(i32::MAX))
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(n)
}

#[derive(Deserialize)]
pub(super) struct ListQuery {
    #[serde(default = "default_list")]
    limit: i64,
}

const fn default_list() -> i64 {
    100
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Item {
    id: Uuid,
    feature: String,
    provider: String,
    host: String,
    model: String,
    /// Mine, or another member's (an owner sees everyone's), or the indexer's (none).
    mine: Option<bool>,
    blocks: serde_json::Value,
    items: i32,
    chars: i32,
    input_tokens: Option<i32>,
    output_tokens: Option<i32>,
    outcome: String,
    /// Unix milliseconds.
    at: i64,
}

type Row = (
    Uuid,
    String,
    String,
    String,
    String,
    Option<Uuid>,
    serde_json::Value,
    i32,
    i32,
    Option<i32>,
    Option<i32>,
    String,
    OffsetDateTime,
);

/// `GET /api/v1/workspaces/{ws}/ai-audit`: what was sent to which provider, newest first.
pub(super) async fn list(
    State(state): State<AppState>,
    auth: Authenticated,
    Path(ws): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Item>>, AskError> {
    let mut tx = state.db().as_user(auth.user).await?;
    super::require_reader(&state, &mut tx, auth.user, ws).await?;
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, feature, provider, host, model, user_id, blocks, items, chars, input_tokens,
                output_tokens, outcome, at
         FROM ai_audit WHERE workspace_id = $1 ORDER BY at DESC LIMIT $2",
    )
    .bind(ws)
    .bind(q.limit.clamp(1, 500))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let me = *auth.user.as_uuid();
    Ok(Json(
        rows.into_iter()
            .map(
                |(
                    id,
                    feature,
                    provider,
                    host,
                    model,
                    user,
                    blocks,
                    items,
                    chars,
                    i,
                    o,
                    outcome,
                    at,
                )| {
                    Item {
                        id,
                        feature,
                        provider,
                        host,
                        model,
                        mine: user.map(|u| u == me),
                        blocks,
                        items,
                        chars,
                        input_tokens: i,
                        output_tokens: o,
                        outcome,
                        at: i64::try_from(at.unix_timestamp_nanos() / 1_000_000).unwrap_or(0),
                    }
                },
            )
            .collect(),
    ))
}
