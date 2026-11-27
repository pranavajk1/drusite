// SPDX-License-Identifier: AGPL-3.0-or-later

//! Drusite's OAuth 2.1 authorization server, for its apps and for agents (MCP clients).
//!
//! An app opens `/oauth/authorize` in the system browser with a PKCE challenge. The browser
//! signs in with whichever methods are on, and comes back to the app's redirect URI with a
//! code. The app swaps the code at `/oauth/token` with a DPoP proof from its device key, and
//! gets an access token and a refresh token bound to that key.
//!
//! An agent does the same with a client ID that is the URL of its metadata document, and asks
//! for the MCP endpoint as its resource (RFC 8707). After signing in, the person sees a consent
//! page and chooses what it may do: which workspaces, read or write, and the journal. Its
//! tokens are good at the MCP endpoint only.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Form, Json};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use time::{Duration, OffsetDateTime};
use url::Url;
use uuid::Uuid;

use super::clients;
use super::dpop::{self, ProofError};
use super::error::{AuthError, AuthResult};
use super::secret::{self, Kind};
use super::session::{self, BrowserSession, Method};
use super::tokens::{self, NewGrant, RefreshError};
use crate::app::AppState;

pub const AUTHORIZE_PATH: &str = "/oauth/authorize";
pub const CONTINUE_PATH: &str = "/oauth/authorize/continue";
pub const TOKEN_PATH: &str = "/oauth/token";
pub const REVOKE_PATH: &str = "/oauth/revoke";
pub const CONSENT_PATH: &str = "/oauth/consent";

const AUTHORIZE_KIND: &str = "authorize";
const CONSENT_KIND: &str = "consent";
/// How long the person has to answer the consent page.
const CONSENT_TTL: Duration = Duration::minutes(15);
/// How long the browser has to sign in.
const AUTHORIZE_TTL: Duration = Duration::minutes(30);
/// How long a code lasts once it's issued.
const CODE_TTL: Duration = Duration::minutes(2);

/// RFC 8414 metadata, so a client can find the endpoints.
pub async fn metadata(State(state): State<AppState>) -> Json<serde_json::Value> {
    let base = state.config().public_url.origin().ascii_serialization();
    Json(serde_json::json!({
        "issuer": issuer(&state),
        "authorization_endpoint": format!("{base}{AUTHORIZE_PATH}"),
        "token_endpoint": format!("{base}{TOKEN_PATH}"),
        "revocation_endpoint": format!("{base}{REVOKE_PATH}"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "revocation_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": clients::SCOPES,
        "dpop_signing_alg_values_supported": ["ES256"],
        "authorization_response_iss_parameter_supported": true,
        "client_id_metadata_document_supported": state.config().agents.enabled,
    }))
}

/// The MCP endpoint's URL: what agents' tokens are for (their resource).
pub fn mcp_resource(state: &AppState) -> String {
    format!("{}{}", issuer(state), crate::mcp::MCP_PATH)
}

/// The issuer identifier: the public URL, without a trailing slash.
pub fn issuer(state: &AppState) -> String {
    state
        .config()
        .public_url
        .as_str()
        .trim_end_matches('/')
        .to_owned()
}

#[derive(Debug, Deserialize)]
pub struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    state: Option<String>,
    scope: Option<String>,
    resource: Option<String>,
    /// RFC 9449 §10: binds the code to the device key from the start.
    dpop_jkt: Option<String>,
    /// `login` makes the browser sign in again even if it already is.
    prompt: Option<String>,
}

/// An authorization request waiting for the browser to sign in.
#[derive(Debug, Serialize, Deserialize)]
pub struct AuthRequest {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    scope: String,
    state: Option<String>,
    dpop_jkt: Option<String>,
    /// For agents: the MCP endpoint, and what to call the agent.
    #[serde(default)]
    resource: Option<String>,
    #[serde(default)]
    client_name: String,
}

/// An agent's request, signed in, waiting for the person's answer on the consent page.
#[derive(Debug, Serialize, Deserialize)]
struct ConsentRequest {
    request: AuthRequest,
    user: drusite_model::UserId,
    method: Method,
}

