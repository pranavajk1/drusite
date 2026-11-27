// SPDX-License-Identifier: AGPL-3.0-or-later

//! Publish to web: a page shared as a read-only link, rendered by this server as plain HTML,
//! like Notion's "Share to web".
//!
//! - **Live.** A link shows the page as it is now. Unpublishing, or making a new link, takes the
//!   old address down at once.
//! - **Links are tokens**, 256 random bits, never a page's ID. The server finds a link by the
//!   token's hash and keeps the token itself sealed with the workspace key, so the person can
//!   copy the link again. Subpages and files are named by keyed hashes of the token, so no
//!   address says which page or file it is, and none outlives its link.
//! - **The server's record counts.** [`published_pages`] decides what's served. After every
//!   change the server writes the share state into the workspace document as the person who
//!   changed it, so every device shows it; that copy is only for showing.
//! - **Who may publish** is Cedar's `publish_document` (editors and owners; an agent only when
//!   its person allowed it). Reading the share state needs only membership.
//! - **Options**, each off by default: subpages as a small site, search engines, an expiry
//!   date, and a password (argon2id, a rate-limited form, a signed cookie, never in the URL).
//!
//! The pages are read on `[publish] public_url` when one is set: a host that serves nothing but
//! published pages and their files ([`public`]), on a listener of its own if wanted ([`tls`]).
//!
//! [`published_pages`]: ../../migrations/0008_publish.sql

mod highlight;
pub mod public;
pub mod render;
pub mod tls;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use drusite_crdt::{ShareMirror, Workspace};
use drusite_model::{PageId, UserId, WorkspaceId};
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::PgConnection;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::{AuthError, Authenticated};
use crate::policy::{Action, AgentRights, Principal, Resource};
use crate::sync::store;

pub use public::{PublicState, public_router};

/// The owner side: the share state of a page, changed by its workspace's editors.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/workspaces/{ws}/pages/{page}/share",
            get(get_share).put(put_share).delete(delete_share),
        )
        .route(
            "/api/v1/workspaces/{ws}/pages/{page}/share/regenerate",
            post(regenerate_share),
        )
        .route("/api/v1/workspaces/{ws}/shares", get(list_shares))
}

/// How a page is shared, as the apps and agents see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareState {
    pub page: Uuid,
    pub published: bool,
    /// The public link, while it's published.
    pub url: Option<String>,
    pub include_subpages: bool,
    pub allow_indexing: bool,
    /// Unix milliseconds.
    pub expires_at: Option<i64>,
    /// The expiry date has passed: the link shows "not found" until it's moved or removed.
    pub expired: bool,
    pub has_password: bool,
    /// Whether the asker may change it (an editor or owner, and publishing is on).
    pub can_publish: bool,
    /// Unix milliseconds.
    pub published_at: Option<i64>,
    /// Where links go: `[publish] public_url`, or the server's own address.
    pub public_base: String,
}

/// A change to how a page is shared. Absent fields stay as they are (off, for a new link).
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShareChange {
    #[serde(default)]
    pub include_subpages: Option<bool>,
    #[serde(default)]
    pub allow_indexing: Option<bool>,
    /// Unix milliseconds; `null` removes the expiry.
    #[serde(default, deserialize_with = "present")]
    pub expires_at: Option<Option<i64>>,
    /// The new password; `null` removes it.
    #[serde(default, deserialize_with = "present")]
    pub password: Option<Option<String>>,
}

/// A field that's present, even as `null`, is `Some`.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// The shortest password a page may have, and the longest.
pub const MIN_PASSWORD: usize = 4;
pub const MAX_PASSWORD: usize = 256;

/// Who is changing a share: a person through an app, or an agent for its person.
#[derive(Debug, Clone, Copy)]
pub struct Actor {
    pub user: UserId,
    pub agent: Option<AgentRights>,
}

impl Actor {
    pub const fn person(user: UserId) -> Self {
        Self { user, agent: None }
    }
}

/// A new link's token: 256 random bits, as 43 URL-safe characters.
fn new_token() -> Result<String, AuthError> {
    Ok(URL_SAFE_NO_PAD.encode(crate::crypto::random_bytes::<32>()?.as_ref()))
}

