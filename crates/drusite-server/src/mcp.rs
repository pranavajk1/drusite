// SPDX-License-Identifier: AGPL-3.0-or-later

//! Agents: the MCP endpoint (`/mcp`, Streamable HTTP), its OAuth protected resource metadata,
//! and the activity log the apps show, with undo.
//!
//! An agent signs in with OAuth ([`crate::auth::oauth`]) and gets a token for this endpoint
//! only. Every request is checked here before it reaches a tool, and every tool acts as the
//! agent's user, through the same policy and row-level security as the apps.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use drusite_crdt::{Page, Workspace};
use drusite_mcp::{
    Access, Activity, Agent, Change, Hit, McpServer, Notes, ToolError, ToolResult, WorkspaceInfo,
};
use drusite_model::{PageId, UserId, WorkspaceId};
use drusite_proto::DocKind;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::oauth::{issuer, mcp_resource};
use crate::auth::tokens;
use crate::auth::{AuthError, Authenticated};
use crate::policy::Role;
use crate::sync::store::{self, DocError};

pub const MCP_PATH: &str = "/mcp";
const METADATA_PATH: &str = "/.well-known/oauth-protected-resource";

/// The MCP endpoint and its metadata; nothing when agents are off.
pub fn routes(state: &AppState) -> Router<AppState> {
    let activity = Router::new()
        .route("/api/v1/workspaces/{ws}/agent-activity", get(list_activity))
        .route(
            "/api/v1/workspaces/{ws}/agent-activity/{id}/undo",
            post(undo_activity),
        );
    if !state.config().agents.enabled {
        return activity;
    }
    let public = &state.config().public_url;
    let mut hosts = Vec::new();
    if let Some(host) = public.host_str() {
        hosts.push(host.to_owned());
        if let Some(port) = public.port() {
            hosts.push(format!("{host}:{port}"));
        }
    }
    let notes = Arc::new(ServerNotes {
        state: state.clone(),
    });
    let service = StreamableHttpService::new(
        move || {
            Ok(McpServer::new(
                Arc::clone(&notes),
                env!("CARGO_PKG_VERSION"),
            ))
        },
        Arc::new(NeverSessionManager::default()),
        StreamableHttpServerConfig::default()
            // Stateless (the 2026-07-28 protocol always is), so any node can answer any call.
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_allowed_hosts(hosts)
            // Agents aren't web pages: a browser's request has no business here.
            .enforce_origin_validation(),
    );
    activity
        .route(METADATA_PATH, get(resource_metadata))
        .route(
            &format!("{METADATA_PATH}{MCP_PATH}"),
            get(resource_metadata),
        )
        .route_service(
            MCP_PATH,
            tower::ServiceBuilder::new()
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    require_agent,
                ))
                .service(service),
        )
}

/// RFC 9728: where an agent learns how to sign in for this endpoint.
async fn resource_metadata(State(state): State<AppState>) -> Response {
    (
        [(header::CACHE_CONTROL, "max-age=3600")],
        Json(serde_json::json!({
            "resource": mcp_resource(&state),
            "authorization_servers": [issuer(&state)],
            "scopes_supported": crate::auth::clients::SCOPES,
            "bearer_methods_supported": ["header"],
            "dpop_signing_alg_values_supported": ["ES256"],
            "resource_name": "Drusite",
        })),
    )
        .into_response()
}

fn challenge(state: &AppState, error: Option<&str>) -> Response {
    let base = state.config().public_url.origin().ascii_serialization();
    let mut value = format!(
        r#"Bearer resource_metadata="{base}{METADATA_PATH}{MCP_PATH}", scope="read write""#
    );
    if let Some(e) = error {
        value.push_str(&format!(r#", error="{e}""#));
    }
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": error.unwrap_or("invalid_request"),
            "error_description": "sign in to Drusite to use this endpoint",
        })),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&value) {
        response.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    response
}

