// SPDX-License-Identifier: AGPL-3.0-or-later

//! Staying signed in to a server, and in sync with it: what the apps run.
//!
//! A [`Supervisor`] owns the device's connection. Signed in, it keeps one sync connection open
//! ([`crate::sync::net::run`]), refreshing the access token before it runs out and reconnecting
//! with backoff when the network goes away. It reports how things stand as an
//! [`AccountStatus`], which the app forwards to the UI.
//!
//! Every request to the server (sync, attachments, Ask, sharing) uses the one access token the
//! supervisor holds. It's refreshed a minute before it runs out, even while the connection stays
//! open, and a request the server refuses with `invalid_token` refreshes and tries once more.
//!
//! The refresh token is kept in the encrypted store, and rotates on every refresh: the new one
//! is saved before the access token is used. The device key never leaves the platform's key
//! store; it signs a DPoP proof per connection and per refresh.
//!
//! Signing in is two halves, because the middle happens in the system browser:
//! [`Supervisor::begin_sign_in`] gives the page to open, and [`Supervisor::finish_sign_in`]
//! takes the redirect the browser came back with.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use drusite_model::WorkspaceId;
use serde::Deserialize;
use tokio::sync::Notify;
use url::Url;

use crate::api::{AccountStatus, SyncPhase};
use crate::auth::{self, Authorization, DeviceKey};
use crate::engine::Core;
use crate::sync::SyncError;
use crate::sync::net::{self, NetError};

mod ask;
mod share;

