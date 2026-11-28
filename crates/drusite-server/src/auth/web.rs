// SPDX-License-Identifier: AGPL-3.0-or-later

//! The web app's sign-in: a backend-for-frontend. The browser never holds a token.
//!
//! 1. `/web/login` makes a PKCE verifier and keeps it here (bound to a nonce cookie), then sends
//!    the browser to this server's own `/oauth/authorize` as the built-in client
//!    [`clients::WEB`], so the usual sign-in page runs with every method the server has on.
//! 2. `/web/callback` gets the code, redeems it in-process, and makes a grant like a device's
//!    (listed under Devices, revocable, with the browser's own idle and absolute limits).
//! 3. The browser gets one cookie for that grant: HttpOnly, SameSite=Strict, `__Host-` and
//!    Secure on https. [`Authenticated`](super::Authenticated) takes it on every API route, with
//!    the origin checks in [`same_origin`]; that's what stands in for DPoP here.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use drusite_model::UserId;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use time::Duration;
use uuid::Uuid;

use super::error::{AuthError, AuthResult};
use super::secret::{self, Kind};
use super::tokens::GrantId;
use super::{clients, oauth, pages, session, tokens, users};
use crate::app::AppState;

const LOGIN_KIND: &str = "web_login";
const LOGIN_TTL: Duration = Duration::minutes(15);

fn secure(state: &AppState) -> bool {
    state.config().public_url.scheme() == "https"
}

/// The session cookie's name: `__Host-` where the browser enforces it (https).
pub fn cookie_name(state: &AppState) -> &'static str {
    if secure(state) {
        "__Host-drusite_web"
    } else {
        "drusite_web"
    }
}

/// The cookie that ties a sign-in's start to its end, in the same browser. Lax: the end can
/// arrive at the end of redirects that started at another site (an OIDC provider's).
fn nonce_name(state: &AppState) -> &'static str {
    if secure(state) {
        "__Host-drusite_web_login"
    } else {
        "drusite_web_login"
    }
}

fn cookie(state: &AppState, name: &str, value: &str, same_site: &str, max_age: i64) -> HeaderValue {
    let secure = if secure(state) { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite={same_site}{secure}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// Where the web app's sign-in comes back to: this server, and only this server.
pub fn callback_url(state: &AppState) -> String {
    format!(
        "{}/web/callback",
        state.config().public_url.as_str().trim_end_matches('/')
    )
}

/// The routes, when the server has a web app to serve.
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/web/login", get(login))
        .route("/web/callback", get(callback))
        .route("/api/v1/web/session", get(current))
        .route("/api/v1/web/logout", post(logout))
}

/// Whether a request carrying the web cookie comes from the web app itself. The cookie is
/// SameSite=Strict, so no other site's request carries it; but sibling subdomains are the "same
/// site", so the origin must be this server's exactly. Browsers send `Origin` on every request
/// that isn't a plain navigation (WebSocket upgrades included), and Fetch Metadata on all of
/// them.
pub fn same_origin(state: &AppState, headers: &HeaderMap) -> bool {
    let ours = state.config().public_url.origin().ascii_serialization();
    let origin_ok = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|o| o == ours);
    let site_ok = !matches!(
        headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()),
        Some("cross-site" | "same-site")
    );
    origin_ok && site_ok
}

/// A web cookie's grant.
#[derive(Debug, Clone)]
pub enum Lookup {
    Live {
        user: UserId,
        grant: GrantId,
        scope: String,
    },
    /// The grant was revoked: signed out from another device, or the account page.
    SignedOut,
    /// No cookie, an unknown one, or one that ran out.
    None,
}