/// An error shown in the browser, for a request so wrong it can't be sent back to the client.
fn error_page(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Html(super::pages::message_page("Can't sign in", message)),
    )
        .into_response()
}

fn redirect_error(
    state: &AppState,
    redirect_uri: &str,
    client_state: Option<&str>,
    error: &str,
    description: &str,
) -> Response {
    let Ok(mut url) = Url::parse(redirect_uri) else {
        return error_page(description);
    };
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("error", error);
        q.append_pair("error_description", description);
        if let Some(s) = client_state {
            q.append_pair("state", s);
        }
        q.append_pair("iss", &issuer(state));
    }
    see_other(url.as_str())
}

/// A 303 to `location`. axum's `Redirect::to` is a 303 too; this one also forbids caching.
fn see_other(location: &str) -> Response {
    let mut response = Redirect::to(location).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn is_pkce_value(s: &str) -> bool {
    (43..=128).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

pub async fn authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuthorizeQuery>,
) -> Response {
    match authorize_inner(&state, &headers, q).await {
        Ok(response) => response,
        Err(e) => e.into_response(),
    }
}

async fn authorize_inner(
    state: &AppState,
    headers: &HeaderMap,
    q: AuthorizeQuery,
) -> AuthResult<Response> {
    // Until the client and redirect URI check out, errors stay in the browser: sending them
    // to an unchecked URI would make this an open redirector.
    let Some(client_id) = q.client_id.as_deref() else {
        return Ok(error_page(
            "The app asking you to sign in didn't say who it is.",
        ));
    };
    let client = match clients::resolve(state, client_id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::info!(client_id, error = %e, "an unknown client asked to sign in");
            return Ok(error_page(&format!(
                "The app asking you to sign in can't connect: {e}."
            )));
        }
    };
    let Some(redirect_uri) = q
        .redirect_uri
        .filter(|r| clients::redirect_allowed(&client, r))
    else {
        return Ok(error_page(
            "The app gave a return address this server doesn't allow.",
        ));
    };
    let fail = |error: &str, description: &str| {
        Ok(redirect_error(
            state,
            &redirect_uri,
            q.state.as_deref(),
            error,
            description,
        ))
    };
    if q.response_type.as_deref() != Some("code") {
        return fail("unsupported_response_type", "response_type must be code");
    }
    if q.code_challenge_method.as_deref() != Some("S256") {
        return fail(
            "invalid_request",
            "PKCE with code_challenge_method S256 is required",
        );
    }
    let Some(code_challenge) = q
        .code_challenge
        .filter(|c| c.len() == 43 && is_pkce_value(c))
    else {
        return fail("invalid_request", "code_challenge is missing or malformed");
    };
    let Some(scope) = clients::normalize_scope(q.scope.as_deref()) else {
        return fail("invalid_scope", "unknown scope");
    };
    let resource = if client.is_agent() {
        // MCP clients must say which server their tokens are for (RFC 8707).
        match q.resource.as_deref().map(|r| r.trim_end_matches('/')) {
            Some(r) if r == mcp_resource(state) => Some(r.to_owned()),
            _ => {
                return fail(
                    "invalid_target",
                    &format!("resource must be {}", mcp_resource(state)),
                );
            }
        }
    } else {
        if let Some(resource) = &q.resource
            && resource.trim_end_matches('/') != issuer(state)
        {
            return fail("invalid_target", "resource must be this server");
        }
        None
    };
    if q.dpop_jkt.as_ref().is_some_and(|j| j.len() != 43) {
        return fail("invalid_request", "dpop_jkt is malformed");
    }
    if q.state.as_ref().is_some_and(|s| s.len() > 512) {
        return fail("invalid_request", "state is too long");
    }

    let request = AuthRequest {
        client_id: client.id.clone(),
        redirect_uri,
        code_challenge,
        scope,
        state: q.state,
        dpop_jkt: q.dpop_jkt,
        resource,
        client_name: client.label(),
    };
    let fresh_login = q.prompt.as_deref() == Some("login");
    if !fresh_login && let Some(browser) = session::current(state, headers).await? {
        return signed_in(state, request, browser).await;
    }
    let id = session::put_challenge(state, AUTHORIZE_KIND, &request, None, AUTHORIZE_TTL).await?;
    Ok(see_other(&format!("/login?request={id}")))
}

