// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signed-in browsers, and the short-lived state a sign-in carries between requests.
//!
//! The sign-in pages run in the browser the app opened (or any browser, for account settings).
//! Signing in there gives that browser a session cookie: HttpOnly, SameSite=Lax (the OAuth and
//! OIDC redirects arrive as top-level navigations from other sites), and `__Host-` with Secure
//! when the server is on https. The apps never see it; they get OAuth tokens instead.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use drusite_model::UserId;
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::error::{AuthError, AuthResult};
use super::secret::{self, Kind};
use crate::app::AppState;

/// How a browser or a device signed in; kept with sessions and grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Password,
    PasswordTotp,
    Passkey,
    MagicLink,
    Oidc,
    Recovery,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::PasswordTotp => "password_totp",
            Self::Passkey => "passkey",
            Self::MagicLink => "magic_link",
            Self::Oidc => "oidc",
            Self::Recovery => "recovery",
        }
    }
}

fn secure(state: &AppState) -> bool {
    state.config().public_url.scheme() == "https"
}

pub fn session_cookie_name(state: &AppState) -> &'static str {
    if secure(state) {
        "__Host-drusite_session"
    } else {
        "drusite_session"
    }
}

/// A `Set-Cookie` value. `max_age` of zero deletes the cookie.
pub fn cookie(state: &AppState, name: &str, value: &str, path: &str, max_age: i64) -> HeaderValue {
    let secure = if secure(state) { "; Secure" } else { "" };
    let text =
        format!("{name}={value}; Path={path}; Max-Age={max_age}; HttpOnly; SameSite=Lax{secure}");
    HeaderValue::from_str(&text).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// A cookie's value from the request, if it's there.
pub fn read_cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(k, v)| (k == name).then_some(v))
}

/// A signed-in browser.
#[derive(Debug, Clone, Copy)]
pub struct BrowserSession {
    pub user: UserId,
    pub method: Method,
}

/// Signs the browser in: stores a new session and returns its `Set-Cookie`.
pub async fn start(state: &AppState, user: UserId, method: Method) -> AuthResult<HeaderValue> {
    let token = secret::generate(Kind::Session)?;
    let hours = i64::from(state.config().auth.session_hours);
    let hash = secret::hash(Kind::Session, &token)
        .ok_or_else(|| AuthError::internal(anyhow::anyhow!("a new session token didn't hash")))?;
    sqlx::query(
        "INSERT INTO browser_sessions (token_hash, user_id, method, expires_at)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(hash.as_slice())
    .bind(user.as_uuid())
    .bind(method.as_str())
    .bind(OffsetDateTime::now_utc() + Duration::hours(hours))
    .execute(state.db().pool())
    .await?;
    Ok(cookie(
        state,
        session_cookie_name(state),
        &token,
        "/",
        hours * 3600,
    ))
}

/// The browser's session, if it has a live one for an account that isn't disabled.
pub async fn current(state: &AppState, headers: &HeaderMap) -> AuthResult<Option<BrowserSession>> {
    let Some(hash) = read_cookie(headers, session_cookie_name(state))
        .and_then(|t| secret::hash(Kind::Session, t))
    else {
        return Ok(None);
    };
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT s.user_id, s.method FROM browser_sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = $1 AND s.expires_at > now() AND u.disabled_at IS NULL",
    )
    .bind(hash.as_slice())
    .fetch_optional(state.db().pool())
    .await?;
    Ok(row.and_then(|(user, method)| {
        let method = serde_json::from_value(serde_json::Value::String(method)).ok()?;
        Some(BrowserSession {
            user: UserId::from_uuid(user),
            method,
        })
    }))
}

/// The browser's session, or 401.
pub async fn require(state: &AppState, headers: &HeaderMap) -> AuthResult<BrowserSession> {
    current(state, headers)
        .await?
        .ok_or(AuthError::Unauthorized("not_signed_in", "sign in first"))
}

