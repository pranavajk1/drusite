// SPDX-License-Identifier: AGPL-3.0-or-later

//! Password login (argon2id), with TOTP as a second step where the account has it.
//!
//! Wrong email and wrong password get the same answer in about the same time, and failures
//! are limited per email address. With `require_totp`, a password account must set up TOTP
//! before its first sign-in completes.

use std::sync::LazyLock;

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher as _, PasswordVerifier as _};
use axum::extract::State;
use axum::response::{IntoResponse as _, Response};
use axum::{Json, Router, routing::post};
use drusite_model::UserId;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::error::{AuthError, AuthResult};
use super::login::{self, Step};
use super::session::{self, Method};
use super::totp;
use super::users::{self, limits};
use crate::app::AppState;
use crate::crypto::random_bytes;

const MIN_LENGTH: usize = 10;
const MAX_LENGTH: usize = 1024;
/// Failed attempts allowed per email address in `WINDOW_MINUTES`.
const MAX_FAILURES: i64 = 10;
const WINDOW_MINUTES: i32 = 15;
const TOTP_KIND: &str = "totp";
const TOTP_SETUP_KIND: &str = "totp_setup";
const TOTP_TTL: Duration = Duration::minutes(5);

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/totp", post(totp_step))
        .route("/totp-setup", post(totp_setup_step))
        .route("/signup", post(signup))
}

/// A hash of a password nobody has, so an unknown email costs as much as a known one.
static DUMMY_HASH: LazyLock<Option<String>> = LazyLock::new(|| {
    Argon2::default()
        .hash_password(b"drusite timing equaliser")
        .ok()
        .map(|h| h.to_string())
});

pub(super) fn check_strength(password: &str) -> AuthResult<()> {
    let len = password.chars().count();
    if len < MIN_LENGTH {
        return Err(AuthError::bad(
            "weak_password",
            format!("use at least {MIN_LENGTH} characters"),
        ));
    }
    if password.len() > MAX_LENGTH {
        return Err(AuthError::bad("weak_password", "that password is too long"));
    }
    Ok(())
}

/// Hashes off the async threads: argon2id is deliberately slow.
pub(super) async fn hash(password: &str) -> AuthResult<String> {
    let password = Zeroizing::new(password.to_owned());
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|h| h.to_string())
            .map_err(|e| AuthError::internal(anyhow::anyhow!("argon2: {e}")))
    })
    .await
    .map_err(AuthError::internal)?
}

/// A published page's password, hashed as account passwords are.
pub(crate) async fn hash_page_password(password: &str) -> AuthResult<String> {
    hash(password).await
}

/// Checks a reader's try at a published page's password.
pub(crate) async fn verify_page_password(password: &str, phc: String) -> bool {
    verify(password, Some(phc)).await.unwrap_or(false)
}

async fn verify(password: &str, phc: Option<String>) -> AuthResult<bool> {
    let password = Zeroizing::new(password.to_owned());
    let real = phc.is_some();
    let phc = phc.or_else(|| DUMMY_HASH.clone()).unwrap_or_default();
    tokio::task::spawn_blocking(move || {
        let ok = Argon2::default()
            .verify_password(password.as_bytes(), phc.as_str())
            .is_ok();
        Ok(ok && real)
    })
    .await
    .map_err(AuthError::internal)?
}

pub(super) async fn set(state: &AppState, user: UserId, password: &str) -> AuthResult<()> {
    check_strength(password)?;
    let phc = hash(password).await?;
    sqlx::query(
        "INSERT INTO password_credentials (user_id, phc) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET phc = excluded.phc, updated_at = now()",
    )
    .bind(user.as_uuid())
    .bind(phc)
    .execute(state.db().pool())
    .await?;
    Ok(())
}

