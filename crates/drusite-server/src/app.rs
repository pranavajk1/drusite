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
use crate::search::Embedder;
use crate::storage::BlobStore;
use crate::sync::Hub;
use crate::{auth, health, search, sync, telemetry};

#[derive(Debug)]
struct Inner {
    config: Config,
    db: Db,
    blobs: BlobStore,
    keys: Keyring,
    policy: Policy,
    auth: AuthState,
    hub: Hub,
    embedder: Option<Embedder>,
    chat: Option<crate::ask::Chat>,
    publish: crate::publish::PublicState,
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
        let embedder = match Embedder::from_config(&config) {
            Ok(e) => e,
            Err(error) => {
                tracing::error!(%error, "the embedding model can't be used; searching words only");
                None
            }
        };
        let chat = match crate::ask::Chat::from_config(&config) {
            Ok(c) => c,
            Err(error) => {
                tracing::error!(%error, "the chat model can't be used; Ask is unavailable");
                None
            }
        };
        Self(Arc::new(Inner {
            embedder,
            chat,
            publish: crate::publish::PublicState::new(&config),
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

    /// The embedding model for semantic search, when one is configured.
    pub fn embedder(&self) -> Option<&Embedder> {
        self.0.embedder.as_ref()
    }

    /// The public host's limits.
    pub fn publish(&self) -> &crate::publish::PublicState {
        &self.0.publish
    }

    /// The chat model Ask answers with, when one is configured.
    pub fn chat(&self) -> Option<&crate::ask::Chat> {
        self.0.chat.as_ref()
    }
}

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Published pages on the server's own address, when they have no host of their own; and the
/// robots.txt that goes with that.
fn published_here(state: &AppState) -> Router<AppState> {
    if state.config().publish_separate() {
        Router::new().route(
            "/robots.txt",
            axum::routing::get(|| async { "User-agent: *\nDisallow: /\n" }),
        )
    } else {
        crate::publish::public::public_routes(state)
    }
}

/// A request for the public host's name on the main listener gets the public host, which
/// serves published pages and nothing else.
async fn public_host(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use tower::ServiceExt as _;
    let config = state.config();
    let public = config
        .publish
        .public_url
        .as_ref()
        .and_then(|u| u.host_str())
        .filter(|_| config.publish_separate());
    if let Some(public) = public {
        let asked = request.uri().host().map(str::to_owned).or_else(|| {
            request
                .headers()
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .map(|h| match h.rsplit_once(':') {
                    Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => {
                        name.to_owned()
                    }
                    _ => h.to_owned(),
                })
        });
        if asked.is_some_and(|a| a.trim_end_matches('.').eq_ignore_ascii_case(public)) {
            let app = crate::publish::public_router(state.clone());
            return match app.oneshot(request).await {
                Ok(r) => r,
                Err(never) => match never {},
            };
        }
    }
    next.run(request).await
}

pub fn router(state: AppState) -> Router {
    let timeout = Duration::from_secs(state.config().server.request_timeout_secs);
    // Ordinary requests have a deadline; the sync WebSocket lives as long as the device stays,
    // and attachments take as long as their size needs.
    let requests = Router::new()
        .merge(health::routes())
        .merge(auth::routes(&state))
        .merge(search::routes())
        .merge(crate::ask::routes())
        .merge(crate::mcp::routes(&state))
        .merge(crate::publish::routes())
        .merge(published_here(&state))
        .merge(crate::web::routes(&state))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
        ));
    Router::new()
        .merge(requests)
        .merge(sync::routes())
        // An answer streams for as long as the model takes, within [ask] timeout_secs.
        .merge(crate::ask::streaming_routes())
        // A big file on a slow network takes longer than an ordinary request may; its size is
        // bounded instead ([attachments] max_bytes).
        .merge(crate::attachments::routes(&state))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            public_host,
        ))
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