/// Refresh the access token when it has less than this left (or less than half its life, for
/// tokens shorter than two minutes).
const REFRESH_BEFORE: Duration = Duration::from_secs(60);
/// How long to wait before trying again after refreshing ahead of time failed.
const REFRESH_RETRY: Duration = Duration::from_secs(30);
/// How long one HTTP request may take.
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
/// The shortest and longest wait before reconnecting.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// How long to wait after a failure a retry is unlikely to fix.
const FAILED_RETRY: Duration = Duration::from_secs(300);

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("{0}")]
    Unreachable(String),
    #[error("{0}")]
    NotDrusite(String),
    #[error("the server refused: {0}")]
    Refused(String),
    #[error("no sign-in is waiting for that answer")]
    NoSignIn,
    #[error(transparent)]
    Auth(#[from] auth::AuthError),
    #[error(transparent)]
    Core(#[from] crate::Error),
    #[error("the core is unusable after a panic elsewhere")]
    Poisoned,
}

/// The device key, shared with the connection task.
pub type SharedKey = Arc<dyn DeviceKey + Send + Sync>;

/// Turns what someone typed into a server's base URL: `https://` when no scheme is given, and a
/// trailing slash, so paths join under it.
pub fn server_url(typed: &str) -> Result<Url, AccountError> {
    let typed = typed.trim();
    let with_scheme = if typed.contains("://") {
        typed.to_owned()
    } else {
        format!("https://{typed}")
    };
    let mut url = Url::parse(&with_scheme)
        .map_err(|_| AccountError::NotDrusite(format!("“{typed}” isn't a server address")))?;
    if !matches!(url.scheme(), "https" | "http") || url.host().is_none() {
        return Err(AccountError::NotDrusite(format!(
            "“{typed}” isn't a server address"
        )));
    }
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

#[derive(Debug, Deserialize)]
struct Metadata {
    issuer: String,
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    expires_in: u64,
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct OAuthError {
    error: String,
    /// Drusite's own API calls it `message`.
    #[serde(default, alias = "message")]
    error_description: String,
}

#[derive(Debug, Deserialize)]
struct WorkspaceInfo {
    id: WorkspaceId,
    role: String,
}

#[derive(Debug, Deserialize)]
struct Me {
    email: String,
}

/// A blocking HTTP call failed.
enum HttpError {
    /// The network, or a server that answers garbage: worth retrying.
    Network(String),
    /// The server said no (an OAuth error, or another 4xx).
    Refused { code: String, description: String },
}

impl From<HttpError> for AccountError {
    fn from(e: HttpError) -> Self {
        match e {
            HttpError::Network(m) => Self::Unreachable(m),
            HttpError::Refused { code, description } => {
                Self::Refused(format!("{code}: {description}"))
            }
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into()
}

fn read<T: serde::de::DeserializeOwned>(
    response: Result<http::Response<ureq::Body>, ureq::Error>,
) -> Result<T, HttpError> {
    let mut response = response.map_err(|e| HttpError::Network(e.to_string()))?;
    let status = response.status();
    if status.is_success() {
        return response
            .body_mut()
            .read_json()
            .map_err(|e| HttpError::Network(format!("unexpected answer: {e}")));
    }
    if status.is_client_error() {
        let body: Option<OAuthError> = response.body_mut().read_json().ok();
        return Err(match body {
            Some(e) => HttpError::Refused {
                code: e.error,
                description: e.error_description,
            },
            None => HttpError::Refused {
                code: status.as_u16().to_string(),
                description: status.canonical_reason().unwrap_or_default().to_owned(),
            },
        });
    }
    Err(HttpError::Network(format!("the server answered {status}")))
}

/// Runs a blocking HTTP call off the async threads.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, HttpError> + Send + 'static,
) -> Result<T, HttpError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| HttpError::Network(e.to_string()))?
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

struct Inner {
    core: Arc<Mutex<Core>>,
    key: SharedKey,
    device_name: String,
    on_status: Box<dyn Fn(&AccountStatus) + Send + Sync>,
    status: Mutex<AccountStatus>,
    /// The sign-in waiting in the browser, if any.
    pending: Mutex<Option<Authorization>>,
    /// Wakes the loop: drop the connection or the wait, and look again.
    wake: Notify,
    /// Set by [`Supervisor::stop`]: the loop ends when it next wakes.
    stopped: AtomicBool,
    /// The server and the access token every request uses: the sync connection, attachments,
    /// Ask, sharing. Only [`access`] changes it once signed in.
    current: Mutex<Option<Session>>,
    /// Held while refreshing, so the sync loop and a request never both spend the refresh token
    /// (it rotates on every use).
    refreshing: tokio::sync::Mutex<()>,
    /// The server, and the version it gave when sync last connected to it.
    server_version: Mutex<Option<(String, String)>>,
}

#[derive(Clone)]
struct Session {
    server: Url,
    access: Access,
}

/// The device's connection to its server. Cheap to clone; every clone is the same supervisor.
#[derive(Clone)]
pub struct Supervisor {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> Result<MutexGuard<'_, T>, AccountError> {
    m.lock().map_err(|_| AccountError::Poisoned)
}

impl Supervisor {
    /// Starts supervising `core`'s connection on `runtime`, signing with `key`. `device_name`
    /// is how the server lists this device. `on_status` hears every change of status.
    pub fn start(
        runtime: &tokio::runtime::Handle,
        core: Arc<Mutex<Core>>,
        key: SharedKey,
        device_name: impl Into<String>,
        on_status: impl Fn(&AccountStatus) + Send + Sync + 'static,
    ) -> Self {
        // Both rustls users (the WebSocket and HTTP) take the process's default provider. Only
        // one may be installed; a second install is refused, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let initial = match core
            .lock()
            .ok()
            .and_then(|c| c.store.account().ok())
            .flatten()
        {
            Some(a) if a.refresh_token.is_some() => AccountStatus {
                server: Some(a.server),
                email: a.email,
                phase: SyncPhase::Connecting,
                message: None,
                server_version: None,
            },
            _ => AccountStatus::signed_out(None),
        };
        let inner = Arc::new(Inner {
            core,
            key,
            device_name: device_name.into(),
            on_status: Box::new(on_status),
            status: Mutex::new(initial),
            pending: Mutex::new(None),
            wake: Notify::new(),
            stopped: AtomicBool::new(false),
            current: Mutex::new(None),
            refreshing: tokio::sync::Mutex::new(()),
            server_version: Mutex::new(None),
        });
        runtime.spawn(run(Arc::clone(&inner)));
        Self { inner }
    }

    pub fn status(&self) -> AccountStatus {
        self.inner
            .status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| AccountStatus::signed_out(None))
    }

    /// Drops the connection and ends the supervisor, for good. The store keeps the account, so
    /// a supervisor started later on the same store carries on.
    pub fn stop(&self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        self.inner.wake.notify_one();
    }

    /// Reconnects now instead of after the backoff: the app came back to the foreground, or the
    /// network came back.
    pub fn kick(&self) {
        self.inner.wake.notify_one();
    }

    /// Starts signing in to the server at `typed` (as the person typed it), coming back to
    /// `redirect_uri`. Returns the page to open in the browser.
    pub async fn begin_sign_in(
        &self,
        typed: &str,
        redirect_uri: &str,
    ) -> Result<Url, AccountError> {
        let typed_url = server_url(typed)?;
        let metadata_url = typed_url
            .join(".well-known/oauth-authorization-server")
            .map_err(|e| AccountError::NotDrusite(e.to_string()))?;
        let agent = agent();
        let metadata: Metadata = blocking(move || read(agent.get(metadata_url.as_str()).call()))
            .await
            .map_err(|e| match e {
                HttpError::Network(m) => {
                    AccountError::Unreachable(format!("can't reach {typed_url}: {m}"))
                }
                HttpError::Refused { .. } => AccountError::NotDrusite(format!(
                    "{typed_url} doesn't look like a Drusite server"
                )),
            })?;
        // The issuer is the server's own idea of its address; sign-in answers and proofs are
        // checked against it, so use it rather than what was typed.
        let server = server_url(&metadata.issuer)?;
        if server != typed_url {
            // Reached under another name (an IP address, a LAN name): the browser and sync will
            // both go to the issuer, so make sure this device can reach it before sending the
            // person to a page that won't load.
            let probe = server
                .join(".well-known/oauth-authorization-server")
                .map_err(|e| AccountError::NotDrusite(e.to_string()))?;
            let http = self::agent();
            if let Err(HttpError::Network(m)) =
                blocking(move || read::<Metadata>(http.get(probe.as_str()).call())).await
            {
                return Err(AccountError::Unreachable(format!(
                    "{typed_url} answers as {server}, but this device can't reach {server} ({m}). \
                     The server's public_url should be an address its devices can reach."
                )));
            }
        }
        let auth = Authorization::new(&server, redirect_uri, &self.inner.key.jwk().thumbprint())?;
        let url = auth.url().clone();
        *lock(&self.inner.pending)? = Some(auth);
        self.set(AccountStatus {
            server: Some(server.to_string()),
            email: None,
            phase: SyncPhase::SigningIn,
            message: None,
            server_version: None,
        });
        Ok(url)
    }

    /// Finishes signing in with the redirect the browser came back with.
    pub async fn finish_sign_in(&self, callback: &Url) -> Result<(), AccountError> {
        let auth = lock(&self.inner.pending)?
            .take()
            .ok_or(AccountError::NoSignIn)?;
        let result = self.exchange(&auth, callback).await;
        match &result {
            Ok(()) => self.inner.wake.notify_one(),
            Err(e) => self.set(AccountStatus::signed_out(Some(format!(
                "Signing in didn't work: {e}"
            )))),
        }
        result
    }

    async fn exchange(&self, auth: &Authorization, callback: &Url) -> Result<(), AccountError> {
        let code = auth.code_from(callback)?;
        let server = server_url(auth.server().as_str())?;
        let endpoint = auth::token_endpoint(&server)?;
        let proof = auth::proof(&*self.inner.key, "POST", &endpoint, None, now())?;
        let form = auth.token_form(&code, &self.inner.device_name);
        let agent = agent();
        let tokens: Tokens = blocking(move || {
            read(
                agent
                    .post(endpoint.as_str())
                    .header("DPoP", &proof)
                    .send_form(form.iter().map(|(k, v)| (*k, v.as_str()))),
            )
        })
        .await?;
        lock(&self.inner.core)?
            .store
            .sign_in(server.as_str(), &tokens.refresh_token)?;
        if let Ok(me) =
            get_json::<Me>(&server, "api/v1/me", &tokens.access_token, &self.inner.key).await
        {
            lock(&self.inner.core)?.store.set_account_email(&me.email)?;
        }
        *lock(&self.inner.current)? = Some(Session {
            server,
            access: Access::new(tokens.access_token, tokens.expires_in),
        });
        Ok(())
    }

    /// The browser half of a sign-in failed before there was an answer to finish with.
    pub fn sign_in_failed(&self, message: &str) {
        if let Ok(mut pending) = self.inner.pending.lock() {
            pending.take();
        }
        self.set(AccountStatus::signed_out(Some(format!(
            "Signing in didn't work: {message}"
        ))));
        self.inner.wake.notify_one();
    }

    /// Gives up a sign-in still waiting in the browser.
    pub fn cancel_sign_in(&self) {
        if let Ok(mut pending) = self.inner.pending.lock()
            && pending.take().is_some()
        {
            self.inner.wake.notify_one();
        }
    }

    /// Signs out: drops the connection, forgets the refresh token, and asks the server to
    /// revoke it (best effort: offline, the server's grant simply goes unused).
    pub async fn sign_out(&self) -> Result<(), AccountError> {
        let account = {
            let mut core = lock(&self.inner.core)?;
            let account = core.store.account()?;
            core.store.sign_out()?;
            account
        };
        *lock(&self.inner.current)? = None;
        self.inner.wake.notify_one();
        if let Some(account) = account
            && let Some(token) = account.refresh_token
            && let Ok(server) = server_url(&account.server)
            && let Ok(endpoint) = server.join("oauth/revoke")
        {
            let agent = agent();
            let form = [
                ("token", token),
                ("token_type_hint", "refresh_token".to_owned()),
                ("client_id", auth::CLIENT_ID.to_owned()),
            ];
            let _ = blocking(move || {
                agent
                    .post(endpoint.as_str())
                    .send_form(form.iter().map(|(k, v)| (*k, v.as_str())))
                    .map(drop)
                    .map_err(|e| HttpError::Network(e.to_string()))
            })
            .await;
        }
        Ok(())
    }

    fn set(&self, status: AccountStatus) {
        set_status(&self.inner, status);
    }

    /// The server and an access token that isn't about to run out, refreshing it if it is.
    /// `stale` is a token the server just refused: it's replaced even if it should still be
    /// good (the server's clock, or a token revoked early).
    async fn access(&self, stale: Option<&str>) -> Result<(Url, String), TokenError> {
        access(&self.inner, stale).await
    }

    pub(crate) fn workspace(&self) -> Result<String, String> {
        self.inner
            .core
            .lock()
            .map(|c| c.workspace_id().to_string())
            .map_err(|_| "the core crashed".to_owned())
    }

    /// What agents did through the server in this device's workspace, newest first.
    pub async fn server_agent_activity(&self) -> Result<Vec<crate::api::AgentCall>, String> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Item {
            id: String,
            agent: String,
            tool: String,
            summary: String,
            at: i64,
            undone_at: Option<i64>,
            pages: Vec<drusite_model::PageId>,
        }
        let ws = self.workspace()?;
        let items: Vec<Item> = self
            .call(
                "GET",
                &format!("api/v1/workspaces/{ws}/agent-activity"),
                None,
            )
            .await
            .map_err(|f| f.message)?;
        Ok(items
            .into_iter()
            .map(|i| crate::api::AgentCall {
                id: i.id,
                source: crate::api::AgentSource::Server,
                agent: i.agent,
                tool: i.tool,
                summary: i.summary,
                at: i.at,
                undone_at: i.undone_at,
                pages: i.pages,
            })
            .collect())
    }

    /// Undoes an agent's call on the server; the change comes back here by sync.
    pub async fn undo_server_agent_call(&self, id: &str) -> Result<(), String> {
        let ws = self.workspace()?;
        let _: serde_json::Value = self
            .call(
                "POST",
                &format!("api/v1/workspaces/{ws}/agent-activity/{id}/undo"),
                None,
            )
            .await
            .map_err(|f| f.message)?;
        Ok(())
    }
}

fn set_status(inner: &Inner, mut status: AccountStatus) {
    // The server's version stays known while it's the same server, offline or not.
    if let Ok(known) = inner.server_version.lock() {
        status.server_version = known
            .as_ref()
            .filter(|(server, _)| status.server.as_ref() == Some(server))
            .map(|(_, version)| version.clone());
    }
    let Ok(mut current) = inner.status.lock() else {
        return;
    };
    if *current != status {
        current.clone_from(&status);
        drop(current);
        (inner.on_status)(&status);
    }
}

/// GETs `path` on `server` as JSON, with the access token and a proof.
async fn get_json<T: serde::de::DeserializeOwned + Send + 'static>(
    server: &Url,
    path: &str,
    access_token: &str,
    key: &SharedKey,
) -> Result<T, HttpError> {
    let url = server
        .join(path)
        .map_err(|e| HttpError::Network(e.to_string()))?;
    let proof = auth::proof(&**key, "GET", &url, Some(access_token), now())
        .map_err(|e| HttpError::Network(e.to_string()))?;
    let authorization = format!("DPoP {access_token}");
    let agent = agent();
    blocking(move || {
        read(
            agent
                .get(url.as_str())
                .header("Authorization", &authorization)
                .header("DPoP", &proof)
                .call(),
        )
    })
    .await
}