/// What a link is found by.
pub(crate) fn token_hash(token: &str) -> Vec<u8> {
    blake3::hash(token.as_bytes()).as_bytes().to_vec()
}

/// Whether `token` could be a link's token at all (so junk never reaches the database).
pub(crate) fn plausible_token(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn token_context(page: Uuid) -> Vec<u8> {
    let mut c = b"drusite publish token\0".to_vec();
    c.extend(page.as_bytes());
    c
}

/// The public address of a page shared with `token`.
pub fn link(base: &url::Url, token: &str) -> String {
    format!("{}p/{token}", base.as_str())
}

fn millis(t: OffsetDateTime) -> i64 {
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

fn from_millis(ms: i64) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()
}

#[derive(sqlx::FromRow)]
struct Row {
    token: Vec<u8>,
    include_subpages: bool,
    allow_indexing: bool,
    expires_at: Option<OffsetDateTime>,
    has_password: bool,
    published_at: OffsetDateTime,
}

async fn row(conn: &mut PgConnection, page: Uuid) -> Result<Option<Row>, AuthError> {
    Ok(sqlx::query_as(
        "SELECT token, include_subpages, allow_indexing, expires_at,
                password_phc IS NOT NULL AS has_password, published_at
         FROM published_pages WHERE page_id = $1",
    )
    .bind(page)
    .fetch_optional(&mut *conn)
    .await?)
}

fn may_publish(
    state: &AppState,
    actor: Actor,
    role: Option<crate::policy::Role>,
    ws: Uuid,
    page: Uuid,
) -> bool {
    state.config().publish.enabled
        && state.policy().is_allowed(
            &Principal {
                user: actor.user,
                disabled: false,
                role,
                agent: actor.agent,
            },
            Action::PublishDocument,
            &Resource::Document {
                workspace: WorkspaceId::from_uuid(ws),
                document: page,
            },
        )
}

/// The workspace document, as the transaction's user sees it.
async fn workspace(
    state: &AppState,
    conn: &mut PgConnection,
    ws: Uuid,
) -> Result<Workspace, AuthError> {
    let doc = store::load(state, conn, ws)
        .await
        .map_err(AuthError::internal)?
        .filter(|d| d.kind == "workspace")
        .ok_or(AuthError::NotFound)?;
    Workspace::load_all(&doc.parts()).map_err(AuthError::internal)
}

async fn state_of(
    state: &AppState,
    conn: &mut PgConnection,
    ws: Uuid,
    page: Uuid,
    can_publish: bool,
) -> Result<ShareState, AuthError> {
    let base = state.config().publish_base();
    let mut out = ShareState {
        page,
        published: false,
        url: None,
        include_subpages: false,
        allow_indexing: false,
        expires_at: None,
        expired: false,
        has_password: false,
        can_publish,
        published_at: None,
        public_base: base.to_string(),
    };
    if let Some(r) = row(conn, page).await? {
        let key = state
            .keys()
            .version(
                conn,
                WorkspaceId::from_uuid(ws),
                crate::crypto::sealed_key_version(&r.token)?,
            )
            .await
            .map_err(AuthError::internal)?;
        let token = key.decrypt(&token_context(page), &r.token)?;
        let token = String::from_utf8(token).map_err(AuthError::internal)?;
        out.published = true;
        out.url = Some(link(&base, &token));
        out.include_subpages = r.include_subpages;
        out.allow_indexing = r.allow_indexing;
        out.expires_at = r.expires_at.map(millis);
        out.expired = r.expires_at.is_some_and(|e| e <= OffsetDateTime::now_utc());
        out.has_password = r.has_password;
        out.published_at = Some(millis(r.published_at));
    }
    Ok(out)
}

/// Checks the actor's membership and returns their role; a stranger gets "not found".
async fn member(conn: &mut PgConnection, ws: Uuid) -> Result<crate::policy::Role, AuthError> {
    store::role(conn, ws)
        .await
        .map_err(AuthError::internal)?
        .ok_or(AuthError::NotFound)
}

/// How a page is shared, for any member of its workspace.
pub async fn status(
    state: &AppState,
    actor: Actor,
    ws: Uuid,
    page: Uuid,
) -> Result<ShareState, AuthError> {
    let mut tx = state.db().as_user(actor.user).await?;
    let role = member(&mut tx, ws).await?;
    let exists: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM documents WHERE id = $1 AND workspace_id = $2 AND kind = 'page'",
    )
    .bind(page)
    .bind(ws)
    .fetch_optional(&mut *tx)
    .await?;
    let can = exists.is_some() && may_publish(state, actor, Some(role), ws, page);
    let out = state_of(state, &mut tx, ws, page, can).await?;
    tx.commit().await?;
    Ok(out)
}

