// SPDX-License-Identifier: AGPL-3.0-or-later

//! The signed-in account's settings, for the account page (browser session) and `/api/v1/me`
//! for the apps (DPoP access token).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use axum::{
    Json, Router,
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::error::{AuthError, AuthResult};
use super::extract::Authenticated;
use super::session;
use super::tokens::{self, GrantId};
use super::users;
use super::{password, totp};
use crate::app::AppState;
use crate::crypto::random_bytes;

const TOTP_ENROL_KIND: &str = "totp_enrol";

pub(super) fn routes(state: &AppState) -> Router<AppState> {
    let mut router = Router::new()
        .route("/", get(me))
        .route("/logout", post(logout))
        .route("/devices", get(devices))
        .route("/devices/{id}", delete(sign_out_device));
    let auth = &state.config().auth;
    if auth.password.enabled {
        router = router
            .route("/password", post(set_password))
            .route("/totp/start", post(totp_start))
            .route("/totp/confirm", post(totp_confirm));
    }
    if auth.passkeys.enabled {
        router = router.nest("/passkeys", super::passkeys::account_routes());
    }
    router
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Me {
    id: String,
    email: String,
    email_verified: bool,
    display_name: String,
    has_password: bool,
    has_totp: bool,
}

async fn describe(state: &AppState, user: drusite_model::UserId) -> AuthResult<Me> {
    let account = users::by_id(state, user)
        .await?
        .ok_or(AuthError::NotFound)?;
    let (has_password, has_totp): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM password_credentials WHERE user_id = $1),
                EXISTS (SELECT 1 FROM totp_credentials WHERE user_id = $1
                        AND confirmed_at IS NOT NULL)",
    )
    .bind(user.as_uuid())
    .fetch_one(state.db().pool())
    .await?;
    Ok(Me {
        id: account.id.to_string(),
        email: account.email,
        email_verified: account.email_verified,
        display_name: account.display_name,
        has_password,
        has_totp,
    })
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> AuthResult<Json<Me>> {
    let browser = session::require(&state, &headers).await?;
    Ok(Json(describe(&state, browser.user).await?))
}

/// `GET /api/v1/me`, for the apps.
pub(super) async fn app_me(
    State(state): State<AppState>,
    auth: Authenticated,
) -> AuthResult<Json<Me>> {
    Ok(Json(describe(&state, auth.user).await?))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> AuthResult<Response> {
    let clear = session::end(&state, &headers).await?;
    Ok((
        [(header::SET_COOKIE, clear)],
        Json(serde_json::json!({ "status": "signed_out" })),
    )
        .into_response())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    id: Uuid,
    name: String,
    signed_in_with: String,
    created_at: String,
    last_used_at: String,
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

async fn devices(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AuthResult<Json<Vec<Device>>> {
    let browser = session::require(&state, &headers).await?;
    let rows: Vec<(Uuid, String, String, OffsetDateTime, OffsetDateTime)> = sqlx::query_as(
        "SELECT id, device_name, auth_method, created_at, last_used_at FROM oauth_grants
         WHERE user_id = $1 AND revoked_at IS NULL
           AND idle_expires_at > now() AND absolute_expires_at > now()
         ORDER BY last_used_at DESC",
    )
    .bind(browser.user.as_uuid())
    .fetch_all(state.db().pool())
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, method, created, used)| Device {
                id,
                name,
                signed_in_with: method,
                created_at: rfc3339(created),
                last_used_at: rfc3339(used),
            })
            .collect(),
    ))
}

async fn sign_out_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> AuthResult<Json<serde_json::Value>> {
    let browser = session::require(&state, &headers).await?;
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT user_id FROM oauth_grants WHERE id = $1")
        .bind(id)
        .fetch_optional(state.db().pool())
        .await?;
    if owner != Some(*browser.user.as_uuid()) {
        return Err(AuthError::NotFound);
    }
    tokens::revoke_grant(&state, GrantId(id), "signed out from the account page").await?;
    Ok(Json(serde_json::json!({ "status": "signed_out" })))
}

#[derive(Debug, Deserialize)]
struct PasswordBody {
    current: Option<String>,
    password: String,
}

async fn set_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PasswordBody>,
) -> AuthResult<Json<serde_json::Value>> {
    let browser = session::require(&state, &headers).await?;
    let has: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM password_credentials WHERE user_id = $1)")
            .bind(browser.user.as_uuid())
            .fetch_one(state.db().pool())
            .await?;
    // Changing a password takes the old one; setting a first one takes only the session.
    if has
        && !password::matches(
            &state,
            browser.user,
            body.current.as_deref().unwrap_or_default(),
        )
        .await?
    {
        return Err(AuthError::Unauthorized(
            "invalid_credentials",
            "the current password isn't right",
        ));
    }
    password::set(&state, browser.user, &body.password).await?;
    Ok(Json(serde_json::json!({ "status": "saved" })))
}

#[derive(Debug, Serialize, Deserialize)]
struct Enrolment {
    user: drusite_model::UserId,
    secret: Vec<u8>,
}

async fn totp_start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AuthResult<Json<serde_json::Value>> {
    let browser = session::require(&state, &headers).await?;
    let account = users::by_id(&state, browser.user)
        .await?
        .ok_or(AuthError::NotFound)?;
    let secret = random_bytes::<20>()?.to_vec();
    let uri = totp::otpauth_uri(&password::issuer_name(&state), &account.email, &secret);
    let text = totp::base32(&secret);
    let challenge = session::put_challenge(
        &state,
        TOTP_ENROL_KIND,
        &Enrolment {
            user: browser.user,
            secret,
        },
        None,
        Duration::minutes(15),
    )
    .await?;
    Ok(Json(serde_json::json!({
        "challenge": challenge,
        "otpauthUri": uri,
        "secret": text,
    })))
}

#[derive(Debug, Deserialize)]
struct ConfirmBody {
    challenge: Uuid,
    code: String,
}

async fn totp_confirm(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ConfirmBody>,
) -> AuthResult<Json<serde_json::Value>> {
    let browser = session::require(&state, &headers).await?;
    let pending: Enrolment =
        session::peek_challenge(&state, body.challenge, TOTP_ENROL_KIND).await?;
    if pending.user != browser.user {
        return Err(AuthError::NotFound);
    }
    let now = u64::try_from(OffsetDateTime::now_utc().unix_timestamp()).unwrap_or_default();
    let Some(step) = totp::verify(&pending.secret, &body.code, now, 0) else {
        return Err(AuthError::Unauthorized(
            "invalid_code",
            "that code isn't right",
        ));
    };
    let pending: Enrolment =
        session::take_challenge(&state, body.challenge, TOTP_ENROL_KIND, None).await?;
    password::save_totp(&state, pending.user, &pending.secret, step).await?;
    // The page treats this like a sign-in step: back to the account page.
    Ok(Json(
        serde_json::json!({ "status": "signed_in", "next": "/account" }),
    ))
}