/// Lets a request through only with a live token for this endpoint, and gives the tools the
/// agent it is.
async fn require_agent(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let auth = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let Some(auth) = auth else {
        return challenge(&state, None);
    };
    let (scheme, token) = auth.split_once(' ').unwrap_or((&auth, ""));
    let token = token.trim();
    let grant = match tokens::lookup_access(&state, token).await {
        Ok(Some(g)) => g,
        Ok(None) => return challenge(&state, Some("invalid_token")),
        Err(e) => return e.into_response(),
    };
    // The token must be for this endpoint (RFC 8707): the app's are no good here.
    if grant.resource.as_deref() != Some(mcp_resource(&state).as_str()) {
        return challenge(&state, Some("invalid_token"));
    }
    match &grant.dpop_jkt {
        None if scheme.eq_ignore_ascii_case("bearer") => {}
        Some(bound) if scheme.eq_ignore_ascii_case("dpop") => {
            let proof = crate::auth::dpop::check(
                &state,
                request.headers(),
                request.method().as_str(),
                request.uri().path(),
                Some(token),
            )
            .await;
            match proof {
                Ok(p)
                    if bool::from(subtle::ConstantTimeEq::ct_eq(
                        p.jkt.as_bytes(),
                        bound.as_bytes(),
                    )) => {}
                _ => return challenge(&state, Some("invalid_token")),
            }
        }
        _ => return challenge(&state, Some("invalid_token")),
    }
    let Some(access) = grant
        .access
        .and_then(|a| serde_json::from_value::<Access>(a).ok())
    else {
        return challenge(&state, Some("invalid_token"));
    };
    request.extensions_mut().insert(Agent {
        name: grant.client_name,
        access,
        user: Some(*grant.user.as_uuid()),
        grant: grant.grant.0,
    });
    next.run(request).await
}

/// The documents as an agent's user sees them in Postgres.
#[derive(Debug)]
struct ServerNotes {
    state: AppState,
}

fn user_of(agent: &Agent) -> ToolResult<UserId> {
    agent
        .user
        .map(UserId::from_uuid)
        .ok_or_else(|| ToolError::Server("an agent without a user".to_owned()))
}

fn server<E: std::fmt::Display>(e: E) -> ToolError {
    ToolError::Server(e.to_string())
}

fn doc_error(e: DocError) -> ToolError {
    match e {
        DocError::Forbidden => ToolError::Forbidden("not allowed".to_owned()),
        DocError::NotFound => ToolError::NotFound("no such page".to_owned()),
        DocError::Invalid(m) => ToolError::Invalid(m),
        DocError::TooLarge => ToolError::Invalid("that is too large".to_owned()),
        other => ToolError::Server(other.to_string()),
    }
}

async fn load_doc(
    state: &AppState,
    user: UserId,
    doc: Uuid,
    kind: &str,
) -> ToolResult<Option<store::OpenedDoc>> {
    let mut tx = state.db().as_user(user).await.map_err(server)?;
    let opened = store::load(state, &mut tx, doc).await.map_err(doc_error)?;
    tx.commit().await.map_err(server)?;
    Ok(opened.filter(|d| d.kind == kind))
}

async fn role_in(state: &AppState, user: UserId, ws: Uuid) -> ToolResult<Option<Role>> {
    let mut tx = state.db().as_user(user).await.map_err(server)?;
    let role = store::role(&mut tx, ws).await.map_err(doc_error)?;
    tx.commit().await.map_err(server)?;
    Ok(role)
}

fn activity_context(id: Uuid) -> Vec<u8> {
    let mut c = b"drusite agent activity\0".to_vec();
    c.extend(id.as_bytes());
    c
}

