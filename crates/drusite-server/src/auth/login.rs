// SPDX-License-Identifier: AGPL-3.0-or-later

//! What every login method does when it succeeds: sign the browser in and say where to go next.

use axum::Json;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use drusite_model::UserId;
use serde::Serialize;
use uuid::Uuid;

use super::error::AuthResult;
use super::oauth::CONTINUE_PATH;
use super::session::{self, Method};
use crate::app::AppState;

/// Where the sign-in page goes after signing in: back to the app that asked, or to the
/// account page.
pub(super) fn next_url(request: Option<Uuid>) -> String {
    request.map_or_else(
        || "/account".to_owned(),
        |id| format!("{CONTINUE_PATH}?request={id}"),
    )
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum Step {
    SignedIn {
        next: String,
    },
    /// The password was right; now a TOTP code.
    TotpRequired {
        challenge: Uuid,
    },
    /// The password was right, but the server requires TOTP and this account has none yet.
    #[serde(rename_all = "camelCase")]
    TotpSetupRequired {
        challenge: Uuid,
        otpauth_uri: String,
        secret: String,
    },
}

/// Signs the browser in as `user` and answers with where to go next.
pub(super) async fn complete(
    state: &AppState,
    user: UserId,
    method: Method,
    request: Option<Uuid>,
) -> AuthResult<Response> {
    let cookie = session::start(state, user, method).await?;
    tracing::info!(%user, method = method.as_str(), "signed in");
    Ok((
        [
            (header::SET_COOKIE, cookie),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        Json(Step::SignedIn {
            next: next_url(request),
        }),
    )
        .into_response())
}