/// What to do after an attempt.
enum Next {
    /// Go again now.
    Now,
    /// Wait this long (or until woken), then go again.
    Wait(Duration),
    /// Wait until woken.
    Idle,
}

#[derive(Clone)]
struct Access {
    token: String,
    expires: tokio::time::Instant,
    lifetime: Duration,
}

impl Access {
    fn new(token: String, expires_in: u64) -> Self {
        let lifetime = Duration::from_secs(expires_in);
        Self {
            token,
            expires: tokio::time::Instant::now() + lifetime,
            lifetime,
        }
    }

    /// When to refresh it: a minute before it runs out, or halfway for a short one, so a
    /// one-minute token isn't refreshed on every request.
    fn refresh_at(&self) -> tokio::time::Instant {
        self.expires
            .checked_sub(REFRESH_BEFORE.min(self.lifetime / 2))
            .unwrap_or(self.expires)
    }

    fn usable(&self) -> bool {
        tokio::time::Instant::now() < self.refresh_at()
    }
}

/// Why there's no access token to use.
#[derive(Debug)]
pub(crate) enum TokenError {
    /// No account on this device, or the server just signed it out.
    SignedOut,
    /// The server can't be reached to refresh it.
    Network(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SignedOut => f.write_str("not signed in to a server"),
            Self::Network(m) => f.write_str(m),
        }
    }
}

