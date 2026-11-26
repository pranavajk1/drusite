// SPDX-License-Identifier: AGPL-3.0-or-later

//! The server's shared state and its HTTP router.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderName, StatusCode, header};
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::auth::AuthState;
use crate::auth::mail::Mailer;
use crate::config::Config;
use crate::db::Db;
use crate::keys::Keyring;
use crate::policy::Policy;
use crate::storage::BlobStore;
use crate::sync::Hub;
use crate::{auth, health, sync, telemetry};

#[derive(Debug)]
struct Inner {
    config: Config,
    db: Db,
    blobs: BlobStore,
    keys: Keyring,
    policy: Policy,
    auth: AuthState,
    hub: Hub,
}

/// Everything a request handler needs. Cheap to clone.
#[derive(Debug, Clone)]
pub struct AppState(Arc<Inner>);

impl AppState {
    pub fn new(
        config: Config,
        db: Db,
        blobs: BlobStore,
        keys: Keyring,
        policy: Policy,
        auth: AuthState,
    ) -> Self {
        Self(Arc::new(Inner {
            config,
            db,
            blobs,
            keys,
            policy,
            auth,
            hub: Hub::default(),
        }))
    }

    pub fn auth(&self) -> &AuthState {
        &self.0.auth
    }

    pub fn hub(&self) -> &Hub {
        &self.0.hub
    }

    pub fn mailer(&self) -> &Mailer {
        &self.0.auth.mailer
    }

    pub fn config(&self) -> &Config {
        &self.0.config
    }

    pub fn db(&self) -> &Db {
        &self.0.db
    }

    pub fn blobs(&self) -> &BlobStore {
        &self.0.blobs
    }

    pub fn keys(&self) -> &Keyring {
        &self.0.keys
    }

    pub fn policy(&self) -> &Policy {
        &self.0.policy
    }
}

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

pub fn router(state: AppState) -> Router {
    let timeout = Duration::from_secs(state.config().server.request_timeout_secs);
    // Ordinary requests have a deadline; the sync WebSocket lives as long as the device stays.
    let requests = Router::new()
        .merge(health::routes())
        .merge(auth::routes(&state))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
        ));
    Router::new()
        .merge(requests)
        .merge(sync::routes())
        .layer(axum::middleware::from_fn(telemetry::record_request))
        .layer(
            ServiceBuilder::new()
                .layer(SetSensitiveRequestHeadersLayer::new([
                    header::AUTHORIZATION,
                    header::COOKIE,
                    HeaderName::from_static("dpop"),
                ]))
                .layer(SetRequestIdLayer::new(REQUEST_ID, MakeRequestUuid))
                .layer(TraceLayer::new_for_http().make_span_with(telemetry::request_span))
                .layer(PropagateRequestIdLayer::new(REQUEST_ID))
                .layer(CatchPanicLayer::new()),
        )
        .with_state(state)
}
