// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sign-in with any OIDC provider (Authentik, Google, Keycloak, …), with the openidconnect
//! crate checking the ID token: signature, issuer, audience, expiry and nonce.
//!
//! What a sign-in may do is deliberate about `email_verified`:
//! - An account is found by (provider, subject), never by email.
//! - A first sign-in joins an existing account with the same email only if the provider is
//!   configured to (`link_by_verified_email`), says it verified the email, and the existing
//!   account's email is verified too. Otherwise a provider that lets anyone claim any email,
//!   or an attacker who signed up here with your email first, could take an account over.
//! - `allowed_email_domains` counts only verified emails, and `allowed_groups` is checked at
//!   every sign-in, not just the first.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration as StdDuration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Router, routing::get};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce,
    OAuth2TokenResponse as _, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    TokenResponse as _,
};
use serde::{Deserialize, Serialize};
use time::Duration;
use uuid::Uuid;

use super::error::{AuthError, AuthResult};
use super::login::next_url;
use super::pages::message_page;
use super::session::{self, Method};
use super::users;
use crate::app::AppState;
use crate::config::OidcProviderConfig;
use drusite_model::UserId;

const KIND: &str = "oidc";
const TTL: Duration = Duration::minutes(10);
const BINDING_COOKIE: &str = "drusite_oidc";
/// How long provider metadata (and its signing keys) is kept before it's fetched again.
const METADATA_TTL: StdDuration = StdDuration::from_secs(600);

/// Under `/api/v1/auth/oidc`. A provider that isn't configured or is off answers 404.
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/{provider}/start", get(start))
        .route("/{provider}/callback", get(callback))
}

/// The path a provider sends the browser back to.
pub(super) fn callback_path(id: &str) -> String {
    format!("{}/{id}/callback", super::OIDC_PATH)
}

/// Provider metadata, fetched by discovery and kept for a while, and the HTTP client that
/// talks to providers: no redirects (a provider must answer directly), and a timeout.
#[derive(Debug)]
pub(super) struct Providers {
    http: reqwest::Client,
    cache: RwLock<HashMap<String, (Instant, CoreProviderMetadata)>>,
}

/// openidconnect's HTTP client, on this crate's reqwest.
#[derive(Debug, Clone)]
struct OidcHttp(reqwest::Client);

impl<'c> openidconnect::AsyncHttpClient<'c> for OidcHttp {
    type Error = HttpError;
    type Future = std::pin::Pin<
        Box<dyn Future<Output = Result<openidconnect::HttpResponse, HttpError>> + Send + 'c>,
    >;

    fn call(&'c self, request: openidconnect::HttpRequest) -> Self::Future {
        let client = self.0.clone();
        Box::pin(async move {
            let request = reqwest::Request::try_from(request)?;
            let response = client.execute(request).await?;
            let mut out = http::Response::builder().status(response.status());
            for (name, value) in response.headers() {
                out = out.header(name, value);
            }
            let body = response.bytes().await?.to_vec();
            Ok(out.body(body).unwrap_or_default())
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(super) struct HttpError(#[from] reqwest::Error);

impl Providers {
    pub(super) fn new() -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(StdDuration::from_secs(10))
                .build()?,
            cache: RwLock::default(),
        })
    }

    fn client(&self) -> OidcHttp {
        OidcHttp(self.http.clone())
    }

    async fn metadata(
        &self,
        id: &str,
        config: &OidcProviderConfig,
        refresh: bool,
    ) -> AuthResult<CoreProviderMetadata> {
        let cached = self
            .cache
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .filter(|(at, _)| !refresh && at.elapsed() < METADATA_TTL)
            .map(|(_, m)| m.clone());
        if let Some(metadata) = cached {
            return Ok(metadata);
        }
        let issuer = IssuerUrl::new(config.issuer.as_str().trim_end_matches('/').to_owned())
            .map_err(AuthError::internal)?;
        let metadata = CoreProviderMetadata::discover_async(issuer, &self.client())
            .await
            .map_err(|e| AuthError::internal(anyhow::anyhow!("OIDC discovery for {id}: {e}")))?;
        self.cache
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_owned(), (Instant::now(), metadata.clone()));
        Ok(metadata)
    }
}

fn provider<'a>(state: &'a AppState, id: &str) -> AuthResult<&'a OidcProviderConfig> {
    state
        .config()
        .auth
        .oidc
        .get(id)
        .filter(|p| p.enabled)
        .ok_or(AuthError::NotFound)
}