impl From<TokenError> for String {
    fn from(e: TokenError) -> Self {
        e.to_string()
    }
}

/// The token in use, refreshed first if it's about to run out or is `stale`. One refresh at a
/// time: whoever waited behind another's refresh takes its result.
async fn access(inner: &Inner, stale: Option<&str>) -> Result<(Url, String), TokenError> {
    let _one = inner.refreshing.lock().await;
    let account = inner
        .core
        .lock()
        .map_err(|_| TokenError::Network("the core crashed".to_owned()))?
        .store
        .account()
        .map_err(|e| TokenError::Network(format!("can't read the store: {e}")))?;
    let Some((server, refresh_token)) =
        account.and_then(|a| a.refresh_token.map(|t| (a.server, t)))
    else {
        if let Ok(mut current) = inner.current.lock() {
            *current = None;
        }
        return Err(TokenError::SignedOut);
    };
    let server = server_url(&server).map_err(|e| TokenError::Network(e.to_string()))?;
    let held = inner.current.lock().ok().and_then(|c| c.clone());
    if let Some(s) = held
        && s.server == server
        && s.access.usable()
        && stale != Some(s.access.token.as_str())
    {
        return Ok((s.server, s.access.token));
    }
    match refresh(inner, &server, &refresh_token).await {
        Ok(a) => {
            let token = a.token.clone();
            if let Ok(mut current) = inner.current.lock() {
                *current = Some(Session {
                    server: server.clone(),
                    access: a,
                });
            }
            Ok((server, token))
        }
        Err(HttpError::Refused { code, description }) => {
            if let Ok(mut core) = inner.core.lock() {
                let _ = core.store.sign_out();
            }
            if let Ok(mut current) = inner.current.lock() {
                *current = None;
            }
            set_status(
                inner,
                AccountStatus::signed_out(Some(format!(
                    "The server signed this device out ({code}: {description}). Sign in again to keep syncing."
                ))),
            );
            inner.wake.notify_one();
            Err(TokenError::SignedOut)
        }
        Err(HttpError::Network(m)) => Err(TokenError::Network(m)),
    }
}

