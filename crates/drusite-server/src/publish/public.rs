// SPDX-License-Identifier: AGPL-3.0-or-later

//! The public host: published pages, their files and their stylesheet, and nothing else.
//!
//! Every link that doesn't lead to a published page (unknown, unpublished, expired, in the
//! trash, a subpage that isn't shared) gets the same 404, byte for byte. Pages are rendered
//! live from the documents, decrypted here as search and Ask do. Responses carry a strict CSP
//! (no script at all), `no-referrer` (so a link's token never leaks to the sites it links to),
//! an ETag with a short `max-age`, and `noindex` unless the page allows search engines.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{ConnectInfo, Form, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use drusite_crdt::{Page, PageNode, Workspace};
use drusite_model::{BlockKind, PageId, WorkspaceId};
use serde::Deserialize;
use sqlx::PgConnection;
use time::OffsetDateTime;
use uuid::Uuid;

use super::render::{self, Links, Nav, PageDoc};
use super::{plausible_token, token_hash};
use crate::app::AppState;
use crate::storage::BlobId;
use crate::sync::store;

const CSS: &str = include_str!("../../assets/publish.css");

/// The stylesheet's address: named by its hash, so it can be cached for good.
static CSS_PATH: LazyLock<String> = LazyLock::new(|| {
    let hash = blake3::hash(CSS.as_bytes()).to_hex();
    format!("/assets/publish.{}.css", &hash[..12])
});

const CSP: &str = "default-src 'none'; style-src 'self'; img-src 'self'; form-action 'self'; \
                   base-uri 'none'; frame-ancestors 'none'";
const FILE_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";
const NOINDEX: &str = "noindex, nofollow, noarchive";

/// Wrong passwords a link takes in [`UNLOCK_WINDOW`] before it refuses more, for everyone.
const UNLOCK_FAILURES: i32 = 20;
const UNLOCK_WINDOW: &str = "15 minutes";
/// A reader's password cookie lasts this long.
const COOKIE_HOURS: i64 = 12;

/// Requests a minute per address, counted on this node.
#[derive(Debug)]
struct Limiter {
    per_minute: u32,
    seen: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl Limiter {
    fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            seen: Mutex::default(),
        }
    }

    /// Counts one request; false when the address is over its limit.
    fn allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let Ok(mut seen) = self.seen.lock() else {
            return true;
        };
        if seen.len() > 50_000 {
            seen.retain(|_, (at, _)| now.duration_since(*at) < Duration::from_secs(60));
        }
        let entry = seen.entry(ip).or_insert((now, 0));
        if now.duration_since(entry.0) >= Duration::from_secs(60) {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.per_minute
    }
}

/// What the public host keeps between requests.
#[derive(Debug)]
pub struct PublicState {
    requests: Limiter,
    unlocks: Limiter,
}

impl PublicState {
    pub fn new(config: &crate::config::Config) -> Self {
        Self {
            requests: Limiter::new(config.publish.per_minute),
            // Password tries per address: far fewer than page views.
            unlocks: Limiter::new(10),
        }
    }
}

/// The public host as a whole: what a separate listener, or a request for the public host name
/// on the main one, gets. Anything that isn't a published page is the one 404.
pub fn public_router(state: AppState) -> Router {
    routes()
        .fallback(fallback)
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// The routes of published pages, to serve on the public host or under `/p/` on the server's
/// own address.
fn routes() -> Router<AppState> {
    Router::new()
        .route("/p/{token}", get(root_page))
        .route("/p/{token}/unlock", axum::routing::post(unlock))
        .route("/p/{token}/{page}", get(subpage))
        .route("/p/{token}/{page}/file/{file}", get(file))
        .route("/assets/{name}", get(stylesheet))
        .route("/robots.txt", get(robots))
        // A known path with the wrong method is as unknown as any other.
        .method_not_allowed_fallback(fallback)
}

/// [`public_router`]'s routes, for the server's own address when there's no public host.
pub(crate) fn public_routes(state: &AppState) -> Router<AppState> {
    routes().route_layer(axum::middleware::from_fn_with_state(state.clone(), guard))
}

async fn fallback() -> Response {
    not_found()
}

/// The reader's address, for the limits.
fn client_ip(state: &AppState, request: &Request) -> IpAddr {
    if let Some(name) = &state.config().publish.client_ip_header
        && let Some(value) = request
            .headers()
            .get(name.as_str())
            .and_then(|v| v.to_str().ok())
        && let Some(ip) = value
            .rsplit(',')
            .next()
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
    {
        return ip;
    }
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |c| c.0.ip())
}