/// Saves `change` to `ws` as `user`: new pages first, then the updates, in one transaction with
/// `record` (an activity to add, or one to mark undone).
async fn save(
    state: &AppState,
    user: UserId,
    ws: Uuid,
    change: Change,
    record: Record<'_>,
) -> ToolResult<()> {
    if !matches!(
        role_in(state, user, ws).await?,
        Some(Role::Owner | Role::Editor)
    ) {
        return Err(ToolError::Forbidden(
            "the person can only read this workspace".to_owned(),
        ));
    }
    let origin = Uuid::now_v7();
    for (page, snapshot) in &change.created {
        store::open(
            state,
            user,
            *page.as_uuid(),
            ws,
            DocKind::Page,
            snapshot,
            origin,
        )
        .await
        .map_err(doc_error)?;
    }
    let mut tx = state.db().as_user(user).await.map_err(server)?;
    let mut compact = Vec::new();
    let updates = change
        .pages
        .iter()
        .map(|(p, u)| (*p.as_uuid(), u))
        .chain(change.workspace.as_ref().map(|u| (ws, u)));
    for (doc, update) in updates {
        let (_, due) = store::append_in(state, &mut tx, user, doc, ws, update, Some(origin))
            .await
            .map_err(doc_error)?;
        if due {
            compact.push(doc);
        }
    }
    match record {
        Record::New(agent, activity) => {
            let id = Uuid::now_v7();
            let key = state
                .keys()
                .current(&mut tx, WorkspaceId::from_uuid(ws))
                .await
                .map_err(server)?;
            let summary = key
                .encrypt(&activity_context(id), activity.summary.as_bytes())
                .map_err(server)?;
            sqlx::query(
                "INSERT INTO agent_activity (id, workspace_id, user_id, grant_id, agent_name,
                     tool, summary, effects)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(id)
            .bind(ws)
            .bind(user.as_uuid())
            .bind(agent.grant)
            .bind(&agent.name)
            .bind(&activity.tool)
            .bind(summary)
            .bind(serde_json::to_value(&activity.effects).map_err(server)?)
            .execute(&mut *tx)
            .await
            .map_err(server)?;
        }
        Record::Undone(id) => {
            sqlx::query(
                "UPDATE agent_activity SET undone_at = now(), undone_by = $2
                 WHERE id = $1 AND undone_at IS NULL",
            )
            .bind(id)
            .bind(user.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(server)?;
        }
    }
    tx.commit().await.map_err(server)?;
    for doc in compact {
        state.hub().request_compaction(doc);
    }
    Ok(())
}

enum Record<'a> {
    New(&'a Agent, Activity),
    Undone(Uuid),
}

impl Notes for ServerNotes {
    async fn workspaces(&self, agent: &Agent) -> ToolResult<Vec<WorkspaceInfo>> {
        let list = store::workspaces(&self.state, user_of(agent)?)
            .await
            .map_err(doc_error)?;
        Ok(list
            .into_iter()
            .map(|(id, role)| WorkspaceInfo {
                id: WorkspaceId::from_uuid(id),
                writable: role == "owner" || role == "editor",
            })
            .collect())
    }

    async fn workspace(&self, agent: &Agent, ws: WorkspaceId) -> ToolResult<Workspace> {
        let doc = load_doc(&self.state, user_of(agent)?, *ws.as_uuid(), "workspace")
            .await?
            .ok_or_else(|| ToolError::NotFound("no such workspace".to_owned()))?;
        Ok(Workspace::load_all(&doc.parts())?)
    }

    async fn find_page(&self, agent: &Agent, page: PageId) -> ToolResult<Option<WorkspaceId>> {
        let mut tx = self
            .state
            .db()
            .as_user(user_of(agent)?)
            .await
            .map_err(server)?;
        let ws: Option<Uuid> = sqlx::query_scalar(
            "SELECT workspace_id FROM documents WHERE id = $1 AND kind = 'page'",
        )
        .bind(page.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(server)?;
        tx.commit().await.map_err(server)?;
        Ok(ws.map(WorkspaceId::from_uuid))
    }

    async fn page(&self, agent: &Agent, ws: WorkspaceId, page: PageId) -> ToolResult<Page> {
        let doc = load_doc(&self.state, user_of(agent)?, *page.as_uuid(), "page")
            .await?
            .filter(|d| d.workspace == *ws.as_uuid())
            .ok_or_else(|| {
                ToolError::NotFound("that page hasn't reached the server yet".to_owned())
            })?;
        Ok(Page::load_all(&doc.parts())?)
    }

    async fn search(
        &self,
        agent: &Agent,
        ws: WorkspaceId,
        query: &str,
        limit: usize,
    ) -> ToolResult<Vec<Hit>> {
        if !self.state.config().search.enabled {
            return Err(ToolError::Invalid(
                "search is turned off on this server; use list_pages".to_owned(),
            ));
        }
        let found = crate::search::query::run(
            &self.state,
            user_of(agent)?,
            query,
            limit,
            Some(*ws.as_uuid()),
        )
        .await
        .map_err(server)?;
        Ok(found
            .hits
            .into_iter()
            .map(|h| Hit {
                page: PageId::from_uuid(h.page),
                block: h.block,
                title: h.title,
                snippet: h.snippet.into_iter().map(|p| p.text).collect(),
            })
            .collect())
    }

    async fn share(
        &self,
        agent: &Agent,
        ws: WorkspaceId,
        page: PageId,
        request: drusite_mcp::ShareRequest,
    ) -> ToolResult<String> {
        use crate::publish::{Actor, ShareChange};
        let user = user_of(agent)?;
        let actor = Actor {
            user,
            agent: Some(crate::policy::AgentRights {
                publish: agent.access.publish,
            }),
        };
        let (ws, page) = (*ws.as_uuid(), *page.as_uuid());
        let result = match &request {
            drusite_mcp::ShareRequest::Publish {
                include_subpages,
                allow_search_engines,
            } => {
                crate::publish::publish(
                    &self.state,
                    actor,
                    ws,
                    page,
                    ShareChange {
                        include_subpages: *include_subpages,
                        allow_indexing: *allow_search_engines,
                        ..ShareChange::default()
                    },
                )
                .await
            }
            drusite_mcp::ShareRequest::Unpublish => {
                crate::publish::unpublish(&self.state, actor, ws, page).await
            }
        };
        let share = result.map_err(|e| match e {
            AuthError::NotFound => ToolError::NotFound("no such page".to_owned()),
            AuthError::Forbidden(_, m) => ToolError::Forbidden(m.to_owned()),
            AuthError::BadRequest(_, m) => ToolError::Invalid(m),
            other => ToolError::Server(other.to_string()),
        })?;
        // The person sees it in the agents' activity, like any other change.
        let summary = match (&request, &share.url) {
            (drusite_mcp::ShareRequest::Unpublish, _) => "Took a page off the web".to_owned(),
            (_, Some(_)) => format!(
                "Published a page to the web{}",
                if share.include_subpages {
                    ", with its subpages"
                } else {
                    ""
                }
            ),
            _ => "Changed how a page is published".to_owned(),
        };
        save(
            &self.state,
            user,
            ws,
            Change::default(),
            Record::New(
                agent,
                Activity {
                    tool: "publish_page".to_owned(),
                    summary,
                    effects: Vec::new(),
                },
            ),
        )
        .await?;
        Ok(match share.url {
            Some(url) => format!(
                "Published. Anyone with this link can read it: {url}{}{}",
                if share.include_subpages {
                    " (with its subpages)"
                } else {
                    ""
                },
                if share.allow_indexing {
                    " Search engines may index it."
                } else {
                    ""
                }
            ),
            None => "The page is no longer published; its link doesn't work any more.".to_owned(),
        })
    }

    async fn commit(
        &self,
        agent: &Agent,
        ws: WorkspaceId,
        change: Change,
        activity: Activity,
    ) -> ToolResult<()> {
        save(
            &self.state,
            user_of(agent)?,
            *ws.as_uuid(),
            change,
            Record::New(agent, activity),
        )
        .await
    }
}

/// One agent call, as the apps list it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActivityItem {
    id: Uuid,
    agent: String,
    tool: String,
    summary: String,
    /// Unix milliseconds.
    at: i64,
    undone_at: Option<i64>,
    pages: Vec<Uuid>,
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default = "default_list")]
    limit: i64,
}

