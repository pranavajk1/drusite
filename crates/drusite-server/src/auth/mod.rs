// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identity: sign-in, accounts, and Drusite's own OAuth 2.1 server.
//!
//! Every login method is a config switch, and a method that's off has no routes at all: its
//! paths are 404s, not hidden buttons. The sign-in page asks `GET /api/v1/auth/methods` which
//! to show.
//!
//! - Browsers sign in on `/login` (password with TOTP, passkeys, magic links, OIDC) and get a
//!   session cookie for the sign-in and account pages.
//! - Apps sign in through that page with OAuth 2.1 and PKCE, and get opaque, short-lived access
//!   tokens and rotating refresh tokens, both bound to a device key with DPoP.

mod account;
pub mod clients;
pub mod dpop;
mod error;
pub mod extract;
mod login;
mod magic;
pub mod mail;
pub mod oauth;
mod oidc;
mod pages;
mod passkeys;
pub(crate) mod password;
mod secret;
pub mod session;
pub mod tokens;
pub mod totp;
mod users;
pub mod web;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

pub use error::{AuthError, AuthResult};
pub use extract::Authenticated;

use crate::app::AppState;
use crate::config::LoginMethod;

/// Where each method's endpoints live.
pub const PASSWORD_PATH: &str = "/api/v1/auth/password";
pub const PASSKEYS_PATH: &str = "/api/v1/auth/passkeys";
pub const MAGIC_LINK_PATH: &str = "/api/v1/auth/magic-link";
pub const OIDC_PATH: &str = "/api/v1/auth/oidc";

/// What identity keeps between requests besides the database.
#[derive(Debug)]
pub struct AuthState {
    pub mailer: mail::Mailer,
    providers: oidc::Providers,
    /// Agents' metadata documents, fetched lately.
    pub clients: clients::ClientCache,
}

impl AuthState {
    pub fn new(mailer: mail::Mailer) -> anyhow::Result<Self> {
        Ok(Self {
            mailer,
            providers: oidc::Providers::new()?,
            clients: clients::ClientCache::default(),
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum MethodInfo {
    Passkeys,
    #[serde(rename_all = "camelCase")]
    Oidc {
        id: String,
        display_name: String,
    },
    Password,
    MagicLink,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Methods {
    methods: Vec<MethodInfo>,
    self_signup: bool,
}

async fn methods(State(state): State<AppState>) -> Json<Methods> {
    let auth = &state.config().auth;
    Json(Methods {
        methods: auth
            .enabled_methods()
            .into_iter()
            .map(|m| match m {
                LoginMethod::Passkeys => MethodInfo::Passkeys,
                LoginMethod::Oidc { id, display_name } => MethodInfo::Oidc { id, display_name },
                LoginMethod::Password => MethodInfo::Password,
                LoginMethod::MagicLink => MethodInfo::MagicLink,
            })
            .collect(),
        self_signup: auth.self_signup,
    })
}

/// Which app is asking, for the sign-in page; 400 if the request has expired.
async fn request_info(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AuthResult<Json<serde_json::Value>> {
    let client = oauth::request_info(&state, id).await?;
    Ok(Json(serde_json::json!({ "client": client })))
}

/// The routes of the methods that are switched on, and nothing for the others.
pub fn routes(state: &AppState) -> Router<AppState> {
    let auth = &state.config().auth;
    let mut api = Router::new()
        .route("/api/v1/auth/methods", get(methods))
        .route("/api/v1/auth/request/{id}", get(request_info))
        .route("/api/v1/auth/recover", post(magic::recover))
        .route("/api/v1/auth/recover/peek", post(magic::recover_peek))
        .nest("/api/v1/account", account::routes(state));
    for method in auth.enabled_methods() {
        api = match method {
            LoginMethod::Password => api.nest(PASSWORD_PATH, password::routes()),
            LoginMethod::Passkeys => api.nest(PASSKEYS_PATH, passkeys::routes()),
            LoginMethod::MagicLink => api.nest(MAGIC_LINK_PATH, magic::routes()),
            // All providers share one set of routes, added once below.
            LoginMethod::Oidc { .. } => api,
        };
    }
    if auth.oidc.values().any(|p| p.enabled) {
        api = api.nest(OIDC_PATH, oidc::routes());
    }
    let api = api.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        session::same_origin_only,
    ));

    // The web app signs in here, when the server has one to serve.
    let web = if state.config().web.dir.is_some() {
        web::routes()
    } else {
        Router::new()
    };
    Router::new()
        .merge(api)
        .merge(web)
        .merge(pages::routes())
        .route("/api/v1/me", get(account::app_me))
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth::metadata),
        )
        .route(oauth::AUTHORIZE_PATH, get(oauth::authorize))
        .route(oauth::CONTINUE_PATH, get(oauth::resume))
        .route(oauth::TOKEN_PATH, post(oauth::token))
        .route(oauth::REVOKE_PATH, post(oauth::revoke))
        .route(oauth::CONSENT_PATH, get(oauth::consent_page))
        .merge(
            Router::new()
                .route(oauth::CONSENT_PATH, post(oauth::consent_answer))
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    session::same_origin_only,
                )),
        )
}

/// Deletes expired tokens, challenges and the like every few minutes.
pub fn spawn_sweeper(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            tick.tick().await;
            if let Err(error) = tokens::sweep(&state).await {
                tracing::warn!(%error, "sweeping expired sign-in data");
            }
        }
    })
}