/// Rate limits, then the security headers every public response carries.
async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let ip = client_ip(&state, &request);
    let unlocking =
        request.method() == axum::http::Method::POST && request.uri().path().ends_with("/unlock");
    let limits = state.publish();
    let allowed = limits.requests.allow(ip) && (!unlocking || limits.unlocks.allow(ip));
    let mut response = if allowed {
        next.run(request).await
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            [
                (header::RETRY_AFTER, "60"),
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            "Too many requests. Try again in a minute.\n",
        )
            .into_response()
    };
    let h = response.headers_mut();
    for (name, value) in [
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (
            HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ),
        (
            HeaderName::from_static("cross-origin-resource-policy"),
            "same-origin",
        ),
    ] {
        h.entry(name).or_insert(HeaderValue::from_static(value));
    }
    h.entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(CSP));
    h.entry(HeaderName::from_static("x-robots-tag"))
        .or_insert(HeaderValue::from_static(NOINDEX));
    response
}

/// The one answer for every link that doesn't work.
fn not_found() -> Response {
    static BODY: LazyLock<String> = LazyLock::new(|| render::not_found(&CSS_PATH));
    (
        StatusCode::NOT_FOUND,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        BODY.as_str(),
    )
        .into_response()
}

fn server_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "serving a published page");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        "Something went wrong on the server.\n",
    )
        .into_response()
}

async fn stylesheet(Path(name): Path<String>) -> Response {
    if format!("/assets/{name}") != *CSS_PATH {
        return not_found();
    }
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        CSS,
    )
        .into_response()
}

/// Search engines may crawl published pages; each page says whether it may be indexed
/// (`X-Robots-Tag`, and `<meta name="robots">`), which they only see if they may crawl it.
/// Links are never listed here: that would hand them to anyone.
async fn robots(State(state): State<AppState>) -> Response {
    let body = if state.config().publish.enabled {
        "# Pages shared from Drusite. Each page says whether search engines may index it.\n\
         User-agent: *\nDisallow: /p/*/unlock\nAllow: /p/\nDisallow: /\n"
    } else {
        "User-agent: *\nDisallow: /\n"
    };
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
        .into_response()
}

/// A link that's live, with the workspace as it is now.
struct Live {
    token: String,
    page: Uuid,
    ws: Uuid,
    include_subpages: bool,
    allow_indexing: bool,
    phc: Option<String>,
    secret_version: i32,
    tree: Workspace,
}

#[derive(sqlx::FromRow)]
struct LiveRow {
    page_id: Uuid,
    workspace_id: Uuid,
    include_subpages: bool,
    allow_indexing: bool,
    expires_at: Option<OffsetDateTime>,
    password_phc: Option<String>,
    secret_version: i32,
}

