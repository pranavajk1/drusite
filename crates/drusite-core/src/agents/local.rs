// SPDX-License-Identifier: AGPL-3.0-or-later

//! The local MCP endpoint: `http://127.0.0.1:<port>/mcp`, while the app runs.
//!
//! Only this computer can reach it (it listens on the loopback address), it refuses requests
//! that come from a web page (an `Origin` header, or a `Host` that isn't loopback, as DNS
//! rebinding would send), and it needs a token made in the app.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use drusite_crdt::{Page, Workspace};
use drusite_mcp::{
    Activity, Agent, Change, Hit, McpServer, Notes, ToolError, ToolResult, WorkspaceInfo,
};
use drusite_model::{PageId, WorkspaceId};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::sync::oneshot;

use super::SharedCore;
use crate::api::ApiError;
use crate::engine::Core;

/// The documents on this computer, for the tools.
#[derive(Debug, Clone)]
pub struct LocalNotes {
    core: SharedCore,
}

impl LocalNotes {
    pub fn new(core: SharedCore) -> Self {
        Self { core }
    }

    /// Runs `f` with the core, off the async threads.
    async fn with<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Core) -> Result<T, ApiError> + Send + 'static,
    ) -> ToolResult<T> {
        let core = Arc::clone(&self.core);
        tokio::task::spawn_blocking(move || {
            let mut core = core
                .lock()
                .map_err(|_| ToolError::Server("the core crashed".to_owned()))?;
            f(&mut core).map_err(|e| match e.code {
                crate::api::ErrorCode::UnknownPage => ToolError::NotFound(e.message),
                crate::api::ErrorCode::Invalid => ToolError::Invalid(e.message),
                _ => ToolError::Server(e.message),
            })
        })
        .await
        .map_err(|e| ToolError::Server(e.to_string()))?
    }
}

impl Notes for LocalNotes {
    async fn workspaces(&self, _agent: &Agent) -> ToolResult<Vec<WorkspaceInfo>> {
        self.with(|core| {
            Ok(vec![WorkspaceInfo {
                id: core.workspace_id(),
                writable: true,
            }])
        })
        .await
    }

    async fn workspace(&self, _agent: &Agent, ws: WorkspaceId) -> ToolResult<Workspace> {
        let copy = self.with(|core| core.workspace_copy()).await?;
        if copy.id() != ws {
            return Err(ToolError::NotFound("no such workspace".to_owned()));
        }
        Ok(copy)
    }

    async fn find_page(&self, _agent: &Agent, page: PageId) -> ToolResult<Option<WorkspaceId>> {
        self.with(move |core| {
            let known = core.workspace.all_pages().contains(&page);
            Ok(known.then(|| core.workspace_id()))
        })
        .await
    }

    async fn page(&self, _agent: &Agent, _ws: WorkspaceId, page: PageId) -> ToolResult<Page> {
        self.with(move |core| core.page_copy(page)).await
    }

    async fn search(
        &self,
        _agent: &Agent,
        _ws: WorkspaceId,
        query: &str,
        limit: usize,
    ) -> ToolResult<Vec<Hit>> {
        let query = query.to_owned();
        let limit = u32::try_from(limit).unwrap_or(50);
        let hits = self.with(move |core| core.search(&query, limit)).await?;
        Ok(hits
            .into_iter()
            .map(|h| Hit {
                page: h.page,
                block: h.block,
                title: h.title,
                snippet: h.snippet.into_iter().map(|p| p.text).collect(),
            })
            .collect())
    }

    async fn commit(
        &self,
        agent: &Agent,
        _ws: WorkspaceId,
        change: Change,
        activity: Activity,
    ) -> ToolResult<()> {
        let agent = agent.clone();
        self.with(move |core| core.commit_agent_call(&agent, change, activity))
            .await
    }
}

/// The running endpoint. Dropping it stops it.
#[derive(Debug)]
pub struct LocalServer {
    pub port: u16,
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

fn unauthorized() -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        axum::Json(serde_json::json!({
            "error": "invalid_token",
            "error_description": "make a token for this agent in Drusite (Settings → Agents) and send it as Authorization: Bearer <token>",
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

/// Starts the endpoint on `port` (on the loopback address only), on the current Tokio runtime.
pub async fn serve(
    core: SharedCore,
    port: u16,
    version: &'static str,
) -> std::io::Result<LocalServer> {
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await?;
    let port = listener.local_addr()?.port();
    let notes = Arc::new(LocalNotes::new(Arc::clone(&core)));
    let service = StreamableHttpService::new(
        move || Ok(McpServer::new(Arc::clone(&notes), version)),
        Arc::new(NeverSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            // Loopback names only (the default), against DNS rebinding; and nothing from a web
            // page.
            .enforce_origin_validation(),
    );
    let auth_core = Arc::clone(&core);
    let check = move |mut request: Request, next: Next| {
        let core = Arc::clone(&auth_core);
        async move {
            let token = request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| {
                    v.strip_prefix("Bearer ")
                        .or_else(|| v.strip_prefix("bearer "))
                })
                .map(|t| t.trim().to_owned());
            let Some(token) = token else {
                return unauthorized();
            };
            let agent = tokio::task::spawn_blocking(move || {
                core.lock()
                    .ok()
                    .and_then(|mut c| c.agent_for(&token).ok().flatten())
            })
            .await
            .ok()
            .flatten();
            let Some(agent) = agent else {
                return unauthorized();
            };
            request.extensions_mut().insert(agent);
            next.run(request).await
        }
    };
    let app = axum::Router::new().route_service(
        "/mcp",
        tower::ServiceBuilder::new()
            .layer(axum::middleware::from_fn(check))
            .service(service),
    );
    let (stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let shutdown = async {
            let _ = stopped.await;
        };
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
        {
            eprintln!("drusite: the local MCP endpoint stopped: {error}");
        }
    });
    Ok(LocalServer {
        port,
        stop: Some(stop),
    })
}
