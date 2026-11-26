// SPDX-License-Identifier: AGPL-3.0-or-later

//! Passkeys (WebAuthn), with webauthn-rs.
//!
//! Sign-in uses discoverable credentials: no email first, the browser offers the passkeys it
//! has for this server. Every passkey is therefore registered as a resident key. The relying
//! party is the public URL's host, so passkeys made on one server name don't work on another.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::{
    Json, Router,
    routing::{delete, get, post},
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use drusite_model::UserId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::Duration;
use uuid::Uuid;
use webauthn_rs::prelude::{
    DiscoverableAuthentication, Passkey, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential,
};
use webauthn_rs::{Webauthn, WebauthnBuilder};

use super::error::{AuthError, AuthResult};
use super::login;
use super::session::{self, Method};
use super::users;
use crate::app::AppState;

const LOGIN_KIND: &str = "passkey_login";
const REGISTER_KIND: &str = "passkey_register";
const TTL: Duration = Duration::minutes(5);

/// Sign-in and sign-up, under `/api/v1/auth/passkeys`.
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/login/start", post(login_start))
        .route("/login/finish", post(login_finish))
        .route("/signup/start", post(signup_start))
        .route("/signup/finish", post(register_finish))
}

/// The signed-in account's passkeys, under `/api/v1/account/passkeys`.
pub(super) fn account_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list))
        .route("/register/start", post(register_start))
        .route("/register/finish", post(register_finish))
        .route("/{id}", delete(remove))
}

fn webauthn(state: &AppState) -> AuthResult<Webauthn> {
    let origin = &state.config().public_url;
    let rp_id = origin
        .host_str()
        .ok_or_else(|| AuthError::internal(anyhow::anyhow!("public_url has no host")))?;
    WebauthnBuilder::new(rp_id, origin)
        .and_then(|b| b.rp_name("Drusite").build())
        .map_err(|e| AuthError::internal(anyhow::anyhow!("WebAuthn setup: {e}")))
}