async fn resolve(
    state: &AppState,
    conn: &mut PgConnection,
    token: &str,
) -> anyhow::Result<Option<Live>> {
    if !state.config().publish.enabled || !plausible_token(token) {
        return Ok(None);
    }
    let row: Option<LiveRow> = sqlx::query_as(
        "SELECT page_id, workspace_id, include_subpages, allow_indexing, expires_at,
                password_phc, secret_version
         FROM published_pages WHERE token_hash = $1",
    )
    .bind(token_hash(token))
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row
        .expires_at
        .is_some_and(|e| e <= OffsetDateTime::now_utc())
    {
        return Ok(None);
    }
    let Some(doc) = store::load(state, conn, row.workspace_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .filter(|d| d.kind == "workspace")
    else {
        return Ok(None);
    };
    let tree = Workspace::load_all(&doc.parts())?;
    // In the trash, or deleted for good: gone from the web, the same as never published.
    if tree
        .is_trashed(PageId::from_uuid(row.page_id))
        .unwrap_or(true)
    {
        return Ok(None);
    }
    Ok(Some(Live {
        token: token.to_owned(),
        page: row.page_id,
        ws: row.workspace_id,
        include_subpages: row.include_subpages,
        allow_indexing: row.allow_indexing,
        phc: row.password_phc,
        secret_version: row.secret_version,
        tree,
    }))
}

/// A short name for a page under one link, from the token: says nothing about which page it
/// is, and changes when the link does.
fn page_key(token: &str, page: Uuid) -> String {
    let key = blake3::derive_key("drusite 2026 publish page address", token.as_bytes());
    let mac = blake3::keyed_hash(&key, page.as_bytes());
    URL_SAFE_NO_PAD.encode(&mac.as_bytes()[..12])
}

fn file_key(token: &str, page: Uuid, blob: &str) -> String {
    let key = blake3::derive_key("drusite 2026 publish file address", token.as_bytes());
    let mut h = blake3::Hasher::new_keyed(&key);
    h.update(page.as_bytes());
    h.update(blob.as_bytes());
    URL_SAFE_NO_PAD.encode(&h.finalize().as_bytes()[..12])
}

/// The pages one link shares: its page and, when it includes subpages, every page below it
/// that isn't in the trash.
struct Site {
    root: Uuid,
    nodes: HashMap<Uuid, PageNode>,
    keys: HashMap<Uuid, String>,
}

impl Site {
    fn of(live: &Live) -> Option<Self> {
        let all: HashMap<Uuid, PageNode> = live
            .tree
            .pages()
            .ok()?
            .into_iter()
            .map(|n| (*n.id.as_uuid(), n))
            .collect();
        let mut nodes = HashMap::new();
        let mut stack = vec![live.page];
        while let Some(id) = stack.pop() {
            let Some(node) = all.get(&id) else { continue };
            if live.include_subpages {
                stack.extend(node.children.iter().map(|c| *c.as_uuid()));
            }
            nodes.insert(id, node.clone());
            if nodes.len() > 10_000 {
                break;
            }
        }
        if !nodes.contains_key(&live.page) {
            return None;
        }
        let keys = nodes
            .keys()
            .map(|id| (*id, page_key(&live.token, *id)))
            .collect();
        Some(Self {
            root: live.page,
            nodes,
            keys,
        })
    }

    fn by_key(&self, key: &str) -> Option<Uuid> {
        self.keys
            .iter()
            .find(|(_, k)| bool::from(subtle::ConstantTimeEq::ct_eq(k.as_bytes(), key.as_bytes())))
            .map(|(id, _)| *id)
    }

    fn href(&self, token: &str, page: Uuid) -> Option<String> {
        if page == self.root {
            return Some(format!("/p/{token}"));
        }
        self.keys.get(&page).map(|k| format!("/p/{token}/{k}"))
    }
}

struct SiteLinks<'a> {
    site: &'a Site,
    token: &'a str,
    page: Uuid,
}

impl Links for SiteLinks<'_> {
    fn page(&self, page: PageId) -> Option<String> {
        self.site.href(self.token, *page.as_uuid())
    }

    fn file(&self, blob: &str) -> Option<String> {
        let key = self.site.keys.get(&self.page)?;
        Some(format!(
            "/p/{}/{key}/file/{}",
            self.token,
            file_key(self.token, self.page, blob)
        ))
    }
}

/// The reader's password cookie, if it's good for this link as it is now.
fn cookie_name(token: &str) -> String {
    let h = blake3::hash(token.as_bytes()).to_hex();
    format!("drusite_page_{}", &h[..10])
}

async fn cookie_key(
    state: &AppState,
    conn: &mut PgConnection,
    ws: Uuid,
) -> anyhow::Result<[u8; 32]> {
    let key = state
        .keys()
        .current(conn, WorkspaceId::from_uuid(ws))
        .await?;
    Ok(*key.derive("drusite 2026 publish reader cookie"))
}

fn cookie_mac(key: &[u8; 32], live: &Live, expires: i64) -> String {
    let mut h = blake3::Hasher::new_keyed(key);
    h.update(&token_hash(&live.token));
    h.update(&live.secret_version.to_be_bytes());
    h.update(&expires.to_be_bytes());
    URL_SAFE_NO_PAD.encode(h.finalize().as_bytes())
}

fn has_cookie(headers: &HeaderMap, key: &[u8; 32], live: &Live) -> bool {
    let name = cookie_name(&live.token);
    let now = OffsetDateTime::now_utc().unix_timestamp();
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .filter(|(n, _)| *n == name)
        .any(|(_, value)| {
            let Some((exp, mac)) = value.split_once('.') else {
                return false;
            };
            let Ok(exp) = exp.parse::<i64>() else {
                return false;
            };
            exp > now
                && bool::from(subtle::ConstantTimeEq::ct_eq(
                    cookie_mac(key, live, exp).as_bytes(),
                    mac.as_bytes(),
                ))
        })
}