#[derive(Debug, Deserialize)]
pub struct ContinueQuery {
    request: Uuid,
}

/// Where the sign-in page sends the browser once it's signed in.
pub async fn resume(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ContinueQuery>,
) -> Response {
    let result = async {
        let Some(browser) = session::current(&state, &headers).await? else {
            return Ok(see_other(&format!("/login?request={}", q.request)));
        };
        let request: AuthRequest =
            session::take_challenge(&state, q.request, AUTHORIZE_KIND, None).await?;
        signed_in(&state, request, browser).await
    }
    .await;
    match result {
        Ok(r) => r,
        Err(AuthError::BadRequest("expired", _)) => error_page(
            "This sign-in has expired or was already used. Go back to the app and start again.",
        ),
        Err(e) => e.into_response(),
    }
}

/// Checks that an authorization request is waiting, for the sign-in page to show which app
/// is asking.
pub async fn request_info(state: &AppState, id: Uuid) -> AuthResult<String> {
    let request: AuthRequest = session::peek_challenge(state, id, AUTHORIZE_KIND).await?;
    Ok(if request.client_name.is_empty() {
        clients::find(&request.client_id).map_or_else(|| "an app".to_owned(), |c| c.name)
    } else {
        request.client_name
    })
}

/// The browser is signed in: the app gets its code, and an agent's request goes to the
/// consent page first.
async fn signed_in(
    state: &AppState,
    request: AuthRequest,
    browser: BrowserSession,
) -> AuthResult<Response> {
    if request.resource.is_none() {
        return Ok(see_other(
            issue_code(state, &request, browser, None).await?.as_str(),
        ));
    }
    let consent = ConsentRequest {
        request,
        user: browser.user,
        method: browser.method,
    };
    let id = session::put_challenge(state, CONSENT_KIND, &consent, None, CONSENT_TTL).await?;
    Ok(see_other(&format!("{CONSENT_PATH}?request={id}")))
}

async fn issue_code(
    state: &AppState,
    request: &AuthRequest,
    browser: BrowserSession,
    access: Option<&drusite_mcp::Access>,
) -> AuthResult<Url> {
    let code = secret::generate(Kind::Code)?;
    let hash = secret::hash(Kind::Code, &code)
        .ok_or_else(|| AuthError::internal(anyhow::anyhow!("a new code didn't hash")))?;
    sqlx::query(
        "INSERT INTO oauth_codes (code_hash, user_id, client_id, redirect_uri, code_challenge,
             scope, dpop_jkt, auth_method, expires_at, resource, access, client_name)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(hash.as_slice())
    .bind(browser.user.as_uuid())
    .bind(&request.client_id)
    .bind(&request.redirect_uri)
    .bind(&request.code_challenge)
    .bind(&request.scope)
    .bind(&request.dpop_jkt)
    .bind(browser.method.as_str())
    .bind(OffsetDateTime::now_utc() + CODE_TTL)
    .bind(&request.resource)
    .bind(access.map(serde_json::to_value).transpose()?)
    .bind(&request.client_name)
    .execute(state.db().pool())
    .await?;
    let mut url = Url::parse(&request.redirect_uri).map_err(AuthError::internal)?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("code", &code);
        if let Some(s) = &request.state {
            q.append_pair("state", s);
        }
        q.append_pair("iss", &issuer(state));
    }
    Ok(url)
}

/// An RFC 6749 §5.2 error from the token endpoint.
#[derive(Debug)]
pub struct OAuthError {
    status: StatusCode,
    error: &'static str,
    description: String,
}

impl OAuthError {
    fn new(error: &'static str, description: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error,
            description: description.into(),
        }
    }

    fn invalid_grant() -> Self {
        Self::new("invalid_grant", "the code or refresh token isn't valid")
    }
}

impl From<AuthError> for OAuthError {
    fn from(e: AuthError) -> Self {
        let response = e.into_response();
        // Logged by AuthError; the client only learns it was the server.
        Self {
            status: response.status(),
            error: "server_error",
            description: "something went wrong on the server".to_owned(),
        }
    }
}

impl From<sqlx::Error> for OAuthError {
    fn from(e: sqlx::Error) -> Self {
        AuthError::from(e).into()
    }
}

