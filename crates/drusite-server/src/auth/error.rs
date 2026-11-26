// SPDX-License-Identifier: AGPL-3.0-or-later

//! Errors from the sign-in and account endpoints, as JSON: `{"error": "...", "message": "..."}`.
//! Internal failures are logged and reported only as `server_error`, with nothing about why.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The request is wrong in a way the caller can fix.
    #[error("{1}")]
    BadRequest(&'static str, String),
    /// The credentials didn't check out. The message never says which part was wrong.
    #[error("{1}")]
    Unauthorized(&'static str, &'static str),
    #[error("{1}")]
    Forbidden(&'static str, &'static str),
    #[error("not found")]
    NotFound,
    #[error("too many attempts; try again later")]
    TooManyAttempts,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AuthError {
    pub fn bad(code: &'static str, message: impl Into<String>) -> Self {
        Self::BadRequest(code, message.into())
    }

    pub fn internal(e: impl Into<anyhow::Error>) -> Self {
        Self::Internal(e.into())
    }
}

macro_rules! internal_from {
    ($($t:ty),*) => {$(
        impl From<$t> for AuthError {
            fn from(e: $t) -> Self {
                Self::Internal(e.into())
            }
        }
    )*};
}
internal_from!(
    sqlx::Error,
    crate::db::DbError,
    crate::crypto::CryptoError,
    serde_json::Error
);

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            Self::BadRequest(code, message) => (StatusCode::BAD_REQUEST, *code, message.clone()),
            Self::Unauthorized(code, message) => {
                (StatusCode::UNAUTHORIZED, *code, (*message).to_owned())
            }
            Self::Forbidden(code, message) => (StatusCode::FORBIDDEN, *code, (*message).to_owned()),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found", self.to_string()),
            Self::TooManyAttempts => (
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_attempts",
                self.to_string(),
            ),
            Self::Internal(error) => {
                tracing::error!(error = format!("{error:#}"), "sign-in request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    "something went wrong on the server".to_owned(),
                )
            }
        };
        (
            status,
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({ "error": code, "message": message })),
        )
            .into_response()
    }
}

pub type AuthResult<T> = Result<T, AuthError>;