fn etag_matches(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .map(|t| t.trim().trim_start_matches("W/"))
                .any(|t| t == etag || t == "*")
        })
}

fn html(
    state: &AppState,
    headers: &HeaderMap,
    body: String,
    indexable: bool,
    private: bool,
) -> Response {
    let etag = format!("\"{}\"", &blake3::hash(body.as_bytes()).to_hex()[..32]);
    let cache = if private {
        "private, no-cache".to_owned()
    } else {
        format!(
            "private, max-age={}, must-revalidate",
            state.config().publish.cache_secs
        )
    };
    let mut response = if etag_matches(headers, &etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
    };
    let h = response.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    if let Ok(v) = HeaderValue::from_str(&cache) {
        h.insert(header::CACHE_CONTROL, v);
    }
    // The page differs by cookie when it has a password.
    h.insert(header::VARY, HeaderValue::from_static("Cookie"));
    if indexable {
        h.insert(
            HeaderName::from_static("x-robots-tag"),
            HeaderValue::from_static("all"),
        );
    }
    response
}

fn gate(token: &str, to: Option<&str>, wrong: bool, limited: bool, status: StatusCode) -> Response {
    let action = match to {
        Some(k) => format!("/p/{token}/unlock?to={k}"),
        None => format!("/p/{token}/unlock"),
    };
    let body = render::password_form(&action, &CSS_PATH, wrong, limited);
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

async fn root_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    serve_page(&state, &token, None, &headers).await
}

