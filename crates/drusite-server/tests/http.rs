// SPDX-License-Identifier: AGPL-3.0-or-later

//! The router: login methods that are off have no routes, and the health checks.

#![allow(clippy::unwrap_used)] // helpers outside #[test] functions

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use drusite_server::app::{AppState, router};
use drusite_server::config::Config;
use drusite_server::db::Db;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;

fn state(config: Config, db: Db) -> AppState {
    common::state(config, db).0
}

/// A database that isn't there: connecting fails, quickly.
fn absent_db() -> Db {
    Db::from_pool(
        PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(300))
            .connect_lazy("postgres://nobody@127.0.0.1:1/none")
            .unwrap(),
    )
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, String, Option<String>) {
    let response = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let request_id = response
        .headers()
        .get("x-request-id")
        .map(|v| v.to_str().unwrap().to_owned());
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        String::from_utf8(body.to_vec()).unwrap(),
        request_id,
    )
}

#[tokio::test]
async fn login_methods_that_are_off_have_no_routes() {
    let (config, _dir) = common::config(
        "postgres://unused",
        &[
            ("DRUSITE__AUTH__PASSWORD__ENABLED", "false"),
            ("DRUSITE__AUTH__PASSKEYS__ENABLED", "true"),
            ("DRUSITE__AUTH__OIDC__AUTHENTIK__DISPLAY_NAME", "Authentik"),
            (
                "DRUSITE__AUTH__OIDC__AUTHENTIK__ISSUER",
                "https://auth.example.com/o/drusite/",
            ),
            ("DRUSITE__AUTH__OIDC__AUTHENTIK__CLIENT_ID", "drusite"),
            ("DRUSITE__AUTH__OIDC__GOOGLE__ENABLED", "false"),
            ("DRUSITE__AUTH__OIDC__GOOGLE__DISPLAY_NAME", "Google"),
            (
                "DRUSITE__AUTH__OIDC__GOOGLE__ISSUER",
                "https://accounts.google.com",
            ),
            ("DRUSITE__AUTH__OIDC__GOOGLE__CLIENT_ID", "x"),
        ],
    );
    let app = router(state(config, absent_db()));

    let (status, body, request_id) = get(&app, "/api/v1/auth/methods").await;
    assert_eq!(status, StatusCode::OK);
    assert!(request_id.is_some(), "every response carries a request ID");
    let methods: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        methods,
        serde_json::json!({
            "methods": [
                { "type": "passkeys" },
                { "type": "oidc", "id": "authentik", "displayName": "Authentik" },
            ],
            "selfSignup": false,
        })
    );

    // Switched on: the routes exist (these take POST, so a GET is 405). Switched off: nothing
    // there.
    for path in [
        "/api/v1/auth/passkeys/login/start",
        "/api/v1/auth/passkeys/signup/start",
    ] {
        assert_eq!(
            get(&app, path).await.0,
            StatusCode::METHOD_NOT_ALLOWED,
            "{path}"
        );
    }
    for path in [
        "/api/v1/auth/password/login",
        "/api/v1/auth/password/signup",
        "/api/v1/auth/magic-link/start",
        "/api/v1/auth/oidc/google/start",
        "/api/v1/auth/oidc/nonexistent/start",
    ] {
        assert_eq!(get(&app, path).await.0, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn health_and_readiness() {
    // Without a database the process is alive but not ready, and says which check failed
    // without saying why.
    let (config, _dir) = common::config(
        "postgres://unused",
        &[("DRUSITE__AUTH__PASSWORD__ENABLED", "true")],
    );
    let app = router(state(config, absent_db()));
    assert_eq!(get(&app, "/healthz").await.0, StatusCode::OK);
    let (status, body, _) = get(&app, "/readyz").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["checks"]["database"], "failing");
    assert_eq!(body["checks"]["storage"], "ok");
    assert_eq!(body["checks"]["keys"], "ok");
    assert!(!body.to_string().contains("127.0.0.1"));

    // With a migrated database, ready.
    let Some(t) = common::fresh_db().await else {
        return;
    };
    let (config, _dir) =
        common::config(&t.app_url, &[("DRUSITE__AUTH__PASSWORD__ENABLED", "true")]);
    let app = router(state(config, t.db.clone()));
    let (status, body, _) = get(&app, "/readyz").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
