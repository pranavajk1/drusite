// SPDX-License-Identifier: AGPL-3.0-or-later

//! Accounts, as sign-in sees them.

use drusite_model::UserId;
use uuid::Uuid;

use super::error::{AuthError, AuthResult};
use crate::app::AppState;

#[derive(Debug, Clone)]
pub(super) struct Account {
    pub id: UserId,
    pub email: String,
    pub email_verified: bool,
    pub display_name: String,
    pub disabled: bool,
}

type Row = (Uuid, String, bool, String, bool);

fn from_row((id, email, email_verified, display_name, disabled): Row) -> Account {
    Account {
        id: UserId::from_uuid(id),
        email,
        email_verified,
        display_name,
        disabled,
    }
}

const COLUMNS: &str = "id, email, email_verified, display_name, disabled_at IS NOT NULL";

pub(super) async fn by_email(state: &AppState, email: &str) -> AuthResult<Option<Account>> {
    let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM users WHERE lower(email) = lower($1)"
    )))
    .bind(email)
    .fetch_optional(state.db().pool())
    .await?;
    Ok(row.map(from_row))
}

pub(super) async fn by_id(state: &AppState, id: UserId) -> AuthResult<Option<Account>> {
    let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM users WHERE id = $1"
    )))
    .bind(id.as_uuid())
    .fetch_optional(state.db().pool())
    .await?;
    Ok(row.map(from_row))
}

/// A plausible email address: something@something, no spaces, a sane length. Whether it's
/// real is for the mail server to say.
pub(super) fn normalize_email(email: &str) -> AuthResult<String> {
    let email = email.trim();
    let ok = email.len() <= 254
        && !email.chars().any(char::is_whitespace)
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        });
    if ok {
        Ok(email.to_owned())
    } else {
        Err(AuthError::bad(
            "bad_email",
            "that doesn't look like an email address",
        ))
    }
}

/// Makes an account. Fails with `account_exists` if the email is taken.
pub(super) async fn create(
    state: &AppState,
    email: &str,
    display_name: &str,
    email_verified: bool,
) -> AuthResult<Account> {
    create_with_id(state, UserId::new(), email, display_name, email_verified).await
}

/// Makes an account with a chosen ID: a passkey sign-up registers the passkey for the ID
/// before the account exists.
pub(super) async fn create_with_id(
    state: &AppState,
    id: UserId,
    email: &str,
    display_name: &str,
    email_verified: bool,
) -> AuthResult<Account> {
    let result = sqlx::query(
        "INSERT INTO users (id, email, email_verified, display_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(id.as_uuid())
    .bind(email)
    .bind(email_verified)
    .bind(display_name.trim())
    .execute(state.db().pool())
    .await;
    match result {
        Ok(_) => Ok(Account {
            id,
            email: email.to_owned(),
            email_verified,
            display_name: display_name.trim().to_owned(),
            disabled: false,
        }),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(AuthError::bad(
            "account_exists",
            "an account with that email already exists; sign in instead",
        )),
        Err(e) => Err(e.into()),
    }
}

pub(super) async fn mark_email_verified(state: &AppState, id: UserId) -> AuthResult<()> {
    sqlx::query("UPDATE users SET email_verified = true WHERE id = $1")
        .bind(id.as_uuid())
        .execute(state.db().pool())
        .await?;
    Ok(())
}

/// Rate limits, kept in Postgres so every server node counts together.
pub(super) mod limits {
    use super::{AuthError, AuthResult};
    use crate::app::AppState;

    /// Refuses if `key` has been recorded `max` times in the last `minutes`.
    pub(crate) async fn check(
        state: &AppState,
        key: &str,
        max: i64,
        minutes: i32,
    ) -> AuthResult<()> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM auth_attempts
             WHERE key = $1 AND at > now() - make_interval(mins => $2)",
        )
        .bind(key)
        .bind(minutes)
        .fetch_one(state.db().pool())
        .await?;
        if count >= max {
            Err(AuthError::TooManyAttempts)
        } else {
            Ok(())
        }
    }

    pub(crate) async fn record(state: &AppState, key: &str) -> AuthResult<()> {
        sqlx::query("INSERT INTO auth_attempts (key) VALUES ($1)")
            .bind(key)
            .execute(state.db().pool())
            .await?;
        Ok(())
    }

    pub(crate) async fn clear(state: &AppState, key: &str) -> AuthResult<()> {
        sqlx::query("DELETE FROM auth_attempts WHERE key = $1")
            .bind(key)
            .execute(state.db().pool())
            .await?;
        Ok(())
    }
}