fn redirect_url(state: &AppState, id: &str) -> AuthResult<RedirectUrl> {
    RedirectUrl::new(format!(
        "{}{}",
        state.config().public_url.origin().ascii_serialization(),
        callback_path(id)
    ))
    .map_err(AuthError::internal)
}

#[derive(Debug, Serialize, Deserialize)]
struct Pending {
    provider: String,
    nonce: String,
    verifier: String,
    request: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(super) struct StartQuery {
    request: Option<Uuid>,
}

async fn start(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<StartQuery>,
) -> Response {
    match start_inner(&state, &id, q).await {
        Ok(r) => r,
        Err(e) => failure_page(&e),
    }
}

async fn start_inner(state: &AppState, id: &str, q: StartQuery) -> AuthResult<Response> {
    let config = provider(state, id)?;
    let metadata = state.auth().providers.metadata(id, config, false).await?;
    let client = CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(config.client_id.clone()),
        config
            .client_secret
            .as_ref()
            .map(|s| ClientSecret::new(s.expose().to_owned())),
    )
    .set_redirect_uri(redirect_url(state, id)?);
    let (pkce, verifier) = PkceCodeChallenge::new_random_sha256();
    let nonce = Nonce::new_random();
    // A random value in a cookie ties the callback to this browser, so nobody can send you a
    // callback link that signs you in to their account.
    let binding = URL_SAFE_NO_PAD.encode(crate::crypto::random_bytes::<32>()?.as_ref());
    let challenge = session::put_challenge(
        state,
        KIND,
        &Pending {
            provider: id.to_owned(),
            nonce: nonce.secret().clone(),
            verifier: verifier.secret().clone(),
            request: q.request,
        },
        Some(&binding),
        TTL,
    )
    .await?;
    let mut request = client.authorize_url(
        CoreAuthenticationFlow::AuthorizationCode,
        move || CsrfToken::new(challenge.to_string()),
        move || nonce,
    );
    for scope in config.scopes.iter().filter(|s| *s != "openid") {
        request = request.add_scope(Scope::new(scope.clone()));
    }
    let (url, _, _) = request.set_pkce_challenge(pkce).url();
    let cookie = session::cookie(
        state,
        BINDING_COOKIE,
        &binding,
        super::OIDC_PATH,
        TTL.whole_seconds(),
    );
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to(url.as_str())).into_response())
}

#[derive(Debug, Deserialize)]
pub(super) struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let clear = session::cookie(&state, BINDING_COOKIE, "", super::OIDC_PATH, 0);
    let mut response = match callback_inner(&state, &id, &headers, q).await {
        Ok(r) => r,
        Err(e) => failure_page(&e),
    };
    response.headers_mut().append(header::SET_COOKIE, clear);
    response
}

/// What the ID token says about the person.
#[derive(Debug)]
struct Person {
    subject: String,
    email: Option<String>,
    email_verified: bool,
    name: String,
    groups: Vec<String>,
}

async fn callback_inner(
    state: &AppState,
    id: &str,
    headers: &HeaderMap,
    q: CallbackQuery,
) -> AuthResult<Response> {
    if let Some(error) = q.error {
        tracing::info!(provider = id, %error, "OIDC provider returned an error");
        return Err(AuthError::Forbidden(
            "provider_refused",
            "the sign-in was cancelled or refused at the provider",
        ));
    }
    let (Some(code), Some(challenge)) = (q.code, q.state.and_then(|s| s.parse::<Uuid>().ok()))
    else {
        return Err(AuthError::bad(
            "bad_callback",
            "the provider's answer is incomplete",
        ));
    };
    let binding = session::read_cookie(headers, BINDING_COOKIE);
    let pending: Pending = session::take_challenge(state, challenge, KIND, binding).await?;
    if pending.provider != id {
        return Err(AuthError::bad(
            "bad_callback",
            "this answer is for another provider",
        ));
    }
    let config = provider(state, id)?;
    let person = exchange(state, id, config, code, &pending).await?;
    let user = find_or_create(state, id, config, &person).await?;
    let cookie = session::start(state, user, Method::Oidc).await?;
    tracing::info!(%user, provider = id, "signed in");
    Ok((
        [(header::SET_COOKIE, cookie)],
        Redirect::to(&next_url(pending.request)),
    )
        .into_response())
}