/// When the token in use should be refreshed.
fn refresh_due(inner: &Inner) -> tokio::time::Instant {
    inner
        .current
        .lock()
        .ok()
        .and_then(|c| c.as_ref().map(|s| s.access.refresh_at()))
        .unwrap_or_else(|| tokio::time::Instant::now() + REFRESH_BEFORE)
}

/// The supervisor's loop: one attempt at a time, until the app exits.
async fn run(inner: Arc<Inner>) {
    let mut backoff = BACKOFF_MIN;
    while !inner.stopped.load(Ordering::SeqCst) {
        let next = attempt(&inner, &mut backoff).await;
        if inner.stopped.load(Ordering::SeqCst) {
            break;
        }
        match next {
            Next::Now => {}
            Next::Wait(d) => {
                tokio::select! {
                    () = tokio::time::sleep(d) => {}
                    () = inner.wake.notified() => {}
                }
            }
            Next::Idle => inner.wake.notified().await,
        }
    }
}

/// The next backoff: doubled up to the maximum, with up to a quarter of jitter so devices don't
/// all come back at once after the server restarts.
fn grow(backoff: &mut Duration) -> Duration {
    let current = *backoff;
    *backoff = (current * 2).min(BACKOFF_MAX);
    let mut byte = [0u8; 1];
    let _ = getrandom::fill(&mut byte);
    current + current.mul_f64(f64::from(byte[0]) / 1024.0)
}