impl From<ProofError> for OAuthError {
    fn from(e: ProofError) -> Self {
        match e {
            ProofError::Db(e) => e.into(),
            other => Self::new("invalid_dpop_proof", other.to_string()),
        }
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "error": self.error,
                "error_description": self.description,
            })),
        )
            .into_response()
    }
}

#[derive(Debug, Deserialize)]
pub struct TokenForm {
    grant_type: String,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    client_id: Option<String>,
    refresh_token: Option<String>,
    /// Not standard: a name for the device, shown on the account page ("Pranava's Mac").
    device_name: Option<String>,
    /// RFC 8707.
    resource: Option<String>,
}

pub async fn token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<TokenForm>,
) -> Response {
    let result = match form.grant_type.as_str() {
        "authorization_code" => exchange_code(&state, &headers, form).await,
        "refresh_token" => refresh(&state, &headers, form).await,
        _ => Err(OAuthError::new(
            "unsupported_grant_type",
            "grant_type must be authorization_code or refresh_token",
        )),
    };
    match result {
        Ok(tokens) => (
            [
                (header::CACHE_CONTROL, "no-store"),
                (header::PRAGMA, "no-cache"),
            ],
            Json(tokens),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

async fn exchange_code(
    state: &AppState,
    headers: &HeaderMap,
    form: TokenForm,
) -> Result<tokens::TokenResponse, OAuthError> {
    let (Some(code), Some(redirect_uri), Some(verifier), Some(client_id)) = (
        form.code,
        form.redirect_uri,
        form.code_verifier,
        form.client_id,
    ) else {
        return Err(OAuthError::new(
            "invalid_request",
            "code, redirect_uri, code_verifier and client_id are required",
        ));
    };
    let client = clients::resolve(state, &client_id)
        .await
        .map_err(|e| OAuthError::new("invalid_client", e.to_string()))?;
    // The app always proves its device key; an agent may.
    let jkt = if client.is_agent() && !headers.contains_key("dpop") {
        None
    } else {
        Some(
            dpop::check(state, headers, "POST", TOKEN_PATH, None)
                .await?
                .jkt,
        )
    };
    let hash = secret::hash(Kind::Code, &code).ok_or_else(OAuthError::invalid_grant)?;

    let mut tx = state.db().pool().begin().await?;
    type CodeRow = (
        uuid::Uuid,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        bool,
        Option<OffsetDateTime>,
        Option<Uuid>,
        Option<String>,
        Option<serde_json::Value>,
        String,
    );
    let row: Option<CodeRow> = sqlx::query_as(
        "SELECT user_id, client_id, redirect_uri, code_challenge, scope, dpop_jkt, auth_method,
                expires_at > now(), used_at, grant_id, resource, access, client_name
         FROM oauth_codes WHERE code_hash = $1 FOR UPDATE",
    )
    .bind(hash.as_slice())
    .fetch_optional(&mut *tx)
    .await?;
    let Some((
        user,
        code_client,
        code_redirect,
        challenge,
        scope,
        code_jkt,
        method,
        live,
        used_at,
        grant,
        resource,
        access,
        client_name,
    )) = row
    else {
        return Err(OAuthError::invalid_grant());
    };
    if used_at.is_some() {
        // RFC 9700 §4.5: a code used twice was stolen. Revoke what the first use got.
        tx.rollback().await?;
        if let Some(grant) = grant {
            tokens::revoke_grant(state, tokens::GrantId(grant), "authorization code reused")
                .await?;
        }
        return Err(OAuthError::invalid_grant());
    }
    let challenge_ok = is_pkce_value(&verifier)
        && URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == challenge;
    if !live || code_client != client_id || code_redirect != redirect_uri || !challenge_ok {
        return Err(OAuthError::invalid_grant());
    }
    if code_jkt.is_some_and(|j| Some(j) != jkt) {
        return Err(OAuthError::new(
            "invalid_dpop_proof",
            "the proof is signed by another key than the code is bound to",
        ));
    }
    if let Some(asked) = &form.resource
        && Some(asked.trim_end_matches('/')) != resource.as_deref()
    {
        return Err(OAuthError::new(
            "invalid_target",
            "the resource isn't the one the code was issued for",
        ));
    }
    let method: Method =
        serde_json::from_value(serde_json::Value::String(method)).map_err(AuthError::from)?;
    let (grant, tokens) = tokens::create_grant(
        state,
        &mut tx,
        &NewGrant {
            user: drusite_model::UserId::from_uuid(user),
            client_id: &client_id,
            scope,
            dpop_jkt: jkt.as_deref(),
            method,
            device_name: form.device_name.as_deref().unwrap_or_default(),
            resource: resource.as_deref(),
            access,
            client_name: &client_name,
        },
    )
    .await?;
    sqlx::query("UPDATE oauth_codes SET used_at = now(), grant_id = $2 WHERE code_hash = $1")
        .bind(hash.as_slice())
        .bind(grant.0)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if resource.is_some() {
        tracing::info!(grant = %grant.0, %user, client = %client_id, "agent connected");
    } else {
        tracing::info!(grant = %grant.0, %user, "device signed in");
    }
    Ok(tokens)
}

async fn refresh(
    state: &AppState,
    headers: &HeaderMap,
    form: TokenForm,
) -> Result<tokens::TokenResponse, OAuthError> {
    let Some(refresh_token) = form.refresh_token else {
        return Err(OAuthError::new(
            "invalid_request",
            "refresh_token is required",
        ));
    };
    let jkt = if headers.contains_key("dpop") {
        Some(
            dpop::check(state, headers, "POST", TOKEN_PATH, None)
                .await?
                .jkt,
        )
    } else {
        None
    };
    tokens::refresh(state, &refresh_token, jkt.as_deref())
        .await
        .map_err(|e| match e {
            RefreshError::Invalid => OAuthError::invalid_grant(),
            RefreshError::WrongKey => OAuthError::new(
                "invalid_dpop_proof",
                "the proof is signed by another key than the token is bound to",
            ),
            RefreshError::Server(e) => e.into(),
        })
}

#[derive(Debug, Deserialize)]
pub struct RevokeForm {
    token: String,
}

/// RFC 7009. Signs the device out: the whole grant goes, not just the one token.
pub async fn revoke(State(state): State<AppState>, Form(form): Form<RevokeForm>) -> Response {
    match tokens::revoke_by_token(&state, &form.token).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => OAuthError::from(e).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct ConsentQuery {
    request: Uuid,
}

/// The consent request `id`, if it is this browser's.
async fn consent_for(
    state: &AppState,
    headers: &HeaderMap,
    id: Uuid,
    take: bool,
) -> AuthResult<Option<(ConsentRequest, BrowserSession)>> {
    let Some(browser) = session::current(state, headers).await? else {
        return Ok(None);
    };
    let consent: ConsentRequest = if take {
        session::take_challenge(state, id, CONSENT_KIND, None).await?
    } else {
        session::peek_challenge(state, id, CONSENT_KIND).await?
    };
    Ok((consent.user == browser.user).then_some((consent, browser)))
}

/// `GET /oauth/consent`: what an agent asks for, and the person's choices.
pub async fn consent_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ConsentQuery>,
) -> Response {
    let result = async {
        let Some((consent, _)) = consent_for(&state, &headers, q.request, false).await? else {
            return Ok(error_page(
                "Sign in as the account you started with, then try connecting again.",
            ));
        };
        let spaces = crate::sync::store::workspaces(&state, consent.user)
            .await
            .map_err(|e| AuthError::internal(anyhow::anyhow!("{e}")))?;
        let mut choices = String::new();
        for (n, (ws, role)) in spaces.iter().enumerate() {
            let label = describe_workspace(&state, consent.user, *ws, n, spaces.len()).await;
            let read_only = if role == "viewer" {
                " (you can only read it)"
            } else {
                ""
            };
            choices.push_str(&format!(
                "<label class=\"choice\"><input type=\"checkbox\" name=\"workspace\" \
                 value=\"{ws}\" checked> {}{read_only}</label>",
                super::pages::escape(&label),
            ));
        }
        if spaces.is_empty() {
            choices.push_str("<p class=\"muted\">You have no workspaces yet.</p>");
        }
        let wants_write = consent.request.scope.split(' ').any(|s| s == "write");
        let (read_checked, write_checked) = if wants_write {
            ("", " checked")
        } else {
            (" checked", "")
        };
        Ok::<_, AuthError>(
            (
                super::pages::security_headers(),
                Html(super::pages::consent_page(
                    &consent.request.client_name,
                    &q.request.to_string(),
                    &choices,
                    read_checked,
                    write_checked,
                )),
            )
                .into_response(),
        )
    }
    .await;
    match result {
        Ok(r) => r,
        Err(AuthError::BadRequest("expired", _)) => error_page(
            "This request has expired or was already answered. Start connecting the agent again.",
        ),
        Err(e) => e.into_response(),
    }
}

/// How the consent page names a workspace: there are no names yet, so by what's in it.
async fn describe_workspace(
    state: &AppState,
    user: drusite_model::UserId,
    ws: Uuid,
    n: usize,
    of: usize,
) -> String {
    let titles: Vec<String> = async {
        let mut tx = state.db().as_user(user).await.ok()?;
        let doc = crate::sync::store::load(state, &mut tx, ws).await.ok()??;
        let w = drusite_crdt::Workspace::load_all(&doc.parts()).ok()?;
        let roots: Vec<String> = w
            .pages()
            .ok()?
            .into_iter()
            .filter(|p| p.parent.is_none() && !p.title.is_empty())
            .map(|p| p.title)
            .collect();
        Some(roots)
    }
    .await
    .unwrap_or_default();
    let name = if of == 1 {
        "Your workspace".to_owned()
    } else {
        format!("Workspace {}", n + 1)
    };
    match titles.as_slice() {
        [] => name,
        [a] => format!("{name}, with “{a}”"),
        [a, b] => format!("{name}, with “{a}” and “{b}”"),
        [a, b, ..] => format!("{name}, with “{a}”, “{b}” and more"),
    }
}

/// `POST /oauth/consent`: the person's answer.
pub async fn consent_answer(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let mut request = None;
    let mut allow = false;
    let mut write = false;
    let mut journal = false;
    let mut chosen = Vec::new();
    for (k, v) in url::form_urlencoded::parse(&body) {
        match &*k {
            "request" => request = v.parse::<Uuid>().ok(),
            "decision" => allow = v == "allow",
            "access" => write = v == "write",
            "journal" => journal = v == "on",
            "workspace" => chosen.extend(v.parse::<Uuid>().ok()),
            _ => {}
        }
    }
    let Some(id) = request else {
        return error_page("The answer didn't say which request it was for.");
    };
    let result = async {
        let Some((consent, browser)) = consent_for(&state, &headers, id, true).await? else {
            return Ok(error_page(
                "Sign in as the account you started with, then try connecting again.",
            ));
        };
        let mut request = consent.request;
        if !allow {
            return Ok(redirect_error(
                &state,
                &request.redirect_uri,
                request.state.as_deref(),
                "access_denied",
                "the person didn't allow it",
            ));
        }
        // Only workspaces the person is in.
        let theirs: Vec<Uuid> = crate::sync::store::workspaces(&state, consent.user)
            .await
            .map_err(|e| AuthError::internal(anyhow::anyhow!("{e}")))?
            .into_iter()
            .map(|(w, _)| w)
            .collect();
        chosen.retain(|w| theirs.contains(w));
        chosen.dedup();
        let access = drusite_mcp::Access {
            write,
            journal: write && journal,
            workspaces: Some(
                chosen
                    .into_iter()
                    .map(drusite_model::WorkspaceId::from_uuid)
                    .collect(),
            ),
        };
        request.scope = match (access.write, access.journal) {
            (false, _) => "read",
            (true, false) => "read write",
            (true, true) => "read write journal",
        }
        .to_owned();
        let browser = BrowserSession {
            method: consent.method,
            ..browser
        };
        Ok::<_, AuthError>(see_other(
            issue_code(&state, &request, browser, Some(&access))
                .await?
                .as_str(),
        ))
    }
    .await;
    match result {
        Ok(r) => r,
        Err(AuthError::BadRequest("expired", _)) => error_page(
            "This request has expired or was already answered. Start connecting the agent again.",
        ),
        Err(e) => e.into_response(),
    }
}
