// SPDX-License-Identifier: AGPL-3.0-or-later

//! `Authenticated`: an app's request with a valid DPoP-bound access token, or the web app's
//! with its session cookie.
//!
//! Handlers that take it can only be reached with `Authorization: DPoP <token>` and a `DPoP`
//! proof for this very request from the key the token is bound to, or, from the web app, with
//! its cookie and this server's own origin (see `web`). Anything else is a 401 with a
//! `WWW-Authenticate: DPoP` challenge (RFC 9449 §7.1).

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use drusite_model::UserId;

use super::dpop::{self, ProofError};
use super::tokens::{self, GrantId};
use crate::app::AppState;

#[derive(Debug, Clone)]
pub struct Authenticated {
    pub user: UserId,
    pub grant: GrantId,
    pub scope: String,
}

impl Authenticated {
    pub fn can_write(&self) -> bool {
        self.scope.split(' ').any(|s| s == "write")
    }
}

#[derive(Debug)]
pub enum Rejection {
    /// No token, or one that's unknown, expired or revoked.
    InvalidToken(&'static str),
    InvalidProof(String),
    Server,
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        let (status, error, description) = match self {
            Self::InvalidToken(d) => (StatusCode::UNAUTHORIZED, "invalid_token", d.to_owned()),
            Self::InvalidProof(d) => (StatusCode::UNAUTHORIZED, "invalid_dpop_proof", d),
            Self::Server => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "something went wrong on the server".to_owned(),
            ),
        };
        let mut response = (
            status,
            axum::Json(serde_json::json!({ "error": error, "error_description": description })),
        )
            .into_response();
        if status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_str(&format!(r#"DPoP error="{error}", algs="ES256""#))
                    .unwrap_or_else(|_| HeaderValue::from_static("DPoP")),
            );
        }
        response
    }
}

impl FromRequestParts<AppState> for Authenticated {
    type Rejection = Rejection;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Rejection> {
        if !parts.headers.contains_key(header::AUTHORIZATION)
            && state.config().web.served().is_some()
            && super::session::read_cookie(&parts.headers, super::web::cookie_name(state)).is_some()
        {
            return from_web_cookie(parts, state).await;
        }
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("DPoP "))
            .map(str::trim)
            .ok_or(Rejection::InvalidToken(
                "send the access token as Authorization: DPoP",
            ))?;
        let grant = tokens::lookup_access(state, token)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "looking up an access token");
                Rejection::Server
            })?
            .ok_or(Rejection::InvalidToken("the access token isn't valid"))?;
        let proof = dpop::check(
            state,
            &parts.headers,
            parts.method.as_str(),
            parts.uri.path(),
            Some(token),
        )
        .await
        .map_err(|e| match e {
            ProofError::Db(e) => {
                tracing::error!(error = %e, "recording a DPoP proof");
                Rejection::Server
            }
            other => Rejection::InvalidProof(other.to_string()),
        })?;
        // An agent's token is for the MCP endpoint only (RFC 8707), and never DPoP-free here.
        let Some(bound) = grant
            .dpop_jkt
            .as_deref()
            .filter(|_| grant.resource.is_none())
        else {
            return Err(Rejection::InvalidToken(
                "the access token isn't for this endpoint",
            ));
        };
        if !bool::from(subtle::ConstantTimeEq::ct_eq(
            proof.jkt.as_bytes(),
            bound.as_bytes(),
        )) {
            return Err(Rejection::InvalidProof(ProofError::WrongKey.to_string()));
        }
        Ok(Self {
            user: grant.user,
            grant: grant.grant,
            scope: grant.scope,
        })
    }
}

/// The web app's request: its cookie's grant, from this server's own origin only.
async fn from_web_cookie(parts: &Parts, state: &AppState) -> Result<Authenticated, Rejection> {
    if !super::web::same_origin(state, &parts.headers) {
        return Err(Rejection::InvalidToken(
            "the session cookie is only good from this server's own pages",
        ));
    }
    match super::web::lookup(state, &parts.headers).await {
        Ok(super::web::Lookup::Live { user, grant, scope }) => {
            Ok(Authenticated { user, grant, scope })
        }
        Ok(_) => Err(Rejection::InvalidToken("this browser isn't signed in")),
        Err(e) => {
            tracing::error!(error = %e, "looking up a web session");
            Err(Rejection::Server)
        }
    }
}