async fn attempt(inner: &Arc<Inner>, backoff: &mut Duration) -> Next {
    let account = match inner.core.lock().map(|c| c.store.account()) {
        Ok(Ok(a)) => a,
        Ok(Err(e)) => return failed(inner, None, None, &format!("can't read the store: {e}")),
        Err(_) => return failed(inner, None, None, "the core crashed; restart Drusite"),
    };
    let signing_in = inner.pending.lock().is_ok_and(|p| p.is_some());
    let Some(account) = account.filter(|a| a.refresh_token.is_some()) else {
        if let Ok(mut current) = inner.current.lock() {
            *current = None;
        }
        if !signing_in {
            let message = inner.status.lock().ok().and_then(|s| {
                (s.phase == SyncPhase::SignedOut)
                    .then(|| s.message.clone())
                    .flatten()
            });
            set_status(inner, AccountStatus::signed_out(message));
        }
        return Next::Idle;
    };
    let status = |phase, message: Option<String>| AccountStatus {
        server: Some(account.server.clone()),
        email: account.email.clone(),
        phase,
        message,
        server_version: None,
    };

    let usable = inner
        .current
        .lock()
        .is_ok_and(|c| c.as_ref().is_some_and(|s| s.access.usable()));
    if !usable {
        set_status(inner, status(SyncPhase::Connecting, None));
    }
    let (server, token) = match access(inner, None).await {
        Ok(ok) => ok,
        // The status already says so.
        Err(TokenError::SignedOut) => return Next::Idle,
        Err(TokenError::Network(m)) => {
            set_status(inner, status(SyncPhase::Offline, Some(m)));
            return Next::Wait(grow(backoff));
        }
    };

    let join = match workspace_to_join(inner, &server, &token).await {
        Ok(join) => join,
        Err(JoinProblem::Network(m)) => {
            set_status(inner, status(SyncPhase::Offline, Some(m)));
            return Next::Wait(grow(backoff));
        }
        Err(JoinProblem::Unauthorized) => {
            invalidate(inner, &token);
            return Next::Wait(grow(backoff));
        }
        Err(JoinProblem::Conflict) => {
            set_status(
                inner,
                status(
                    SyncPhase::Failed,
                    Some(
                        "This device has notes of its own, and the account already has notes from \
                         another device. Drusite can't merge two sets of notes yet, so this device \
                         isn't syncing."
                            .to_owned(),
                    ),
                ),
            );
            return Next::Idle;
        }
    };

    set_status(inner, status(SyncPhase::Connecting, None));
    let online = {
        let inner = Arc::clone(inner);
        let online = status(SyncPhase::Online, None);
        move |version: &str| {
            if let Ok(mut known) = inner.server_version.lock() {
                *known = online
                    .server
                    .clone()
                    .zip(Some(version.trim().to_owned()).filter(|v| !v.is_empty()));
            }
            set_status(&inner, online);
        }
    };
    // Attachments and requests take the token in use when they start, which is refreshed
    // ahead of time below while the connection stays open.
    let in_use = {
        let inner = Arc::clone(inner);
        let first = token.clone();
        move || {
            inner
                .current
                .lock()
                .ok()
                .and_then(|c| c.as_ref().map(|s| s.access.token.clone()))
                .unwrap_or_else(|| first.clone())
        }
    };
    let connected = net::run_observed(
        Arc::clone(&inner.core),
        &server,
        in_use,
        &*inner.key,
        join,
        now(),
        online,
    );
    tokio::pin!(connected);
    let mut refreshing: Option<tokio::task::JoinHandle<bool>> = None;
    let mut retry_at: Option<tokio::time::Instant> = None;
    let ended = loop {
        let due = retry_at.unwrap_or_else(|| refresh_due(inner));
        tokio::select! {
            ended = &mut connected => break Some(ended),
            () = inner.wake.notified() => break None,
            refreshed = async {
                match refreshing.as_mut() {
                    Some(h) => h.await.unwrap_or(false),
                    None => false,
                }
            }, if refreshing.is_some() => {
                refreshing = None;
                retry_at = (!refreshed).then(|| tokio::time::Instant::now() + REFRESH_RETRY);
            }
            () = tokio::time::sleep_until(due), if refreshing.is_none() => {
                // Off the loop, so the connection keeps going while the server answers.
                let inner = Arc::clone(inner);
                refreshing = Some(tokio::spawn(async move { access(&inner, None).await.is_ok() }));
            }
        }
    };
    let was_online = inner
        .status
        .lock()
        .is_ok_and(|s| s.phase == SyncPhase::Online);
    let Some(ended) = ended else {
        // Woken: dropped the connection on purpose. Tidy what the driver would have.
        if let Ok(mut core) = inner.core.lock() {
            core.sync_stop();
            core.set_change_listener(None);
        }
        *backoff = BACKOFF_MIN;
        return Next::Now;
    };
    if was_online {
        *backoff = BACKOFF_MIN;
    }
    match ended {
        Ok(()) => {
            set_status(inner, status(SyncPhase::Connecting, None));
            Next::Wait(grow(backoff))
        }
        Err(NetError::Unauthorized) => {
            invalidate(inner, &token);
            Next::Wait(grow(backoff))
        }
        Err(NetError::SignedOut) => {
            if let Ok(mut core) = inner.core.lock() {
                let _ = core.store.sign_out();
            }
            if let Ok(mut current) = inner.current.lock() {
                *current = None;
            }
            set_status(
                inner,
                AccountStatus::signed_out(Some(
                    "This device was signed out from the server. Sign in again to keep syncing."
                        .to_owned(),
                )),
            );
            Next::Idle
        }
        Err(NetError::Connect(m)) => {
            set_status(inner, status(SyncPhase::Offline, Some(m)));
            Next::Wait(grow(backoff))
        }
        Err(NetError::Sync(SyncError::Forbidden(_))) => failed(
            inner,
            Some(&account.server),
            account.email.clone(),
            "This device's notes belong to another account on this server. Sign in with that \
             account to keep syncing them.",
        ),
        Err(e) => failed(
            inner,
            Some(&account.server),
            account.email.clone(),
            &e.to_string(),
        ),
    }
}

