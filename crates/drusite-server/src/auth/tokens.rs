// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grants, refresh tokens and access tokens.
//!
//! A grant is one device's sign-in, bound to its DPoP key. Its refresh token rotates on every
//! use: each refresh returns a new one and retires the old. Presenting a retired refresh token
//! means two parties hold it, so the grant is revoked. Access tokens are opaque and short-lived,
//! and checked against the database on every connection, so revoking a grant ends its access at
//! once.

use drusite_model::UserId;
use serde::Serialize;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::error::AuthError;
use super::secret::{self, Kind};
use super::session::Method;
use crate::app::AppState;

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub refresh_token: String,
    pub scope: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct GrantId(pub Uuid);

fn hashed(kind: Kind, token: &str) -> Result<Vec<u8>, AuthError> {
    secret::hash(kind, token)
        .map(|h| h.to_vec())
        .ok_or_else(|| AuthError::internal(anyhow::anyhow!("a new token didn't hash")))
}

async fn issue_pair(
    state: &AppState,
    conn: &mut sqlx::PgConnection,
    grant: Uuid,
    scope: String,
) -> Result<TokenResponse, AuthError> {
    let minutes = i64::from(state.config().auth.access_token_minutes);
    let access = secret::generate(Kind::Access)?;
    let refresh = secret::generate(Kind::Refresh)?;
    sqlx::query("INSERT INTO access_tokens (token_hash, grant_id, expires_at) VALUES ($1, $2, $3)")
        .bind(hashed(Kind::Access, &access)?)
        .bind(grant)
        .bind(OffsetDateTime::now_utc() + Duration::minutes(minutes))
        .execute(&mut *conn)
        .await?;
    sqlx::query("INSERT INTO refresh_tokens (token_hash, grant_id) VALUES ($1, $2)")
        .bind(hashed(Kind::Refresh, &refresh)?)
        .bind(grant)
        .execute(&mut *conn)
        .await?;
    Ok(TokenResponse {
        access_token: access,
        token_type: "DPoP",
        expires_in: minutes * 60,
        refresh_token: refresh,
        scope,
    })
}

/// What a new grant is for.
#[derive(Debug)]
pub struct NewGrant<'a> {
    pub user: UserId,
    pub client_id: &'a str,
    pub scope: String,
    pub dpop_jkt: &'a str,
    pub method: Method,
    pub device_name: &'a str,
}