/// Finds the grant behind the request's web cookie, and keeps it alive (no more than hourly).
pub async fn lookup(state: &AppState, headers: &HeaderMap) -> AuthResult<Lookup> {
    let Some(hash) =
        session::read_cookie(headers, cookie_name(state)).and_then(|t| secret::hash(Kind::Web, t))
    else {
        return Ok(Lookup::None);
    };
    type Row = (Uuid, Uuid, String, bool, bool, bool);
    let row: Option<Row> = sqlx::query_as(
        "SELECT g.id, g.user_id, g.scope, g.revoked_at IS NOT NULL,
                g.idle_expires_at > now() AND g.absolute_expires_at > now()
                    AND u.disabled_at IS NULL,
                g.last_used_at < now() - interval '1 hour'
         FROM web_sessions w
         JOIN oauth_grants g ON g.id = w.grant_id
         JOIN users u ON u.id = g.user_id
         WHERE w.token_hash = $1 AND g.client_id = $2",
    )
    .bind(hash.as_slice())
    .bind(clients::WEB)
    .fetch_optional(state.db().pool())
    .await?;
    let Some((grant, user, scope, revoked, live, stale)) = row else {
        return Ok(Lookup::None);
    };
    if revoked {
        return Ok(Lookup::SignedOut);
    }
    if !live {
        return Ok(Lookup::None);
    }
    if stale {
        let idle = i64::from(state.config().auth.browser_idle_days);
        sqlx::query(
            "UPDATE oauth_grants SET last_used_at = now(),
                 idle_expires_at = least(now() + make_interval(days => $2::int), absolute_expires_at)
             WHERE id = $1",
        )
        .bind(grant)
        .bind(i32::try_from(idle).unwrap_or(i32::MAX))
        .execute(state.db().pool())
        .await?;
    }
    Ok(Lookup::Live {
        user: UserId::from_uuid(user),
        grant: GrantId(grant),
        scope,
    })
}

#[derive(Serialize, Deserialize)]
struct Pending {
    verifier: String,
}