/// The server refused `token`: refresh before using it again (unless that already happened).
fn invalidate(inner: &Inner, token: &str) {
    if let Ok(mut current) = inner.current.lock()
        && current.as_ref().is_some_and(|s| s.access.token == token)
    {
        *current = None;
    }
}

fn failed(inner: &Inner, server: Option<&str>, email: Option<String>, message: &str) -> Next {
    set_status(
        inner,
        AccountStatus {
            server: server.map(str::to_owned),
            email,
            phase: SyncPhase::Failed,
            message: Some(message.to_owned()),
            server_version: None,
        },
    );
    Next::Wait(FAILED_RETRY)
}

async fn refresh(inner: &Inner, server: &Url, refresh_token: &str) -> Result<Access, HttpError> {
    let endpoint = auth::token_endpoint(server).map_err(|e| HttpError::Network(e.to_string()))?;
    let proof = auth::proof(&*inner.key, "POST", &endpoint, None, now())
        .map_err(|e| HttpError::Network(e.to_string()))?;
    let form = auth::refresh_form(refresh_token);
    let agent = agent();
    let tokens: Tokens = blocking(move || {
        read(
            agent
                .post(endpoint.as_str())
                .header("DPoP", &proof)
                .send_form(form.iter().map(|(k, v)| (*k, v.as_str()))),
        )
    })
    .await?;
    // Saved before use: the old one is spent. (The server takes the old one again for a short
    // while, in case this write is lost to a crash.)
    inner
        .core
        .lock()
        .map_err(|_| HttpError::Network("the core crashed".to_owned()))?
        .store
        .set_refresh_token(&tokens.refresh_token)
        .map_err(|e| HttpError::Network(format!("can't save the new token: {e}")))?;
    Ok(Access::new(tokens.access_token, tokens.expires_in))
}

