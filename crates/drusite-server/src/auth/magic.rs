// SPDX-License-Identifier: AGPL-3.0-or-later

//! Magic links, and the one-time links from `drusite-server account recover`.
//!
//! Both carry their token in the URL's fragment, which browsers never send to a server, so it
//! stays out of logs and Referer headers. The page the link opens shows which account it signs
//! in to and asks before using it. A mail scanner that follows the link uses nothing, and
//! someone who sends you a link to their own account can't quietly sign you in as them.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use axum::{Json, Router, routing::post};
use drusite_model::UserId;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::error::{AuthError, AuthResult};
use super::login;
use super::secret::{self, Kind};
use super::session::Method;
use super::users::{self, limits};
use crate::account::hash_token;
use crate::app::AppState;

/// Links sent per email address in `WINDOW_MINUTES`.
const MAX_SENDS: i64 = 3;
const WINDOW_MINUTES: i32 = 15;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/start", post(start))
        .route("/peek", post(peek))
        .route("/finish", post(finish))
}

/// Where a magic link points.
pub(super) fn link(state: &AppState, token: &str) -> url::Url {
    let mut url = state.config().public_url.clone();
    url.set_path("/auth/magic");
    url.set_fragment(Some(&format!("token={token}")));
    url
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StartBody {
    email: String,
    request: Option<Uuid>,
}

/// Sends a link if the email has an account, or could make one. Answers the same either way,
/// and as quickly, so it can't be used to find out who has an account.
async fn start(State(state): State<AppState>, Json(body): Json<StartBody>) -> AuthResult<Response> {
    let email = users::normalize_email(&body.email)?;
    let key = format!("magic:{}", email.to_lowercase());
    limits::check(&state, &key, MAX_SENDS, WINDOW_MINUTES).await?;
    limits::record(&state, &key).await?;

    let account = users::by_email(&state, &email).await?;
    let user = match &account {
        Some(a) if a.disabled => None,
        Some(a) => Some(Some(a.id)),
        None if state.config().auth.self_signup => Some(None),
        None => None,
    };
    if let Some(user) = user {
        let token = secret::generate(Kind::MagicLink)?;
        let hash = secret::hash(Kind::MagicLink, &token)
            .ok_or_else(|| AuthError::internal(anyhow::anyhow!("a new token didn't hash")))?;
        let ttl = Duration::minutes(i64::from(state.config().auth.magic_link.ttl_minutes));
        sqlx::query(
            "INSERT INTO login_tokens (token_hash, email, user_id, auth_request, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(hash.as_slice())
        .bind(
            account
                .as_ref()
                .map_or(email.as_str(), |a| a.email.as_str()),
        )
        .bind(user.map(|u| *u.as_uuid()))
        .bind(body.request)
        .bind(OffsetDateTime::now_utc() + ttl)
        .execute(state.db().pool())
        .await?;
        let url = link(&state, &token);
        let host = state
            .config()
            .public_url
            .host_str()
            .unwrap_or("Drusite")
            .to_owned();
        let minutes = ttl.whole_minutes();
        let text = format!(
            "Open this link to sign in to Drusite at {host}:\n\n{url}\n\n\
             It works once, for {minutes} minutes. If you didn't ask to sign in, ignore this \
             email; nobody can use the link without access to your inbox.\n"
        );
        // Sent in the background, so the answer takes as long whether or not there's an account.
        let mailer = state.mailer().clone();
        tokio::spawn(async move {
            if let Err(error) = mailer.send(&email, "Your Drusite sign-in link", text).await {
                tracing::error!(error = format!("{error:#}"), "sending a magic link");
            }
        });
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "status": "sent" })),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
pub(super) struct TokenBody {
    token: String,
    #[serde(default)]
    name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Peeked {
    email: String,
    new_account: bool,
}

/// Which account a link signs in to, without using it.
async fn peek(
    State(state): State<AppState>,
    Json(body): Json<TokenBody>,
) -> AuthResult<Json<Peeked>> {
    let hash = secret::hash(Kind::MagicLink, &body.token).ok_or_else(used_up)?;
    let row: Option<(String, Option<Uuid>)> = sqlx::query_as(
        "SELECT email, user_id FROM login_tokens
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()",
    )
    .bind(hash.as_slice())
    .fetch_optional(state.db().pool())
    .await?;
    let (email, user) = row.ok_or_else(used_up)?;
    Ok(Json(Peeked {
        email,
        new_account: user.is_none(),
    }))
}

fn used_up() -> AuthError {
    AuthError::bad(
        "link_used",
        "this link has expired or was already used; ask for a new one",
    )
}

async fn finish(
    State(state): State<AppState>,
    Json(body): Json<TokenBody>,
) -> AuthResult<Response> {
    let hash = secret::hash(Kind::MagicLink, &body.token).ok_or_else(used_up)?;
    let row: Option<(String, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "UPDATE login_tokens SET used_at = now()
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()
         RETURNING email, user_id, auth_request",
    )
    .bind(hash.as_slice())
    .fetch_optional(state.db().pool())
    .await?;
    let (email, user, request) = row.ok_or_else(used_up)?;
    let user = match user {
        Some(id) => id,
        None => {
            // Someone else may have made the account since the link was sent; then it's theirs
            // to sign in to, and this link proves the email.
            match users::create(&state, &email, &body.name, true).await {
                Ok(account) => *account.id.as_uuid(),
                Err(AuthError::BadRequest("account_exists", _)) => users::by_email(&state, &email)
                    .await?
                    .ok_or_else(used_up)?
                    .id
                    .as_uuid()
                    .to_owned(),
                Err(e) => return Err(e),
            }
        }
    };
    let user = UserId::from_uuid(user);
    let account = users::by_id(&state, user).await?.ok_or_else(used_up)?;
    if account.disabled {
        return Err(AuthError::Forbidden("disabled", "this account is disabled"));
    }
    // Opening the link proves the inbox is theirs.
    users::mark_email_verified(&state, user).await?;
    login::complete(&state, user, Method::MagicLink, request).await
}

#[derive(Debug, Deserialize)]
pub(super) struct RecoverBody {
    token: String,
}

/// `account recover` links. Always on, whatever login methods are: they're how someone locked
/// out gets back in to set one up.
pub(super) async fn recover(
    State(state): State<AppState>,
    Json(body): Json<RecoverBody>,
) -> AuthResult<Response> {
    let row: Option<Uuid> = sqlx::query_scalar(
        "UPDATE recovery_tokens SET used_at = now()
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()
         RETURNING user_id",
    )
    .bind(hash_token(body.token.trim()).as_slice())
    .fetch_optional(state.db().pool())
    .await?;
    let user = UserId::from_uuid(row.ok_or_else(used_up)?);
    let account = users::by_id(&state, user).await?.ok_or_else(used_up)?;
    if account.disabled {
        return Err(AuthError::Forbidden("disabled", "this account is disabled"));
    }
    login::complete(&state, user, Method::Recovery, None).await
}

/// Which account a recovery link is for, without using it.
pub(super) async fn recover_peek(
    State(state): State<AppState>,
    Json(body): Json<RecoverBody>,
) -> AuthResult<Json<serde_json::Value>> {
    let email: Option<String> = sqlx::query_scalar(
        "SELECT u.email FROM recovery_tokens t JOIN users u ON u.id = t.user_id
         WHERE t.token_hash = $1 AND t.used_at IS NULL AND t.expires_at > now()",
    )
    .bind(hash_token(body.token.trim()).as_slice())
    .fetch_optional(state.db().pool())
    .await?;
    Ok(Json(
        serde_json::json!({ "email": email.ok_or_else(used_up)? }),
    ))
}