const fn default_list() -> i64 {
    50
}

fn millis(t: OffsetDateTime) -> i64 {
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

type ActivityRow = (
    Uuid,
    String,
    String,
    Vec<u8>,
    serde_json::Value,
    OffsetDateTime,
    Option<OffsetDateTime>,
);

async fn open_summary(
    state: &AppState,
    tx: &mut sqlx::PgConnection,
    ws: Uuid,
    id: Uuid,
    sealed: &[u8],
) -> Result<String, AuthError> {
    let version = crate::crypto::sealed_key_version(sealed).map_err(AuthError::internal)?;
    let key = state
        .keys()
        .version(tx, WorkspaceId::from_uuid(ws), version)
        .await
        .map_err(AuthError::internal)?;
    let plain = key
        .decrypt(&activity_context(id), sealed)
        .map_err(AuthError::internal)?;
    Ok(String::from_utf8_lossy(&plain).into_owned())
}

/// `GET /api/v1/workspaces/{ws}/agent-activity`: what agents did there, newest first.
async fn list_activity(
    State(state): State<AppState>,
    auth: Authenticated,
    Path(ws): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<ActivityItem>>, AuthError> {
    let mut tx = state
        .db()
        .as_user(auth.user)
        .await
        .map_err(AuthError::internal)?;
    let rows: Vec<ActivityRow> = sqlx::query_as(
        "SELECT id, agent_name, tool, summary, effects, created_at, undone_at
         FROM agent_activity WHERE workspace_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(ws)
    .bind(q.limit.clamp(1, 200))
    .fetch_all(&mut *tx)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for (id, agent, tool, sealed, effects, at, undone) in rows {
        let summary = open_summary(&state, &mut tx, ws, id, &sealed).await?;
        let effects: Vec<drusite_mcp::Effect> = serde_json::from_value(effects).unwrap_or_default();
        let activity = Activity {
            tool: tool.clone(),
            summary: String::new(),
            effects,
        };
        let mut pages: Vec<Uuid> = activity.pages().iter().map(|p| *p.as_uuid()).collect();
        pages.extend(activity.effects.iter().filter_map(|e| match e {
            drusite_mcp::Effect::Created { page } => Some(*page.as_uuid()),
            _ => None,
        }));
        items.push(ActivityItem {
            id,
            agent,
            tool,
            summary,
            at: millis(at),
            undone_at: undone.map(millis),
            pages,
        });
    }
    tx.commit().await?;
    Ok(Json(items))
}

/// `POST /api/v1/workspaces/{ws}/agent-activity/{id}/undo`: takes back what one call did,
/// keeping everything that changed since.
async fn undo_activity(
    State(state): State<AppState>,
    auth: Authenticated,
    Path((ws, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AuthError> {
    if !auth.can_write() {
        return Err(AuthError::Forbidden(
            "read_only",
            "this sign-in can only read",
        ));
    }
    let mut tx = state
        .db()
        .as_user(auth.user)
        .await
        .map_err(AuthError::internal)?;
    let row: Option<(String, serde_json::Value, Option<OffsetDateTime>)> = sqlx::query_as(
        "SELECT tool, effects, undone_at FROM agent_activity WHERE id = $1 AND workspace_id = $2",
    )
    .bind(id)
    .bind(ws)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    let Some((tool, effects, undone)) = row else {
        return Err(AuthError::NotFound);
    };
    if undone.is_some() {
        return Ok(Json(serde_json::json!({ "status": "already_undone" })));
    }
    let activity = Activity {
        tool,
        summary: String::new(),
        effects: serde_json::from_value(effects).map_err(AuthError::internal)?,
    };
    let tool_error = |e: ToolError| match e {
        ToolError::Forbidden(m) => AuthError::BadRequest("forbidden", m),
        ToolError::NotFound(_) => AuthError::NotFound,
        other => AuthError::internal(anyhow::anyhow!("{other:?}")),
    };
    let doc = load_doc(&state, auth.user, ws, "workspace")
        .await
        .map_err(tool_error)?
        .ok_or(AuthError::NotFound)?;
    let mut workspace = Workspace::load_all(&doc.parts()).map_err(AuthError::internal)?;
    let mut pages = HashMap::new();
    for page in activity.pages() {
        let doc = load_doc(&state, auth.user, *page.as_uuid(), "page")
            .await
            .map_err(tool_error)?
            .ok_or(AuthError::NotFound)?;
        pages.insert(
            page,
            Page::load_all(&doc.parts()).map_err(AuthError::internal)?,
        );
    }
    let now = millis(OffsetDateTime::now_utc());
    let change =
        drusite_mcp::undo(&activity, &mut workspace, &mut pages, now).map_err(tool_error)?;
    save(&state, auth.user, ws, change, Record::Undone(id))
        .await
        .map_err(tool_error)?;
    Ok(Json(serde_json::json!({ "status": "undone" })))
}