/// Every published page in a workspace, for any member.
pub async fn list(state: &AppState, actor: Actor, ws: Uuid) -> Result<Vec<ShareState>, AuthError> {
    let mut tx = state.db().as_user(actor.user).await?;
    let role = member(&mut tx, ws).await?;
    let pages: Vec<Uuid> = sqlx::query_scalar(
        "SELECT page_id FROM published_pages WHERE workspace_id = $1 ORDER BY published_at",
    )
    .bind(ws)
    .fetch_all(&mut *tx)
    .await?;
    let mut out = Vec::with_capacity(pages.len());
    for page in pages {
        let can = may_publish(state, actor, Some(role), ws, page);
        out.push(state_of(state, &mut tx, ws, page, can).await?);
    }
    tx.commit().await?;
    Ok(out)
}

/// The checks every change makes: a member who may publish, a page that has reached the server
/// and is in the tree, not the trash. Returns the workspace document.
async fn may_change(
    state: &AppState,
    tx: &mut PgConnection,
    actor: Actor,
    ws: Uuid,
    page: Uuid,
) -> Result<Workspace, AuthError> {
    let role = member(tx, ws).await?;
    if !state.config().publish.enabled {
        return Err(AuthError::Forbidden(
            "publish_off",
            "publishing to the web is turned off on this server",
        ));
    }
    if !may_publish(state, actor, Some(role), ws, page) {
        return Err(AuthError::Forbidden(
            "forbidden",
            if actor.agent.is_some() {
                "this agent isn't allowed to publish pages; its person can allow it when connecting it"
            } else {
                "only the workspace's editors and owners can publish pages"
            },
        ));
    }
    let exists: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM documents WHERE id = $1 AND workspace_id = $2 AND kind = 'page'",
    )
    .bind(page)
    .bind(ws)
    .fetch_optional(&mut *tx)
    .await?;
    if exists.is_none() {
        return Err(AuthError::bad(
            "not_synced",
            "this page hasn't reached the server yet; try again once it has synced",
        ));
    }
    let tree = workspace(state, tx, ws).await?;
    match tree.is_trashed(PageId::from_uuid(page)) {
        Ok(false) => Ok(tree),
        Ok(true) => Err(AuthError::bad(
            "in_trash",
            "this page is in the trash; restore it to publish it",
        )),
        Err(_) => Err(AuthError::bad(
            "not_synced",
            "this page isn't in the workspace on the server yet",
        )),
    }
}

/// Writes the share state into the workspace document as the actor, so every device shows it.
async fn mirror(
    state: &AppState,
    tx: &mut PgConnection,
    actor: Actor,
    ws: Uuid,
    mut tree: Workspace,
    page: Uuid,
    share: &ShareState,
) -> Result<bool, AuthError> {
    let id = PageId::from_uuid(page);
    let wanted = share.url.as_ref().map(|url| ShareMirror {
        url: url.clone(),
        include_subpages: share.include_subpages,
        allow_indexing: share.allow_indexing,
        expires_at: share.expires_at,
        has_password: share.has_password,
    });
    if tree.share(id).map_err(AuthError::internal)? == wanted {
        return Ok(false);
    }
    let before = tree.version();
    tree.set_share(id, wanted.as_ref())
        .map_err(AuthError::internal)?;
    let update = tree.export_updates(&before).map_err(AuthError::internal)?;
    let (_, compact) =
        store::append_in(state, tx, actor.user, ws, ws, &update, Some(Uuid::now_v7()))
            .await
            .map_err(|e| AuthError::internal(anyhow::anyhow!("{e}")))?;
    Ok(compact)
}

