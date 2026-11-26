// SPDX-License-Identifier: AGPL-3.0-or-later

//! The server's side of DPoP: checks a request's proof and refuses replays.

use axum::http::HeaderMap;
use drusite_dpop::{DpopError, Expected, Verified};
use time::{Duration, OffsetDateTime};

use crate::app::AppState;

/// How far a proof's `iat` may be from the server's clock, in seconds.
pub const MAX_SKEW: i64 = 60;

#[derive(Debug, thiserror::Error)]
pub enum ProofError {
    #[error("send exactly one DPoP header")]
    Missing,
    #[error(transparent)]
    Invalid(#[from] DpopError),
    #[error("this DPoP proof has been used before")]
    Replayed,
    #[error("the DPoP proof is signed by another key than the token is bound to")]
    WrongKey,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// The URL a proof for `path` on this server must name.
pub fn htu(state: &AppState, path: &str) -> String {
    format!(
        "{}{path}",
        state.config().public_url.origin().ascii_serialization()
    )
}

/// Checks the request's DPoP proof for `method` and `path`, bound to `access_token` if there
/// is one, and remembers its `jti` so it can't be used again.
pub async fn check(
    state: &AppState,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    access_token: Option<&str>,
) -> Result<Verified, ProofError> {
    let mut values = headers.get_all("dpop").iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return Err(ProofError::Missing);
    };
    let proof = value.to_str().map_err(|_| ProofError::Missing)?;
    let now = OffsetDateTime::now_utc();
    let htu = htu(state, path);
    let verified = drusite_dpop::verify(
        proof,
        &Expected {
            htm: method,
            htu: &htu,
            now: now.unix_timestamp(),
            max_skew: MAX_SKEW,
            access_token,
        },
    )?;
    // A proof older than the skew window is refused anyway, so its jti needn't be kept longer.
    let inserted = sqlx::query(
        "INSERT INTO dpop_proofs (jkt, jti, expires_at) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING",
    )
    .bind(&verified.jkt)
    .bind(&verified.jti)
    .bind(now + Duration::seconds(2 * MAX_SKEW + 5))
    .execute(state.db().pool())
    .await?
    .rows_affected();
    if inserted == 0 {
        return Err(ProofError::Replayed);
    }
    Ok(verified)
}