enum JoinProblem {
    Network(String),
    Unauthorized,
    /// This device has notes, and the account has a workspace from elsewhere.
    Conflict,
}

/// Which workspace to sync: `None` for this device's own (already on the server, or becoming the
/// account's first), or the account's, for a blank device to take.
///
/// The server is asked every time rather than trusting that this device synced before: the
/// server may have lost the workspace (its data reset or restored), or this may be another
/// account. Then what this device believed the server had is forgotten, and it goes on as a
/// device that never synced.
async fn workspace_to_join(
    inner: &Inner,
    server: &Url,
    access_token: &str,
) -> Result<Option<WorkspaceId>, JoinProblem> {
    let list: Vec<WorkspaceInfo> = get_json(server, "api/v1/workspaces", access_token, &inner.key)
        .await
        .map_err(|e| match e {
            HttpError::Network(m) => JoinProblem::Network(m),
            HttpError::Refused { .. } => JoinProblem::Unauthorized,
        })?;
    let ids: Vec<(WorkspaceId, bool)> = list.iter().map(|w| (w.id, w.role == "owner")).collect();
    let mut core = inner
        .core
        .lock()
        .map_err(|_| JoinProblem::Network("the core crashed".to_owned()))?;
    match core.workspace_to_join(&ids) {
        Ok(crate::sync::Join::Own) => Ok(None),
        Ok(crate::sync::Join::Take(id)) => Ok(Some(id)),
        Ok(crate::sync::Join::Conflict) => Err(JoinProblem::Conflict),
        Err(e) => Err(JoinProblem::Network(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn typed_addresses_become_base_urls() {
        let s = |t: &str| server_url(t).map(|u| u.to_string()).ok();
        assert_eq!(
            s("notes.example").as_deref(),
            Some("https://notes.example/")
        );
        assert_eq!(
            s(" http://127.0.0.1:18080 ").as_deref(),
            Some("http://127.0.0.1:18080/")
        );
        assert_eq!(
            s("https://example.org/notes?x=1#y").as_deref(),
            Some("https://example.org/notes/")
        );
        assert_eq!(s("ftp://example.org"), None);
        assert_eq!(s(""), None);
    }

    #[test]
    fn backoff_grows_to_a_ceiling() {
        let mut b = BACKOFF_MIN;
        let mut last = Duration::ZERO;
        for _ in 0..20 {
            last = grow(&mut b);
        }
        assert!(last >= BACKOFF_MAX && last <= BACKOFF_MAX + BACKOFF_MAX / 4);
    }
}