fn check_change(change: &ShareChange) -> Result<(), AuthError> {
    if let Some(Some(at)) = change.expires_at
        && from_millis(at).is_none_or(|t| t <= OffsetDateTime::now_utc())
    {
        return Err(AuthError::bad(
            "bad_expiry",
            "the expiry date must be in the future",
        ));
    }
    if let Some(Some(p)) = &change.password {
        let n = p.chars().count();
        if n < MIN_PASSWORD || p.len() > MAX_PASSWORD {
            return Err(AuthError::bad(
                "bad_password",
                format!("a page's password needs {MIN_PASSWORD} to {MAX_PASSWORD} characters"),
            ));
        }
    }
    Ok(())
}

/// Publishes a page, or changes how it's published.
pub async fn publish(
    state: &AppState,
    actor: Actor,
    ws: Uuid,
    page: Uuid,
    change: ShareChange,
) -> Result<ShareState, AuthError> {
    check_change(&change)?;
    let phc = match &change.password {
        Some(Some(p)) => Some(Some(crate::auth::password::hash_page_password(p).await?)),
        Some(None) => Some(None),
        None => None,
    };
    let mut tx = state.db().as_user(actor.user).await?;
    let tree = may_change(state, &mut tx, actor, ws, page).await?;
    let existing = row(&mut tx, page).await?;
    if existing.is_none() {
        let token = new_token()?;
        let key = state
            .keys()
            .current(&mut tx, WorkspaceId::from_uuid(ws))
            .await
            .map_err(AuthError::internal)?;
        let sealed = key.encrypt(&token_context(page), token.as_bytes())?;
        sqlx::query(
            "INSERT INTO published_pages (page_id, workspace_id, token_hash, token, published_by)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(page)
        .bind(ws)
        .bind(token_hash(&token))
        .bind(sealed)
        .bind(actor.user.as_uuid())
        .execute(&mut *tx)
        .await?;
        tracing::info!(%ws, %page, "page published");
    }
    sqlx::query(
        "UPDATE published_pages SET
             include_subpages = coalesce($2, include_subpages),
             allow_indexing = coalesce($3, allow_indexing),
             expires_at = CASE WHEN $4 THEN $5 ELSE expires_at END,
             password_phc = CASE WHEN $6 THEN $7 ELSE password_phc END,
             secret_version = secret_version + CASE WHEN $6 THEN 1 ELSE 0 END,
             failed_unlocks = CASE WHEN $6 THEN 0 ELSE failed_unlocks END,
             updated_at = now()
         WHERE page_id = $1",
    )
    .bind(page)
    .bind(change.include_subpages)
    .bind(change.allow_indexing)
    .bind(change.expires_at.is_some())
    .bind(change.expires_at.flatten().and_then(from_millis))
    .bind(phc.is_some())
    .bind(phc.flatten())
    .execute(&mut *tx)
    .await?;
    let out = state_of(state, &mut tx, ws, page, true).await?;
    let compact = mirror(state, &mut tx, actor, ws, tree, page, &out).await?;
    tx.commit().await?;
    if compact {
        state.hub().request_compaction(ws);
    }
    Ok(out)
}

/// Gives a published page a new link. The old one stops working at once, and so do readers'
/// password cookies.
pub async fn regenerate(
    state: &AppState,
    actor: Actor,
    ws: Uuid,
    page: Uuid,
) -> Result<ShareState, AuthError> {
    let mut tx = state.db().as_user(actor.user).await?;
    let tree = may_change(state, &mut tx, actor, ws, page).await?;
    if row(&mut tx, page).await?.is_none() {
        return Err(AuthError::bad("not_published", "this page isn't published"));
    }
    let token = new_token()?;
    let key = state
        .keys()
        .current(&mut tx, WorkspaceId::from_uuid(ws))
        .await
        .map_err(AuthError::internal)?;
    let sealed = key.encrypt(&token_context(page), token.as_bytes())?;
    sqlx::query(
        "UPDATE published_pages SET token_hash = $2, token = $3,
             secret_version = secret_version + 1, failed_unlocks = 0, updated_at = now()
         WHERE page_id = $1",
    )
    .bind(page)
    .bind(token_hash(&token))
    .bind(sealed)
    .execute(&mut *tx)
    .await?;
    let out = state_of(state, &mut tx, ws, page, true).await?;
    let compact = mirror(state, &mut tx, actor, ws, tree, page, &out).await?;
    tx.commit().await?;
    if compact {
        state.hub().request_compaction(ws);
    }
    tracing::info!(%ws, %page, "published page given a new link");
    Ok(out)
}

/// Takes a page off the web at once. Allowed for a page in the trash too.
pub async fn unpublish(
    state: &AppState,
    actor: Actor,
    ws: Uuid,
    page: Uuid,
) -> Result<ShareState, AuthError> {
    let mut tx = state.db().as_user(actor.user).await?;
    let role = member(&mut tx, ws).await?;
    // With publishing turned off on the server, editors can still take pages down.
    let allowed = if state.config().publish.enabled {
        may_publish(state, actor, Some(role), ws, page)
    } else {
        actor.agent.is_none()
            && matches!(
                role,
                crate::policy::Role::Owner | crate::policy::Role::Editor
            )
    };
    if !allowed {
        return Err(AuthError::Forbidden(
            "forbidden",
            "only the workspace's editors and owners can unpublish pages",
        ));
    }
    sqlx::query("DELETE FROM published_pages WHERE page_id = $1 AND workspace_id = $2")
        .bind(page)
        .bind(ws)
        .execute(&mut *tx)
        .await?;
    let out = state_of(state, &mut tx, ws, page, true).await?;
    let tree = workspace(state, &mut tx, ws).await?;
    let compact = if tree.share(PageId::from_uuid(page)).is_ok() {
        mirror(state, &mut tx, actor, ws, tree, page, &out).await?
    } else {
        false
    };
    tx.commit().await?;
    if compact {
        state.hub().request_compaction(ws);
    }
    tracing::info!(%ws, %page, "page unpublished");
    Ok(out)
}

async fn get_share(
    State(state): State<AppState>,
    auth: Authenticated,
    Path((ws, page)): Path<(Uuid, Uuid)>,
) -> Result<Json<ShareState>, AuthError> {
    Ok(Json(
        status(&state, Actor::person(auth.user), ws, page).await?,
    ))
}

fn writable(auth: &Authenticated) -> Result<(), AuthError> {
    if auth.can_write() {
        Ok(())
    } else {
        Err(AuthError::Forbidden(
            "read_only",
            "this sign-in can't change notes",
        ))
    }
}

async fn put_share(
    State(state): State<AppState>,
    auth: Authenticated,
    Path((ws, page)): Path<(Uuid, Uuid)>,
    Json(change): Json<ShareChange>,
) -> Result<Json<ShareState>, AuthError> {
    writable(&auth)?;
    Ok(Json(
        publish(&state, Actor::person(auth.user), ws, page, change).await?,
    ))
}

async fn delete_share(
    State(state): State<AppState>,
    auth: Authenticated,
    Path((ws, page)): Path<(Uuid, Uuid)>,
) -> Result<Json<ShareState>, AuthError> {
    writable(&auth)?;
    Ok(Json(
        unpublish(&state, Actor::person(auth.user), ws, page).await?,
    ))
}

async fn regenerate_share(
    State(state): State<AppState>,
    auth: Authenticated,
    Path((ws, page)): Path<(Uuid, Uuid)>,
) -> Result<Json<ShareState>, AuthError> {
    writable(&auth)?;
    Ok(Json(
        regenerate(&state, Actor::person(auth.user), ws, page).await?,
    ))
}

async fn list_shares(
    State(state): State<AppState>,
    auth: Authenticated,
    Path(ws): Path<Uuid>,
) -> Result<Json<Vec<ShareState>>, AuthError> {
    Ok(Json(list(&state, Actor::person(auth.user), ws).await?))
}