async fn exchange(
    state: &AppState,
    id: &str,
    config: &OidcProviderConfig,
    code: String,
    pending: &Pending,
) -> AuthResult<Person> {
    let providers = &state.auth().providers;
    let metadata = providers.metadata(id, config, false).await?;
    let client = CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(config.client_id.clone()),
        config
            .client_secret
            .as_ref()
            .map(|s| ClientSecret::new(s.expose().to_owned())),
    )
    .set_redirect_uri(redirect_url(state, id)?);
    let token = client
        .exchange_code(AuthorizationCode::new(code))
        .map_err(AuthError::internal)?
        .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier.clone()))
        .request_async(&providers.client())
        .await
        .map_err(|e| {
            tracing::warn!(provider = id, error = %e, "OIDC code exchange failed");
            AuthError::Unauthorized("provider_failed", "the provider didn't accept the sign-in")
        })?;
    let _ = token.access_token();
    let id_token = token.id_token().ok_or(AuthError::Unauthorized(
        "provider_failed",
        "the provider sent no ID token",
    ))?;
    let nonce = Nonce::new(pending.nonce.clone());
    let claims = match id_token.claims(&client.id_token_verifier(), &nonce) {
        Ok(claims) => claims.clone(),
        Err(e) => {
            // Perhaps the provider rotated its keys: fetch them again and retry once.
            tracing::info!(provider = id, error = %e, "ID token didn't verify; refreshing keys");
            let metadata = providers.metadata(id, config, true).await?;
            let client = CoreClient::from_provider_metadata(
                metadata,
                ClientId::new(config.client_id.clone()),
                config
                    .client_secret
                    .as_ref()
                    .map(|s| ClientSecret::new(s.expose().to_owned())),
            );
            id_token
                .claims(&client.id_token_verifier(), &nonce)
                .map_err(|e| {
                    tracing::warn!(provider = id, error = %e, "ID token didn't verify");
                    AuthError::Unauthorized(
                        "provider_failed",
                        "the provider's ID token didn't verify",
                    )
                })?
                .clone()
        }
    };
    // The token verified, so its payload can be read for the claims openidconnect doesn't type.
    let groups = if config.groups_claim.is_empty() {
        Vec::new()
    } else {
        id_token
            .to_string()
            .split('.')
            .nth(1)
            .and_then(|p| URL_SAFE_NO_PAD.decode(p).ok())
            .and_then(|p| serde_json::from_slice::<serde_json::Value>(&p).ok())
            .and_then(|v| v.get(&config.groups_claim).cloned())
            .and_then(|g| serde_json::from_value::<Vec<String>>(g).ok())
            .unwrap_or_default()
    };
    let name = claims
        .name()
        .and_then(|n| n.get(None))
        .map(|n| n.as_str().to_owned())
        .or_else(|| claims.preferred_username().map(|u| u.as_str().to_owned()))
        .unwrap_or_default();
    Ok(Person {
        subject: claims.subject().as_str().to_owned(),
        email: claims.email().map(|e| e.as_str().to_owned()),
        email_verified: claims.email_verified().unwrap_or(false),
        name,
        groups,
    })
}

fn email_domain_allowed(config: &OidcProviderConfig, person: &Person) -> bool {
    if config.allowed_email_domains.is_empty() {
        return true;
    }
    let Some(email) = person.email.as_deref().filter(|_| person.email_verified) else {
        return false;
    };
    let domain = email.rsplit_once('@').map(|(_, d)| d.to_lowercase());
    domain.is_some_and(|d| {
        config
            .allowed_email_domains
            .iter()
            .any(|allowed| allowed.trim_start_matches('@').eq_ignore_ascii_case(&d))
    })
}