async fn subpage(
    State(state): State<AppState>,
    Path((token, page)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    serve_page(&state, &token, Some(&page), &headers).await
}

async fn serve_page(
    state: &AppState,
    token: &str,
    key: Option<&str>,
    headers: &HeaderMap,
) -> Response {
    match page_response(state, token, key, headers).await {
        Ok(Some(r)) => r,
        Ok(None) => not_found(),
        Err(e) => server_error(e),
    }
}

async fn page_response(
    state: &AppState,
    token: &str,
    key: Option<&str>,
    headers: &HeaderMap,
) -> anyhow::Result<Option<Response>> {
    let mut tx = state.db().as_system().await?;
    let Some(live) = resolve(state, &mut tx, token).await? else {
        return Ok(None);
    };
    let Some(site) = Site::of(&live) else {
        return Ok(None);
    };
    let target = match key {
        None => live.page,
        Some(k) => match site.by_key(k) {
            Some(id) => id,
            None => return Ok(None),
        },
    };
    if live.phc.is_some() {
        let ck = cookie_key(state, &mut tx, live.ws).await?;
        if !has_cookie(headers, &ck, &live) {
            let to = key.filter(|_| target != live.page);
            return Ok(Some(gate(token, to, false, false, StatusCode::OK)));
        }
    }
    let Some(doc) = store::load(state, &mut tx, target)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .filter(|d| d.kind == "page" && d.workspace == live.ws)
    else {
        return Ok(None);
    };
    tx.commit().await?;
    let blocks = Page::load_all(&doc.parts())?.blocks()?;
    let Some(node) = site.nodes.get(&target) else {
        return Ok(None);
    };
    // The way down from the shared page, and the shared pages just inside this one.
    let mut trail = Vec::new();
    let mut at = node.parent.map(|p| *p.as_uuid());
    while target != live.page
        && let Some(p) = at
    {
        let Some(n) = site.nodes.get(&p) else { break };
        trail.push(Nav {
            title: n.title.clone(),
            href: site.href(token, p).unwrap_or_default(),
        });
        if p == live.page {
            break;
        }
        at = n.parent.map(|q| *q.as_uuid());
    }
    trail.reverse();
    let subpages = node
        .children
        .iter()
        .filter_map(|c| {
            let n = site.nodes.get(c.as_uuid())?;
            Some(Nav {
                title: n.title.clone(),
                href: site.href(token, *c.as_uuid())?,
            })
        })
        .collect();
    let base = state.config().publish_base();
    let origin = base.as_str().trim_end_matches('/').to_owned();
    let href = site.href(token, target).unwrap_or_default();
    let url = format!("{origin}{href}");
    let links = SiteLinks {
        site: &site,
        token,
        page: target,
    };
    let body = render::page(
        &PageDoc {
            title: &node.title,
            kind: node.kind,
            properties: &node.properties,
            blocks: &blocks,
            trail,
            subpages,
            url: &url,
            origin: &origin,
            indexable: live.allow_indexing && live.phc.is_none(),
            stylesheet: &CSS_PATH,
        },
        &links,
    );
    Ok(Some(html(
        state,
        headers,
        body,
        live.allow_indexing && live.phc.is_none(),
        live.phc.is_some(),
    )))
}

#[derive(Deserialize)]
struct UnlockForm {
    #[serde(default)]
    password: String,
}

#[derive(Deserialize)]
struct UnlockQuery {
    to: Option<String>,
}

async fn unlock(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(query): Query<UnlockQuery>,
    Form(form): Form<UnlockForm>,
) -> Response {
    match unlock_response(&state, &token, query.to.as_deref(), &form.password).await {
        Ok(Some(r)) => r,
        Ok(None) => not_found(),
        Err(e) => server_error(e),
    }
}

async fn unlock_response(
    state: &AppState,
    token: &str,
    to: Option<&str>,
    password: &str,
) -> anyhow::Result<Option<Response>> {
    let to = to.filter(|k| {
        (1..=32).contains(&k.len())
            && k.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    });
    let mut tx = state.db().as_system().await?;
    let Some(live) = resolve(state, &mut tx, token).await? else {
        return Ok(None);
    };
    let back = match to {
        Some(k) => format!("/p/{token}/{k}"),
        None => format!("/p/{token}"),
    };
    let Some(phc) = live.phc.clone() else {
        return Ok(Some(see_other(&back, None)));
    };
    // The per-link limit, shared by every replica: a window of wrong tries.
    let failed: i32 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "UPDATE published_pages SET
             failed_unlocks = CASE WHEN unlock_window_start IS NULL
                 OR unlock_window_start < now() - interval '{UNLOCK_WINDOW}' THEN 0
                 ELSE failed_unlocks END,
             unlock_window_start = CASE WHEN unlock_window_start IS NULL
                 OR unlock_window_start < now() - interval '{UNLOCK_WINDOW}' THEN now()
                 ELSE unlock_window_start END
         WHERE page_id = $1 RETURNING failed_unlocks"
    )))
    .bind(live.page)
    .fetch_one(&mut *tx)
    .await?;
    if failed >= UNLOCK_FAILURES {
        tx.commit().await?;
        return Ok(Some(gate(
            token,
            to,
            false,
            true,
            StatusCode::TOO_MANY_REQUESTS,
        )));
    }
    let ok = !password.is_empty()
        && password.len() <= super::MAX_PASSWORD
        && crate::auth::password::verify_page_password(password, phc).await;
    if !ok {
        sqlx::query(
            "UPDATE published_pages SET failed_unlocks = failed_unlocks + 1 WHERE page_id = $1",
        )
        .bind(live.page)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(Some(gate(token, to, true, false, StatusCode::FORBIDDEN)));
    }
    let key = cookie_key(state, &mut tx, live.ws).await?;
    tx.commit().await?;
    let expires = OffsetDateTime::now_utc().unix_timestamp() + COOKIE_HOURS * 3600;
    let secure = state.config().publish_base().scheme() == "https";
    let cookie = format!(
        "{}={expires}.{}; Path=/p/{token}; Max-Age={}; HttpOnly; SameSite=Lax{}",
        cookie_name(token),
        cookie_mac(&key, &live, expires),
        COOKIE_HOURS * 3600,
        if secure { "; Secure" } else { "" },
    );
    Ok(Some(see_other(&back, Some(&cookie))))
}

