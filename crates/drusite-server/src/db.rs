// SPDX-License-Identifier: AGPL-3.0-or-later

//! PostgreSQL: the pool, the checks before migrating, the migrations, and transactions scoped
//! to a user (or to the system) for row-level security.

use std::time::Duration;

use drusite_model::UserId;
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Postgres, Transaction};

use crate::config::DatabaseConfig;

/// Drusite's migrations, built into the binary.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// The oldest PostgreSQL Drusite runs on.
pub const MIN_SERVER_VERSION: i32 = 160_000;
/// The oldest pgvector Drusite runs with.
pub const MIN_PGVECTOR: (u32, u32) = (0, 8);

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("{0}")]
    Unsupported(String),
}

/// A transaction under one user's row-level security.
pub type UserTx<'a> = Transaction<'a, Postgres>;

#[derive(Debug, Clone)]
pub struct Db {
    pool: PgPool,
}

impl Db {
    pub async fn connect(config: &DatabaseConfig) -> Result<Self, DbError> {
        let options: PgConnectOptions = config.url.expose().parse()?;
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options.application_name("drusite-server"))
            .await?;
        Ok(Self { pool })
    }

    pub const fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Checks the server before anything is changed: PostgreSQL 16+, a role that row-level
    /// security applies to, and pgvector 0.8+ available and created in this database.
    pub async fn preflight(&self, allow_rls_bypass: bool) -> Result<(), DbError> {
        let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
            .fetch_one(&self.pool)
            .await?;
        if version < MIN_SERVER_VERSION {
            return Err(DbError::Unsupported(format!(
                "Drusite needs PostgreSQL 16 or newer; this server is {}.{}",
                version / 10_000,
                version % 10_000
            )));
        }

        // Before anything is created: refuse a role that row-level security doesn't bind.
        let (superuser, bypass): (bool, bool) = sqlx::query_as(
            "SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user",
        )
        .fetch_one(&self.pool)
        .await?;
        if (superuser || bypass) && !allow_rls_bypass {
            return Err(DbError::Unsupported(
                "the database role is a superuser or has BYPASSRLS, so row-level security \
                 wouldn't separate workspaces. Connect as an ordinary role (it can own the \
                 database), or set [database] allow_rls_bypass = true to accept that."
                    .to_owned(),
            ));
        }
        let (available, installed): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT (SELECT default_version FROM pg_available_extensions WHERE name = 'vector'),
                    (SELECT extversion FROM pg_extension WHERE extname = 'vector')",
        )
        .fetch_one(&self.pool)
        .await?;
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&self.pool)
            .await?;
        match (available, installed) {
            (None, _) => {
                return Err(DbError::Unsupported(
                    "pgvector isn't installed on this PostgreSQL server; Drusite needs pgvector \
                     0.8 or newer (for example the pgvector/pgvector image)"
                        .to_owned(),
                ));
            }
            (Some(_), Some(v)) | (Some(v), None) if !pgvector_new_enough(&v) => {
                return Err(DbError::Unsupported(format!(
                    "Drusite needs pgvector 0.8 or newer; this server has {v} \
                     (ALTER EXTENSION vector UPDATE)"
                )));
            }
            (Some(_), None) => {
                // pgvector isn't a trusted extension: only a superuser (or the database owner
                // on managed services that allow it) can create it.
                if let Err(e) = sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
                    .execute(&self.pool)
                    .await
                {
                    return Err(DbError::Unsupported(format!(
                        "pgvector isn't enabled in database {database:?} and this role can't \
                         enable it ({e}). Ask a database administrator to run \
                         `CREATE EXTENSION vector;` in it."
                    )));
                }
            }
            (Some(_), Some(_)) => {}
        }

        Ok(())
    }

    pub async fn migrate(&self) -> Result<(), DbError> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// Whether every built-in migration has been applied, for the readiness check.
    pub async fn migrations_current(&self) -> Result<bool, DbError> {
        let latest = MIGRATOR.iter().map(|m| m.version).max().unwrap_or(0);
        let applied: Option<i64> =
            sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success")
                .fetch_one(&self.pool)
                .await?;
        Ok(applied == Some(latest))
    }

    /// A transaction that row-level security treats as `user`.
    pub async fn as_user(&self, user: UserId) -> Result<UserTx<'static>, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('drusite.user_id', $1, true)")
            .bind(user.to_string())
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }

    /// Like [`Self::as_user`], with one consistent view of the database for the whole
    /// transaction (repeatable read), for reads that must not see a compaction halfway.
    pub async fn as_user_consistent(&self, user: UserId) -> Result<UserTx<'static>, DbError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ")
            .await?;
        sqlx::query("SELECT set_config('drusite.user_id', $1, true)")
            .bind(user.to_string())
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }

    /// A transaction for work that serves no single user, which row-level security lets
    /// through. Only for jobs and admin commands, after their own checks.
    pub async fn as_system(&self) -> Result<UserTx<'static>, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('drusite.system', 'on', true)")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }

    /// A transaction for tables without row-level security (accounts, sign-in), with neither
    /// a user nor the system set, so workspace data stays out of reach.
    pub async fn anonymous(&self) -> Result<UserTx<'static>, DbError> {
        Ok(self.pool.begin().await?)
    }
}

fn pgvector_new_enough(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    (major, minor) >= MIN_PGVECTOR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pgvector_versions() {
        assert!(pgvector_new_enough("0.8.0"));
        assert!(pgvector_new_enough("0.8.7"));
        assert!(pgvector_new_enough("1.0"));
        assert!(!pgvector_new_enough("0.7.4"));
        assert!(!pgvector_new_enough("garbage"));
    }
}