/// Signs the browser out; returns the `Set-Cookie` that clears it.
pub async fn end(state: &AppState, headers: &HeaderMap) -> AuthResult<HeaderValue> {
    if let Some(hash) = read_cookie(headers, session_cookie_name(state))
        .and_then(|t| secret::hash(Kind::Session, t))
    {
        sqlx::query("DELETE FROM browser_sessions WHERE token_hash = $1")
            .bind(hash.as_slice())
            .execute(state.db().pool())
            .await?;
    }
    Ok(cookie(state, session_cookie_name(state), "", "/", 0))
}

/// Refuses state-changing requests from other sites. The session cookie is SameSite=Lax, so a
/// cross-site POST arrives without it anyway; this is the second line, and it also covers the
/// endpoints that work without a session (a forged sign-in would sign the victim in as the
/// attacker).
pub async fn same_origin_only(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    use axum::response::IntoResponse as _;
    if matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    ) {
        return next.run(request).await;
    }
    let headers = request.headers();
    let ours = state.config().public_url.origin().ascii_serialization();
    let origin_ok = match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(origin) => origin == ours,
        // No Origin header: not a browser's cross-site request, unless Fetch Metadata says so.
        None => !matches!(
            headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()),
            Some("cross-site" | "same-site")
        ),
    };
    if origin_ok {
        next.run(request).await
    } else {
        AuthError::Forbidden("cross_origin", "requests from other sites aren't allowed")
            .into_response()
    }
}

/// State a sign-in keeps between requests, stored so any server node can pick it up.
pub async fn put_challenge(
    state: &AppState,
    kind: &str,
    data: &impl Serialize,
    binding: Option<&str>,
    ttl: Duration,
) -> AuthResult<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO auth_challenges (id, kind, data, binding, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(kind)
    .bind(serde_json::to_value(data)?)
    .bind(binding.map(|b| blake3::hash(b.as_bytes()).as_bytes().to_vec()))
    .bind(OffsetDateTime::now_utc() + ttl)
    .execute(state.db().pool())
    .await?;
    Ok(id)
}

/// Takes a challenge, once: it's deleted as it's read. Fails if it's unknown, expired, of
/// another kind, or bound to another browser.
pub async fn take_challenge<T: DeserializeOwned>(
    state: &AppState,
    id: Uuid,
    kind: &str,
    binding: Option<&str>,
) -> AuthResult<T> {
    let row: Option<(serde_json::Value, Option<Vec<u8>>)> = sqlx::query_as(
        "DELETE FROM auth_challenges WHERE id = $1 AND kind = $2 AND expires_at > now()
         RETURNING data, binding",
    )
    .bind(id)
    .bind(kind)
    .fetch_optional(state.db().pool())
    .await?;
    let (data, stored) = row.ok_or_else(expired)?;
    if let Some(stored) = stored {
        let given = binding.map(|b| blake3::hash(b.as_bytes()));
        if given.is_none_or(|g| {
            !bool::from(subtle::ConstantTimeEq::ct_eq(
                g.as_bytes().as_slice(),
                stored.as_slice(),
            ))
        }) {
            return Err(AuthError::bad(
                "wrong_browser",
                "finish signing in in the browser you started in",
            ));
        }
    }
    Ok(serde_json::from_value(data)?)
}

/// Reads a challenge without using it up.
pub async fn peek_challenge<T: DeserializeOwned>(
    state: &AppState,
    id: Uuid,
    kind: &str,
) -> AuthResult<T> {
    let data: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT data FROM auth_challenges WHERE id = $1 AND kind = $2 AND expires_at > now()",
    )
    .bind(id)
    .bind(kind)
    .fetch_optional(state.db().pool())
    .await?;
    Ok(serde_json::from_value(data.ok_or_else(expired)?)?)
}

fn expired() -> AuthError {
    AuthError::bad("expired", "this sign-in has expired; start again")
}
