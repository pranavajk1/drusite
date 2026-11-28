// SPDX-License-Identifier: AGPL-3.0-or-later

//! The web app: the full Drusite app, served at `/` from `[web] dir` (the built `ui/dist-web`).
//!
//! `index.html` at `/`, its service worker at `/sw.js`, and everything else under `/app/`, named
//! by content and cached for good. The build leaves `.br` and `.gz` copies beside the files,
//! sent to browsers that take them. The page gets a strict CSP: this server's own scripts (and
//! WebAssembly), styles and connections, nothing inline, no framing.

use std::path::{Path, PathBuf};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::app::AppState;

/// What the page may load and run.
pub const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
                       style-src 'self'; img-src 'self' data: blob:; font-src 'self'; \
                       connect-src 'self'; worker-src 'self'; object-src 'none'; \
                       base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

fn page_headers(cache: &'static str) -> [(HeaderName, HeaderValue); 6] {
    [
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        ),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ),
        (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
        (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
        (
            HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ),
    ]
}

/// Whether `dir` holds a built web app.
pub fn is_built(dir: &Path) -> bool {
    dir.join("index.html").is_file() && dir.join("app").is_dir()
}

/// The web app's routes, or none when the server has no web app.
pub fn routes(state: &AppState) -> Router<AppState> {
    let Some(dir) = state.config().web.served().map(Path::to_path_buf) else {
        return Router::new();
    };
    let files = ServeDir::new(dir.join("app"))
        .precompressed_br()
        .precompressed_gzip();
    let files = tower::ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .service(files);
    Router::new()
        .route("/", get(index))
        .route("/sw.js", get(service_worker))
        .nest_service("/app", files)
}

/// A file from the web app's folder.
fn file(state: &AppState, name: &str) -> Option<PathBuf> {
    state.config().web.served().map(|d| d.join(name))
}

/// A file of the web app, served with the page's headers.
async fn serve(path: Option<PathBuf>, content_type: &'static str) -> Response {
    let Some(path) = path else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(body) => (
            page_headers("no-cache"),
            [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
            body,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(path = %path.display(), error = %e, "the web app's file is missing");
            (
                StatusCode::NOT_FOUND,
                "the web app isn't installed on this server",
            )
                .into_response()
        }
    }
}

async fn index(State(state): State<AppState>) -> Response {
    serve(file(&state, "index.html"), "text/html; charset=utf-8").await
}

async fn service_worker(State(state): State<AppState>) -> Response {
    serve(file(&state, "sw.js"), "text/javascript; charset=utf-8").await
}