fn webauthn_failed(e: impl std::fmt::Display) -> AuthError {
    tracing::info!(error = %e, "a passkey didn't verify");
    AuthError::Unauthorized("passkey_failed", "that passkey couldn't be verified")
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct LoginStartBody {
    /// Offer passkeys in the browser's autofill rather than in a dialog.
    #[serde(default)]
    conditional: bool,
}

#[derive(Debug, Serialize)]
struct Challenge {
    challenge: Uuid,
    options: Value,
}

async fn login_start(
    State(state): State<AppState>,
    body: Option<Json<LoginStartBody>>,
) -> AuthResult<Json<Challenge>> {
    let (options, auth) = webauthn(&state)?
        .start_discoverable_authentication()
        .map_err(AuthError::internal)?;
    let mut options = serde_json::to_value(options)?;
    if !body.is_some_and(|b| b.conditional) {
        // webauthn-rs asks for autofill (conditional mediation); a sign-in button wants the
        // ordinary dialog.
        if let Some(o) = options.as_object_mut() {
            o.remove("mediation");
        }
    }
    let challenge = session::put_challenge(&state, LOGIN_KIND, &auth, None, TTL).await?;
    Ok(Json(Challenge { challenge, options }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LoginFinishBody {
    challenge: Uuid,
    credential: PublicKeyCredential,
    request: Option<Uuid>,
}

async fn login_finish(
    State(state): State<AppState>,
    Json(body): Json<LoginFinishBody>,
) -> AuthResult<Response> {
    let auth: DiscoverableAuthentication =
        session::take_challenge(&state, body.challenge, LOGIN_KIND, None).await?;
    let webauthn = webauthn(&state)?;
    let (user_handle, credential_id) = webauthn
        .identify_discoverable_authentication(&body.credential)
        .map_err(webauthn_failed)?;
    let row: Option<(Uuid, Value)> =
        sqlx::query_as("SELECT user_id, passkey FROM passkeys WHERE id = $1")
            .bind(credential_id)
            .fetch_optional(state.db().pool())
            .await?;
    let Some((owner, stored)) = row else {
        return Err(webauthn_failed("unknown credential"));
    };
    if owner != user_handle {
        return Err(webauthn_failed(
            "the credential's user handle doesn't match",
        ));
    }
    let mut passkey: Passkey = serde_json::from_value(stored)?;
    let result = webauthn
        .finish_discoverable_authentication_v2(&body.credential, auth, &passkey)
        .map_err(webauthn_failed)?;
    passkey.update_credential(&result);
    sqlx::query("UPDATE passkeys SET passkey = $2, last_used_at = now() WHERE id = $1")
        .bind(credential_id)
        .bind(serde_json::to_value(&passkey)?)
        .execute(state.db().pool())
        .await?;
    let user = UserId::from_uuid(owner);
    if users::by_id(&state, user).await?.is_none_or(|a| a.disabled) {
        return Err(webauthn_failed("the account is disabled"));
    }
    login::complete(&state, user, Method::Passkey, body.request).await
}

/// A registration in progress: for a signed-in account, or for an account sign-up will make.
#[derive(Debug, Serialize, Deserialize)]
struct PendingRegistration {
    user: UserId,
    registration: PasskeyRegistration,
    /// Set for a sign-up: the account to make when the passkey checks out.
    signup: Option<Signup>,
    request: Option<Uuid>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Signup {
    email: String,
    name: String,
}

/// Makes the creation options ask for a discoverable credential, so the passkey can sign in
/// without an email first.
fn require_resident_key(options: impl Serialize) -> AuthResult<Value> {
    let mut options = serde_json::to_value(options)?;
    if let Some(selection) = options
        .pointer_mut("/publicKey/authenticatorSelection")
        .and_then(Value::as_object_mut)
    {
        selection.insert("residentKey".to_owned(), "required".into());
        selection.insert("requireResidentKey".to_owned(), true.into());
    }
    Ok(options)
}

async fn start_registration(
    state: &AppState,
    user: UserId,
    email: &str,
    name: &str,
    signup: Option<Signup>,
    request: Option<Uuid>,
) -> AuthResult<Json<Challenge>> {
    let existing: Vec<Vec<u8>> = sqlx::query_scalar("SELECT id FROM passkeys WHERE user_id = $1")
        .bind(user.as_uuid())
        .fetch_all(state.db().pool())
        .await?;
    let display = if name.is_empty() { email } else { name };
    let (options, registration) = webauthn(state)?
        .start_passkey_registration(*user.as_uuid(), email, display, Some(existing))
        .map_err(AuthError::internal)?;
    let options = require_resident_key(options)?;
    let challenge = session::put_challenge(
        state,
        REGISTER_KIND,
        &PendingRegistration {
            user,
            registration,
            signup,
            request,
        },
        None,
        TTL,
    )
    .await?;
    Ok(Json(Challenge { challenge, options }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SignupStartBody {
    email: String,
    #[serde(default)]
    name: String,
    request: Option<Uuid>,
}

async fn signup_start(
    State(state): State<AppState>,
    Json(body): Json<SignupStartBody>,
) -> AuthResult<Json<Challenge>> {
    if !state.config().auth.self_signup {
        return Err(AuthError::Forbidden(
            "signup_closed",
            "this server doesn't let anyone create an account",
        ));
    }
    let email = users::normalize_email(&body.email)?;
    if users::by_email(&state, &email).await?.is_some() {
        return Err(AuthError::bad(
            "account_exists",
            "an account with that email already exists; sign in instead",
        ));
    }
    let user = UserId::new();
    start_registration(
        &state,
        user,
        &email,
        body.name.trim(),
        Some(Signup {
            email: email.clone(),
            name: body.name.trim().to_owned(),
        }),
        body.request,
    )
    .await
}

async fn register_start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AuthResult<Json<Challenge>> {
    let browser = session::require(&state, &headers).await?;
    let account = users::by_id(&state, browser.user)
        .await?
        .ok_or(AuthError::NotFound)?;
    start_registration(
        &state,
        account.id,
        &account.email,
        &account.display_name,
        None,
        None,
    )
    .await
}

#[derive(Debug, Deserialize)]
pub(super) struct RegisterFinishBody {
    challenge: Uuid,
    credential: RegisterPublicKeyCredential,
    #[serde(default)]
    name: String,
}

async fn register_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegisterFinishBody>,
) -> AuthResult<Response> {
    let pending: PendingRegistration =
        session::take_challenge(&state, body.challenge, REGISTER_KIND, None).await?;
    // Adding to an existing account needs that account's browser session.
    if pending.signup.is_none() {
        let browser = session::require(&state, &headers).await?;
        if browser.user != pending.user {
            return Err(AuthError::Forbidden(
                "wrong_account",
                "this passkey was started for another account",
            ));
        }
    }
    let passkey = webauthn(&state)?
        .finish_passkey_registration(&body.credential, &pending.registration)
        .map_err(webauthn_failed)?;
    if let Some(signup) = &pending.signup {
        users::create_with_id(&state, pending.user, &signup.email, &signup.name, false).await?;
    }
    let name = if body.name.trim().is_empty() {
        "Passkey".to_owned()
    } else {
        body.name.trim().chars().take(100).collect()
    };
    sqlx::query("INSERT INTO passkeys (id, user_id, name, passkey) VALUES ($1, $2, $3, $4)")
        .bind(passkey.cred_id().as_ref() as &[u8])
        .bind(pending.user.as_uuid())
        .bind(name)
        .bind(serde_json::to_value(&passkey)?)
        .execute(state.db().pool())
        .await?;
    if pending.signup.is_some() {
        login::complete(&state, pending.user, Method::Passkey, pending.request).await
    } else {
        Ok(axum::response::IntoResponse::into_response(Json(
            serde_json::json!({ "status": "added" }),
        )))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PasskeyInfo {
    id: String,
    name: String,
    created_at: String,
    last_used_at: Option<String>,
}

async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AuthResult<Json<Vec<PasskeyInfo>>> {
    let browser = session::require(&state, &headers).await?;
    let rows: Vec<(
        Vec<u8>,
        String,
        time::OffsetDateTime,
        Option<time::OffsetDateTime>,
    )> = sqlx::query_as(
        "SELECT id, name, created_at, last_used_at FROM passkeys
             WHERE user_id = $1 ORDER BY created_at",
    )
    .bind(browser.user.as_uuid())
    .fetch_all(state.db().pool())
    .await?;
    let fmt = |t: time::OffsetDateTime| {
        t.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default()
    };
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, created, used)| PasskeyInfo {
                id: URL_SAFE_NO_PAD.encode(id),
                name,
                created_at: fmt(created),
                last_used_at: used.map(fmt),
            })
            .collect(),
    ))
}

async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AuthResult<Json<Value>> {
    let browser = session::require(&state, &headers).await?;
    let id = URL_SAFE_NO_PAD
        .decode(id)
        .map_err(|_| AuthError::NotFound)?;
    let deleted = sqlx::query("DELETE FROM passkeys WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(browser.user.as_uuid())
        .execute(state.db().pool())
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(AuthError::NotFound);
    }
    Ok(Json(serde_json::json!({ "status": "removed" })))
}