/// Checks `password` against `user`'s, for changing it.
pub(super) async fn matches(state: &AppState, user: UserId, password: &str) -> AuthResult<bool> {
    let phc: Option<String> =
        sqlx::query_scalar("SELECT phc FROM password_credentials WHERE user_id = $1")
            .bind(user.as_uuid())
            .fetch_optional(state.db().pool())
            .await?;
    if phc.is_none() {
        return Ok(false);
    }
    verify(password, phc).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LoginBody {
    email: String,
    password: String,
    request: Option<Uuid>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PendingTotp {
    user: UserId,
    request: Option<Uuid>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PendingSetup {
    user: UserId,
    request: Option<Uuid>,
    secret: Vec<u8>,
}

fn limit_key(email: &str) -> String {
    format!("password:{}", email.trim().to_lowercase())
}

async fn login(State(state): State<AppState>, Json(body): Json<LoginBody>) -> AuthResult<Response> {
    let key = limit_key(&body.email);
    limits::check(&state, &key, MAX_FAILURES, WINDOW_MINUTES).await?;
    let account = users::by_email(&state, body.email.trim()).await?;
    let phc: Option<String> = match &account {
        Some(a) if !a.disabled => {
            sqlx::query_scalar("SELECT phc FROM password_credentials WHERE user_id = $1")
                .bind(a.id.as_uuid())
                .fetch_optional(state.db().pool())
                .await?
        }
        _ => None,
    };
    let (true, Some(account)) = (verify(&body.password, phc).await?, account) else {
        limits::record(&state, &key).await?;
        return Err(AuthError::Unauthorized(
            "invalid_credentials",
            "that email and password don't match an account",
        ));
    };
    limits::clear(&state, &key).await?;
    after_password(&state, account.id, &account.email, body.request).await
}

/// The password is right: finish, or ask for the second factor.
async fn after_password(
    state: &AppState,
    user: UserId,
    email: &str,
    request: Option<Uuid>,
) -> AuthResult<Response> {
    let confirmed: Option<bool> = sqlx::query_scalar(
        "SELECT confirmed_at IS NOT NULL FROM totp_credentials WHERE user_id = $1",
    )
    .bind(user.as_uuid())
    .fetch_optional(state.db().pool())
    .await?;
    if confirmed == Some(true) {
        let challenge = session::put_challenge(
            state,
            TOTP_KIND,
            &PendingTotp { user, request },
            None,
            TOTP_TTL,
        )
        .await?;
        return Ok(Json(Step::TotpRequired { challenge }).into_response());
    }
    if state.config().auth.password.require_totp {
        let secret = random_bytes::<20>()?.to_vec();
        let otpauth_uri = totp::otpauth_uri(&issuer_name(state), email, &secret);
        let text = totp::base32(&secret);
        let challenge = session::put_challenge(
            state,
            TOTP_SETUP_KIND,
            &PendingSetup {
                user,
                request,
                secret,
            },
            None,
            Duration::minutes(15),
        )
        .await?;
        return Ok(Json(Step::TotpSetupRequired {
            challenge,
            otpauth_uri,
            secret: text,
        })
        .into_response());
    }
    login::complete(state, user, Method::Password, request).await
}

/// The name authenticator apps show: the server's host.
pub(super) fn issuer_name(state: &AppState) -> String {
    format!(
        "Drusite ({})",
        state.config().public_url.host_str().unwrap_or("server")
    )
}

#[derive(Debug, Deserialize)]
pub(super) struct TotpBody {
    challenge: Uuid,
    code: String,
}

/// Checks a code against the user's confirmed secret and spends its time step.
pub(super) async fn check_code(state: &AppState, user: UserId, code: &str) -> AuthResult<bool> {
    let key = format!("totp:{user}");
    limits::check(state, &key, 5, WINDOW_MINUTES).await?;
    let mut tx = state.db().pool().begin().await?;
    let row: Option<(Vec<u8>, i64)> = sqlx::query_as(
        "SELECT secret, last_step FROM totp_credentials
         WHERE user_id = $1 AND confirmed_at IS NOT NULL FOR UPDATE",
    )
    .bind(user.as_uuid())
    .fetch_optional(&mut *tx)
    .await?;
    let Some((secret, last_step)) = row else {
        return Ok(false);
    };
    let now = u64::try_from(OffsetDateTime::now_utc().unix_timestamp()).unwrap_or_default();
    let last = u64::try_from(last_step).unwrap_or_default();
    let Some(step) = totp::verify(&secret, code, now, last) else {
        drop(tx);
        limits::record(state, &key).await?;
        return Ok(false);
    };
    sqlx::query("UPDATE totp_credentials SET last_step = $2 WHERE user_id = $1")
        .bind(user.as_uuid())
        .bind(i64::try_from(step).unwrap_or(i64::MAX))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    limits::clear(state, &key).await?;
    Ok(true)
}

async fn totp_step(
    State(state): State<AppState>,
    Json(body): Json<TotpBody>,
) -> AuthResult<Response> {
    // Peek, not take: a mistyped code can be tried again until the challenge expires.
    let pending: PendingTotp = session::peek_challenge(&state, body.challenge, TOTP_KIND).await?;
    if !check_code(&state, pending.user, &body.code).await? {
        return Err(AuthError::Unauthorized(
            "invalid_code",
            "that code isn't right",
        ));
    }
    let pending: PendingTotp =
        session::take_challenge(&state, body.challenge, TOTP_KIND, None).await?;
    login::complete(&state, pending.user, Method::PasswordTotp, pending.request).await
}

/// Saves a confirmed TOTP secret for `user`, replacing any earlier one.
pub(super) async fn save_totp(
    state: &AppState,
    user: UserId,
    secret: &[u8],
    step: u64,
) -> AuthResult<()> {
    sqlx::query(
        "INSERT INTO totp_credentials (user_id, secret, confirmed_at, last_step)
         VALUES ($1, $2, now(), $3)
         ON CONFLICT (user_id) DO UPDATE
             SET secret = excluded.secret, confirmed_at = now(), last_step = excluded.last_step",
    )
    .bind(user.as_uuid())
    .bind(secret)
    .bind(i64::try_from(step).unwrap_or(i64::MAX))
    .execute(state.db().pool())
    .await?;
    Ok(())
}

async fn totp_setup_step(
    State(state): State<AppState>,
    Json(body): Json<TotpBody>,
) -> AuthResult<Response> {
    let pending: PendingSetup =
        session::peek_challenge(&state, body.challenge, TOTP_SETUP_KIND).await?;
    let now = u64::try_from(OffsetDateTime::now_utc().unix_timestamp()).unwrap_or_default();
    let Some(step) = totp::verify(&pending.secret, &body.code, now, 0) else {
        return Err(AuthError::Unauthorized(
            "invalid_code",
            "that code isn't right",
        ));
    };
    let pending: PendingSetup =
        session::take_challenge(&state, body.challenge, TOTP_SETUP_KIND, None).await?;
    save_totp(&state, pending.user, &pending.secret, step).await?;
    login::complete(&state, pending.user, Method::PasswordTotp, pending.request).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SignupBody {
    email: String,
    password: String,
    #[serde(default)]
    name: String,
    request: Option<Uuid>,
}

async fn signup(
    State(state): State<AppState>,
    Json(body): Json<SignupBody>,
) -> AuthResult<Response> {
    if !state.config().auth.self_signup {
        return Err(AuthError::Forbidden(
            "signup_closed",
            "this server doesn't let anyone create an account",
        ));
    }
    let email = users::normalize_email(&body.email)?;
    check_strength(&body.password)?;
    // The email isn't verified: a magic link or a trusted OIDC provider can verify it later.
    let account = users::create(&state, &email, &body.name, false).await?;
    set(&state, account.id, &body.password).await?;
    after_password(&state, account.id, &account.email, body.request).await
}
