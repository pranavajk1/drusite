// SPDX-License-Identifier: AGPL-3.0-or-later

//! Account administration from the server's own command line. This is the only way into an
//! account that has lost every login method: there is no back-door password.
//!
//! `account recover` (and `account create`, for a first account when sign-up is off) prints a
//! one-time link for the operator to hand to the person. Opening it signs them in once so they
//! can add a login method. Only a hash of the token is stored.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use drusite_model::UserId;
use time::{Duration, OffsetDateTime};
use url::Url;

use crate::crypto::random_bytes;
use crate::db::Db;

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("no account has the email {0:?}")]
    NoSuchAccount(String),
    #[error("an account with the email {0:?} already exists")]
    Exists(String),
    #[error("{0:?} doesn't look like an email address")]
    BadEmail(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Database(#[from] crate::db::DbError),
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
}

/// Where a recovery link points; the token rides in the fragment, so it never reaches server
/// logs or a Referer header.
pub fn recovery_link(public_url: &Url, token: &str) -> Url {
    let mut url = public_url
        .join("auth/recover")
        .expect("a fixed relative path joins");
    url.set_fragment(Some(&format!("token={token}")));
    url
}

pub fn hash_token(token: &str) -> [u8; 32] {
    *blake3::hash(token.as_bytes()).as_bytes()
}

async fn issue(db: &Db, user: UserId, ttl: Duration) -> Result<String, AccountError> {
    let raw = random_bytes::<32>()?;
    let token = URL_SAFE_NO_PAD.encode(raw.as_ref());
    let mut tx = db.anonymous().await?;
    // A new link replaces any earlier unused one.
    sqlx::query("DELETE FROM recovery_tokens WHERE user_id = $1 AND used_at IS NULL")
        .bind(user.as_uuid())
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO recovery_tokens (token_hash, user_id, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(hash_token(&token).as_slice())
    .bind(user.as_uuid())
    .bind(OffsetDateTime::now_utc() + ttl)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(token)
}

/// Issues a one-time sign-in link for an existing account.
pub async fn recover(
    db: &Db,
    email: &str,
    ttl: Duration,
) -> Result<(UserId, String), AccountError> {
    let user: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE lower(email) = lower($1)")
            .bind(email)
            .fetch_optional(db.pool())
            .await?;
    let user =
        UserId::from_uuid(user.ok_or_else(|| AccountError::NoSuchAccount(email.to_owned()))?);
    let token = issue(db, user, ttl).await?;
    Ok((user, token))
}

/// Creates an account and issues its first sign-in link. The email isn't marked verified: the
/// operator vouching for it isn't the person proving they receive mail there.
pub async fn create(
    db: &Db,
    email: &str,
    display_name: &str,
    ttl: Duration,
) -> Result<(UserId, String), AccountError> {
    let email = email.trim();
    if !email.contains('@') || email.starts_with('@') || email.ends_with('@') {
        return Err(AccountError::BadEmail(email.to_owned()));
    }
    let user = UserId::new();
    let inserted = sqlx::query(
        "INSERT INTO users (id, email, display_name) VALUES ($1, $2, $3)
         ON CONFLICT (lower(email)) DO NOTHING",
    )
    .bind(user.as_uuid())
    .bind(email)
    .bind(display_name)
    .execute(db.pool())
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(AccountError::Exists(email.to_owned()));
    }
    let token = issue(db, user, ttl).await?;
    Ok((user, token))
}
