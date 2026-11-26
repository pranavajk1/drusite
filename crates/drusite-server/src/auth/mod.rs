// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sign-in. Every login method is a config switch, and a method that's off has no routes at
//! all: its paths are 404s, not hidden buttons. The apps ask `GET /api/v1/auth/methods` which
//! methods to show.
//!
//! Step 1 of Phase 2 wires the switches; each method's endpoints arrive with Identity (step 2)
//! and answer 501 until then.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::app::AppState;
use crate::config::{AuthConfig, LoginMethod};

/// Where each method's endpoints live.
pub const PASSWORD_PATH: &str = "/api/v1/auth/password";
pub const PASSKEYS_PATH: &str = "/api/v1/auth/passkeys";
pub const MAGIC_LINK_PATH: &str = "/api/v1/auth/magic-link";
pub const OIDC_PATH: &str = "/api/v1/auth/oidc";

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

async fn not_built_yet() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({ "error": "not_implemented" })),
    )
}

fn placeholder() -> Router<AppState> {
    Router::new().fallback(not_built_yet)
}

/// The routes of the methods that are switched on, and nothing for the others.
pub fn routes(config: &AuthConfig) -> Router<AppState> {
    let mut router = Router::new().route("/api/v1/auth/methods", get(methods));
    for method in config.enabled_methods() {
        router = match method {
            LoginMethod::Password => router.nest(PASSWORD_PATH, placeholder()),
            LoginMethod::Passkeys => router.nest(PASSKEYS_PATH, placeholder()),
            LoginMethod::MagicLink => router.nest(MAGIC_LINK_PATH, placeholder()),
            LoginMethod::Oidc { id, .. } => {
                router.nest(&format!("{OIDC_PATH}/{id}"), placeholder())
            }
        };
    }
    router
}