async fn find_or_create(
    state: &AppState,
    id: &str,
    config: &OidcProviderConfig,
    person: &Person,
) -> AuthResult<UserId> {
    if !config.allowed_groups.is_empty()
        && !person
            .groups
            .iter()
            .any(|g| config.allowed_groups.contains(g))
    {
        return Err(AuthError::Forbidden(
            "not_allowed",
            "your account at the provider isn't in a group allowed to use this server",
        ));
    }
    if !email_domain_allowed(config, person) {
        return Err(AuthError::Forbidden(
            "not_allowed",
            "your email address isn't one allowed to use this server",
        ));
    }

    let known: Option<Uuid> = sqlx::query_scalar(
        "UPDATE oidc_identities SET last_login_at = now(), email = $3
         WHERE provider = $1 AND subject = $2 RETURNING user_id",
    )
    .bind(id)
    .bind(&person.subject)
    .bind(&person.email)
    .fetch_optional(state.db().pool())
    .await?;
    let user = if let Some(user) = known {
        UserId::from_uuid(user)
    } else {
        let existing = match &person.email {
            Some(email) => users::by_email(state, email).await?,
            None => None,
        };
        let user = match existing {
            Some(account)
                if config.link_by_verified_email
                    && person.email_verified
                    && account.email_verified =>
            {
                account.id
            }
            Some(_) => {
                return Err(AuthError::Forbidden(
                    "account_exists",
                    "an account with your email already exists. Sign in to it another way; \
                     this provider can't be joined to it automatically",
                ));
            }
            None if config.auto_create => {
                let email = person.email.as_deref().ok_or(AuthError::Forbidden(
                    "no_email",
                    "the provider didn't share your email address, which a new account needs",
                ))?;
                let email = users::normalize_email(email)?;
                users::create(state, &email, &person.name, person.email_verified)
                    .await?
                    .id
            }
            None => {
                return Err(AuthError::Forbidden(
                    "no_account",
                    "there's no account for you on this server; ask its administrator",
                ));
            }
        };
        let inserted = sqlx::query(
            "INSERT INTO oidc_identities (provider, subject, user_id, email)
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(&person.subject)
        .bind(user.as_uuid())
        .bind(&person.email)
        .execute(state.db().pool())
        .await?;
        if inserted.rows_affected() == 1 {
            tracing::info!(%user, provider = id, "OIDC identity added to account");
        }
        user
    };
    if users::by_id(state, user).await?.is_none_or(|a| a.disabled) {
        return Err(AuthError::Forbidden("disabled", "this account is disabled"));
    }
    Ok(user)
}

/// Errors end on a page in the browser: the provider sent it here, not the sign-in page's code.
fn failure_page(error: &AuthError) -> Response {
    let (status, text) = match error {
        AuthError::Forbidden(_, m) | AuthError::Unauthorized(_, m) => {
            (StatusCode::FORBIDDEN, (*m).to_owned())
        }
        AuthError::BadRequest(_, m) => (StatusCode::BAD_REQUEST, m.clone()),
        AuthError::NotFound => (
            StatusCode::NOT_FOUND,
            "There's no such sign-in provider.".to_owned(),
        ),
        other => {
            // Let it log itself.
            let response = AuthError::Internal(anyhow::anyhow!("{other}")).into_response();
            (
                response.status(),
                "Something went wrong on the server. Try again later.".to_owned(),
            )
        }
    };
    (status, Html(message_page("Can't sign in", &text))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(domains: &[&str]) -> OidcProviderConfig {
        OidcProviderConfig {
            enabled: true,
            display_name: "x".into(),
            issuer: url::Url::parse("https://idp.example").unwrap_or_else(|_| unreachable!()),
            client_id: "c".into(),
            client_secret: None,
            scopes: vec!["openid".into()],
            auto_create: true,
            link_by_verified_email: false,
            allowed_email_domains: domains.iter().map(|d| (*d).to_owned()).collect(),
            allowed_groups: vec![],
            groups_claim: "groups".into(),
        }
    }

    fn person(email: &str, verified: bool) -> Person {
        Person {
            subject: "s".into(),
            email: Some(email.into()),
            email_verified: verified,
            name: String::new(),
            groups: vec![],
        }
    }

    #[test]
    fn domains_count_only_verified_emails() {
        let c = config(&["example.com"]);
        assert!(email_domain_allowed(&c, &person("a@EXAMPLE.com", true)));
        assert!(!email_domain_allowed(&c, &person("a@example.com", false)));
        assert!(!email_domain_allowed(
            &c,
            &person("a@example.com.evil", true)
        ));
        assert!(email_domain_allowed(
            &config(&[]),
            &person("a@any.where", false)
        ));
    }
}