/// Starts a device's sign-in, inside the caller's transaction (which also spends the code).
pub async fn create_grant(
    state: &AppState,
    conn: &mut sqlx::PgConnection,
    grant: &NewGrant<'_>,
) -> Result<(GrantId, TokenResponse), AuthError> {
    let auth = &state.config().auth;
    let now = OffsetDateTime::now_utc();
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO oauth_grants (id, user_id, client_id, scope, dpop_jkt, auth_method,
             device_name, idle_expires_at, absolute_expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(grant.user.as_uuid())
    .bind(grant.client_id)
    .bind(&grant.scope)
    .bind(grant.dpop_jkt)
    .bind(grant.method.as_str())
    .bind(grant.device_name.chars().take(100).collect::<String>())
    .bind(now + Duration::days(i64::from(auth.device_idle_days)))
    .bind(now + Duration::days(i64::from(auth.device_max_days)))
    .execute(&mut *conn)
    .await?;
    let tokens = issue_pair(state, conn, id, grant.scope.clone()).await?;
    Ok((GrantId(id), tokens))
}

/// How long after a refresh the device may retry it with the same refresh token.
const REFRESH_RETRY_GRACE: Duration = Duration::seconds(60);

#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error("the refresh token isn't valid")]
    Invalid,
    #[error("the refresh token is bound to another key")]
    WrongKey,
    #[error(transparent)]
    Server(#[from] AuthError),
}

impl From<sqlx::Error> for RefreshError {
    fn from(e: sqlx::Error) -> Self {
        Self::Server(e.into())
    }
}

/// Swaps a refresh token for a new pair. `jkt` is the thumbprint of the key that signed the
/// request's DPoP proof.
pub async fn refresh(
    state: &AppState,
    refresh_token: &str,
    jkt: &str,
) -> Result<TokenResponse, RefreshError> {
    let hash = secret::hash(Kind::Refresh, refresh_token).ok_or(RefreshError::Invalid)?;
    let mut tx = state.db().pool().begin().await?;
    // Lock the grant's row, so two refreshes of one grant run one after the other.
    let row: Option<(Uuid, Option<OffsetDateTime>, String, String, bool)> = sqlx::query_as(
        "SELECT g.id, r.used_at, g.dpop_jkt, g.scope,
                g.revoked_at IS NULL AND g.idle_expires_at > now()
                    AND g.absolute_expires_at > now() AND u.disabled_at IS NULL
         FROM refresh_tokens r
         JOIN oauth_grants g ON g.id = r.grant_id
         JOIN users u ON u.id = g.user_id
         WHERE r.token_hash = $1
         FOR UPDATE OF g, r",
    )
    .bind(hash.as_slice())
    .fetch_optional(&mut *tx)
    .await?;
    let Some((grant, used_at, bound_jkt, scope, live)) = row else {
        return Err(RefreshError::Invalid);
    };
    if !live {
        return Err(RefreshError::Invalid);
    }
    // The token alone is worth nothing without the device's key: refuse, but don't punish the
    // grant for it.
    if !bool::from(subtle::ConstantTimeEq::ct_eq(
        bound_jkt.as_bytes(),
        jkt.as_bytes(),
    )) {
        return Err(RefreshError::WrongKey);
    }
    match used_at {
        // Retired within the last minute: most likely the device's own retry after the
        // response to its refresh was lost on a bad network. The proof shows it holds the key,
        // which never leaves the device, so start the chain again from here: the token it never
        // received is retired, and it gets a new pair.
        Some(used) if OffsetDateTime::now_utc() - used < REFRESH_RETRY_GRACE => {
            sqlx::query("DELETE FROM refresh_tokens WHERE grant_id = $1 AND used_at IS NULL")
                .bind(grant)
                .execute(&mut *tx)
                .await?;
        }
        // Retired longer ago: two copies of the grant are refreshing. Revoke it.
        Some(_) => {
            sqlx::query(
                "UPDATE oauth_grants SET revoked_at = now(),
                     revoked_reason = 'refresh token reused'
                 WHERE id = $1",
            )
            .bind(grant)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            tracing::warn!(%grant, "an old refresh token came back; the device's sign-in is revoked");
            return Err(RefreshError::Invalid);
        }
        None => {
            sqlx::query("UPDATE refresh_tokens SET used_at = now() WHERE token_hash = $1")
                .bind(hash.as_slice())
                .execute(&mut *tx)
                .await?;
        }
    }
    let idle = Duration::days(i64::from(state.config().auth.device_idle_days));
    sqlx::query(
        "UPDATE oauth_grants SET last_used_at = now(),
             idle_expires_at = least(now() + $2, absolute_expires_at)
         WHERE id = $1",
    )
    .bind(grant)
    .bind(idle)
    .execute(&mut *tx)
    .await?;
    // Older access tokens of this grant stop working: a device holds one at a time.
    sqlx::query("DELETE FROM access_tokens WHERE grant_id = $1")
        .bind(grant)
        .execute(&mut *tx)
        .await?;
    let tokens = issue_pair(state, &mut tx, grant, scope).await?;
    tx.commit().await?;
    Ok(tokens)
}

/// A live access token, as the resource endpoints see it.
#[derive(Debug, Clone)]
pub struct AccessGrant {
    pub grant: GrantId,
    pub user: UserId,
    pub dpop_jkt: String,
    pub scope: String,
}

pub async fn lookup_access(
    state: &AppState,
    token: &str,
) -> Result<Option<AccessGrant>, AuthError> {
    let Some(hash) = secret::hash(Kind::Access, token) else {
        return Ok(None);
    };
    let row: Option<(Uuid, Uuid, String, String)> = sqlx::query_as(
        "SELECT g.id, g.user_id, g.dpop_jkt, g.scope
         FROM access_tokens a
         JOIN oauth_grants g ON g.id = a.grant_id
         JOIN users u ON u.id = g.user_id
         WHERE a.token_hash = $1 AND a.expires_at > now()
           AND g.revoked_at IS NULL AND g.absolute_expires_at > now() AND u.disabled_at IS NULL",
    )
    .bind(hash.as_slice())
    .fetch_optional(state.db().pool())
    .await?;
    Ok(row.map(|(grant, user, dpop_jkt, scope)| AccessGrant {
        grant: GrantId(grant),
        user: UserId::from_uuid(user),
        dpop_jkt,
        scope,
    }))
}

/// Revokes the grant a refresh or access token belongs to (RFC 7009). Unknown tokens are
/// ignored, as the RFC asks.
pub async fn revoke_by_token(state: &AppState, token: &str) -> Result<(), AuthError> {
    let grant: Option<Uuid> = if let Some(hash) = secret::hash(Kind::Refresh, token) {
        sqlx::query_scalar("SELECT grant_id FROM refresh_tokens WHERE token_hash = $1")
            .bind(hash.as_slice())
            .fetch_optional(state.db().pool())
            .await?
    } else if let Some(hash) = secret::hash(Kind::Access, token) {
        sqlx::query_scalar("SELECT grant_id FROM access_tokens WHERE token_hash = $1")
            .bind(hash.as_slice())
            .fetch_optional(state.db().pool())
            .await?
    } else {
        None
    };
    if let Some(grant) = grant {
        revoke_grant(state, GrantId(grant), "signed out").await?;
    }
    Ok(())
}

pub async fn revoke_grant(state: &AppState, grant: GrantId, reason: &str) -> Result<(), AuthError> {
    let mut tx = state.db().pool().begin().await?;
    sqlx::query(
        "UPDATE oauth_grants SET revoked_at = now(), revoked_reason = $2
         WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(grant.0)
    .bind(reason)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM access_tokens WHERE grant_id = $1")
        .bind(grant.0)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM refresh_tokens WHERE grant_id = $1")
        .bind(grant.0)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Deletes what has expired. Run now and then by any node.
pub async fn sweep(state: &AppState) -> Result<(), sqlx::Error> {
    let pool = state.db().pool();
    for sql in [
        "DELETE FROM dpop_proofs WHERE expires_at < now()",
        "DELETE FROM access_tokens WHERE expires_at < now()",
        "DELETE FROM auth_challenges WHERE expires_at < now()",
        "DELETE FROM browser_sessions WHERE expires_at < now()",
        "DELETE FROM oauth_codes WHERE expires_at < now() - interval '1 day'",
        "DELETE FROM login_tokens WHERE expires_at < now() - interval '1 day'",
        "DELETE FROM recovery_tokens WHERE expires_at < now() - interval '1 day'",
        "DELETE FROM auth_attempts WHERE at < now() - interval '1 day'",
        // Keep a revoked or expired grant for a while, so the account page can show it.
        "DELETE FROM oauth_grants WHERE (revoked_at < now() - interval '30 days')
             OR absolute_expires_at < now() - interval '30 days'
             OR idle_expires_at < now() - interval '30 days'",
    ] {
        sqlx::query(sql).execute(pool).await?;
    }
    Ok(())
}
