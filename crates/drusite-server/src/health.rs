// SPDX-License-Identifier: AGPL-3.0-or-later

//! `/healthz`: the process is up. `/readyz`: it can serve, meaning the database answers with
//! every migration applied, the object store answers and the key source works. Failures say
//! which check failed, never why: the details go to the log, not to whoever asked.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::app::AppState;
use crate::keys::KeySource;

const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Serialize)]
struct Readiness {
    status: &'static str,
    checks: Checks,
}

#[derive(Debug, Serialize)]
struct Checks {
    database: &'static str,
    storage: &'static str,
    keys: &'static str,
}

async fn live() -> &'static str {
    "ok"
}

async fn run<F, E>(name: &str, check: F) -> &'static str
where
    F: Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    match tokio::time::timeout(CHECK_TIMEOUT, check).await {
        Ok(Ok(())) => "ok",
        Ok(Err(error)) => {
            tracing::warn!(check = name, %error, "readiness check failed");
            "failing"
        }
        Err(_) => {
            tracing::warn!(check = name, "readiness check timed out");
            "failing"
        }
    }
}

async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Readiness>) {
    let database = run("database", async {
        sqlx::query("SELECT 1").execute(state.db().pool()).await?;
        if state.db().migrations_current().await? {
            Ok(())
        } else {
            Err(crate::db::DbError::Unsupported(
                "migrations are pending".to_owned(),
            ))
        }
    });
    let storage = run("storage", state.blobs().check_reachable());
    let keys = run("keys", async {
        match state.keys().source() {
            // Local master keys were checked at start and can't stop working.
            KeySource::Local(_) => Ok(()),
            KeySource::Transit(_) => state.keys().source().check().await,
        }
    });
    let (database, storage, keys) = tokio::join!(database, storage, keys);
    let checks = Checks {
        database,
        storage,
        keys,
    };
    let ok = [database, storage, keys].iter().all(|s| *s == "ok");
    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(Readiness {
            status: if ok { "ok" } else { "failing" },
            checks,
        }),
    )
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(live))
        .route("/readyz", get(ready))
}