fn see_other(location: &str, cookie: Option<&str>) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    let h = response.headers_mut();
    if let Ok(v) = HeaderValue::from_str(location) {
        h.insert(header::LOCATION, v);
    }
    if let Some(c) = cookie.and_then(|c| HeaderValue::from_str(c).ok()) {
        h.insert(header::SET_COOKIE, c);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `filename*` for Content-Disposition (RFC 6266, RFC 8187).
fn disposition(kind: &str, name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || ".-_ ".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            encoded.push(char::from(b));
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

async fn file(
    State(state): State<AppState>,
    Path((token, page, file)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    match file_response(&state, &token, &page, &file, &headers).await {
        Ok(Some(r)) => r,
        Ok(None) => not_found(),
        Err(e) => server_error(e),
    }
}

async fn file_response(
    state: &AppState,
    token: &str,
    key: &str,
    file: &str,
    headers: &HeaderMap,
) -> anyhow::Result<Option<Response>> {
    let mut tx = state.db().as_system().await?;
    let Some(live) = resolve(state, &mut tx, token).await? else {
        return Ok(None);
    };
    let Some(site) = Site::of(&live) else {
        return Ok(None);
    };
    let Some(target) = site.by_key(key) else {
        return Ok(None);
    };
    if live.phc.is_some() {
        let ck = cookie_key(state, &mut tx, live.ws).await?;
        if !has_cookie(headers, &ck, &live) {
            return Ok(None);
        }
    }
    let Some(doc) = store::load(state, &mut tx, target)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .filter(|d| d.kind == "page" && d.workspace == live.ws)
    else {
        return Ok(None);
    };
    // Only a file the page shows now: one taken off the page is gone from the web with it.
    let found = Page::load_all(&doc.parts())?
        .blocks()?
        .into_iter()
        .find_map(|b| match b.kind {
            BlockKind::Attachment {
                blob, name, mime, ..
            } if bool::from(subtle::ConstantTimeEq::ct_eq(
                file_key(token, target, &blob).as_bytes(),
                file.as_bytes(),
            )) =>
            {
                Some((blob, name, mime))
            }
            _ => None,
        });
    let Some((blob, name, mime)) = found else {
        return Ok(None);
    };
    let etag = format!("\"{file}\"");
    let cache = if live.phc.is_some() {
        "private, no-cache".to_owned()
    } else {
        format!(
            "private, max-age={}, must-revalidate",
            state.config().publish.cache_secs
        )
    };
    let image = render::is_image(&mime);
    let mut response = if etag_matches(headers, &etag) {
        tx.commit().await?;
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let Some(blob) = BlobId::parse(&blob) else {
            return Ok(None);
        };
        let Some((version, address)) =
            crate::attachments::find_blob(state, &mut tx, live.ws, &blob).await?
        else {
            return Ok(None);
        };
        let key = state
            .keys()
            .version(
                &mut tx,
                WorkspaceId::from_uuid(live.ws),
                u32::try_from(version).unwrap_or(0),
            )
            .await?;
        tx.commit().await?;
        let address = BlobId::parse(&address).ok_or_else(|| anyhow::anyhow!("bad address"))?;
        let bytes = state.blobs().get(&key, &address).await?;
        let content_type = if image {
            mime.to_ascii_lowercase()
        } else {
            "application/octet-stream".to_owned()
        };
        (
            [(header::CONTENT_TYPE, HeaderValue::from_str(&content_type)?)],
            bytes,
        )
            .into_response()
    };
    let h = response.headers_mut();
    h.insert(header::ETAG, HeaderValue::from_str(&etag)?);
    h.insert(header::CACHE_CONTROL, HeaderValue::from_str(&cache)?);
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(FILE_CSP),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition(
            if image { "inline" } else { "attachment" },
            &name,
        ))?,
    );
    if live.allow_indexing && live.phc.is_none() {
        h.insert(
            HeaderName::from_static("x-robots-tag"),
            HeaderValue::from_static("all"),
        );
    }
    Ok(Some(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_depend_on_the_token_and_hide_the_ids() {
        let page = Uuid::now_v7();
        let a = page_key("a".repeat(43).as_str(), page);
        let b = page_key("b".repeat(43).as_str(), page);
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
        assert!(!a.contains(&page.simple().to_string()[..8]));
        assert_ne!(file_key("t", page, "x"), file_key("t", page, "y"));
    }

    #[test]
    fn content_disposition_is_safe_for_any_name() {
        let d = disposition("attachment", "a\"b\r\nc ü.pdf");
        assert!(HeaderValue::from_str(&d).is_ok());
        assert!(d.contains("filename*=UTF-8''a%22b%0D%0Ac%20%C3%BC.pdf"));
        assert!(d.contains("filename=\"a_b__c _.pdf\""));
    }

    #[test]
    fn the_limiter_counts_per_address() {
        let l = Limiter::new(2);
        let a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        assert!(l.allow(a) && l.allow(a));
        assert!(!l.allow(a));
        assert!(l.allow(b));
    }
}