/// Starts signing the browser in.
async fn login(State(state): State<AppState>) -> AuthResult<Response> {
    let verifier = URL_SAFE_NO_PAD.encode(crate::crypto::random_bytes::<32>()?.as_ref());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let nonce = URL_SAFE_NO_PAD.encode(crate::crypto::random_bytes::<32>()?.as_ref());
    let id = session::put_challenge(
        &state,
        LOGIN_KIND,
        &Pending { verifier },
        Some(&nonce),
        LOGIN_TTL,
    )
    .await?;
    let mut authorize = url::Url::parse(&format!(
        "{}{}",
        state.config().public_url.as_str().trim_end_matches('/'),
        oauth::AUTHORIZE_PATH
    ))
    .map_err(AuthError::internal)?;
    authorize
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", clients::WEB)
        .append_pair("redirect_uri", &callback_url(&state))
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("scope", "read write")
        .append_pair("state", &id.to_string());
    Ok((
        [
            (
                header::SET_COOKIE,
                cookie(
                    &state,
                    nonce_name(&state),
                    &nonce,
                    "Lax",
                    LOGIN_TTL.whole_seconds(),
                ),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        Redirect::to(authorize.as_str()),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn failed(message: &str) -> Response {
    let body = format!(
        "{} <a href=\"/web/login\">Sign in again</a>.",
        pages::escape(message)
    );
    (
        StatusCode::BAD_REQUEST,
        pages::security_headers(),
        Html(pages::message_page_html("Can't sign in", &body)),
    )
        .into_response()
}

/// What to call a browser on the Devices list: "Firefox on macOS".
pub fn device_name(headers: &HeaderMap) -> String {
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let browser = if ua.contains("Firefox/") {
        "Firefox"
    } else if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("Chrome/") || ua.contains("Chromium/") {
        "Chrome"
    } else if ua.contains("Safari/") {
        "Safari"
    } else {
        "A browser"
    };
    let system = if ua.contains("Android") {
        Some("Android")
    } else if ua.contains("iPhone") || ua.contains("iPad") {
        Some("iOS")
    } else if ua.contains("Mac OS X") || ua.contains("Macintosh") {
        Some("macOS")
    } else if ua.contains("Windows") {
        Some("Windows")
    } else if ua.contains("CrOS") {
        Some("ChromeOS")
    } else if ua.contains("Linux") {
        Some("Linux")
    } else {
        None
    };
    match system {
        Some(s) => format!("{browser} on {s}"),
        None => browser.to_owned(),
    }
}

/// Finishes signing the browser in: the cookie, and back to the app.
async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    if let Some(error) = q.error {
        let message = q.error_description.unwrap_or(error);
        return failed(&format!("Signing in didn't finish ({message})."));
    }
    let (Some(code), Some(id)) = (q.code, q.state.and_then(|s| s.parse::<Uuid>().ok())) else {
        return failed("The sign-in came back without what it needs.");
    };
    // Checked before the pending sign-in is taken (which spends it), so a request from another
    // browser can't use it up.
    let Some(nonce) = session::read_cookie(&headers, nonce_name(&state)) else {
        return failed("Finish signing in in the browser you started in.");
    };
    let pending: Pending = match session::take_challenge(&state, id, LOGIN_KIND, Some(nonce)).await
    {
        Ok(p) => p,
        Err(AuthError::BadRequest(_, message)) => return failed(&message),
        Err(e) => return e.into_response(),
    };
    let grant = match oauth::redeem_web_code(
        &state,
        &code,
        &pending.verifier,
        &device_name(&headers),
    )
    .await
    {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };
    let result: AuthResult<HeaderValue> = async {
        let token = secret::generate(Kind::Web)?;
        let hash = secret::hash(Kind::Web, &token)
            .ok_or_else(|| AuthError::internal(anyhow::anyhow!("a new cookie didn't hash")))?;
        sqlx::query("INSERT INTO web_sessions (token_hash, grant_id) VALUES ($1, $2)")
            .bind(hash.as_slice())
            .bind(grant.0)
            .execute(state.db().pool())
            .await?;
        let days = i64::from(state.config().auth.browser_max_days);
        Ok(cookie(
            &state,
            cookie_name(&state),
            &token,
            "Strict",
            days * 86_400,
        ))
    }
    .await;
    match result {
        Ok(set) => {
            tracing::info!(grant = %grant.0, "browser signed in");
            let mut response = Redirect::to("/").into_response();
            let h = response.headers_mut();
            h.append(header::SET_COOKIE, set);
            h.append(
                header::SET_COOKIE,
                cookie(&state, nonce_name(&state), "", "Lax", 0),
            );
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(e) => e.into_response(),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Current {
    email: String,
    server: String,
    server_version: &'static str,
}

/// Who this browser is signed in as; 401 `signed_out` if the server signed it out, else 401
/// `not_signed_in`.
async fn current(State(state): State<AppState>, headers: HeaderMap) -> AuthResult<Response> {
    if !same_origin(&state, &headers) {
        return Err(AuthError::Forbidden(
            "cross_origin",
            "requests from other sites aren't allowed",
        ));
    }
    match lookup(&state, &headers).await? {
        Lookup::Live { user, .. } => {
            let account = users::by_id(&state, user)
                .await?
                .ok_or(AuthError::NotFound)?;
            Ok((
                [(header::CACHE_CONTROL, "no-store")],
                Json(Current {
                    email: account.email,
                    server: state.config().public_url.to_string(),
                    server_version: env!("CARGO_PKG_VERSION"),
                }),
            )
                .into_response())
        }
        Lookup::SignedOut => Err(AuthError::Unauthorized(
            "signed_out",
            "this browser was signed out",
        )),
        Lookup::None => Err(AuthError::Unauthorized("not_signed_in", "sign in first")),
    }
}

/// Signs this browser out: its grant, its cookie, and the sign-in pages' session too, so a
/// shared computer is left signed out of everything.
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> AuthResult<Response> {
    if !same_origin(&state, &headers) {
        return Err(AuthError::Forbidden(
            "cross_origin",
            "requests from other sites aren't allowed",
        ));
    }
    if let Lookup::Live { grant, .. } = lookup(&state, &headers).await? {
        tokens::revoke_grant(&state, grant, "signed out in the browser").await?;
    }
    if let Some(hash) =
        session::read_cookie(&headers, cookie_name(&state)).and_then(|t| secret::hash(Kind::Web, t))
    {
        sqlx::query("DELETE FROM web_sessions WHERE token_hash = $1")
            .bind(hash.as_slice())
            .execute(state.db().pool())
            .await?;
    }
    let pages = session::end(&state, &headers).await?;
    let mut response = Json(serde_json::json!({ "status": "signed_out" })).into_response();
    let h = response.headers_mut();
    h.append(
        header::SET_COOKIE,
        cookie(&state, cookie_name(&state), "", "Strict", 0),
    );
    h.append(header::SET_COOKIE, pages);
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browsers_are_named_for_the_devices_list() {
        let named = |ua: &str| {
            let mut h = HeaderMap::new();
            h.insert(
                header::USER_AGENT,
                HeaderValue::from_str(ua).unwrap_or_else(|_| HeaderValue::from_static("")),
            );
            device_name(&h)
        };
        assert_eq!(
            named(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:140.0) Gecko/20100101 Firefox/140.0"
            ),
            "Firefox on macOS"
        );
        assert_eq!(
            named(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36 Edg/140.0"
            ),
            "Edge on Windows"
        );
        assert_eq!(
            named(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36"
            ),
            "Chrome on Linux"
        );
        assert_eq!(named(""), "A browser");
    }
}
