// SPDX-License-Identifier: AGPL-3.0-or-later

//! OAuth clients: who may ask for tokens, and where the answer may go.
//!
//! Two kinds. The Drusite app, on every platform, is built in: a public client (an app can't
//! keep a secret) that proves itself with PKCE and binds its tokens to a device key with DPoP.
//! Agents (MCP clients) register nothing here: their client ID is the URL of a JSON document
//! describing them (a Client ID Metadata Document, draft-ietf-oauth-client-id-metadata-document),
//! which the server fetches when they ask to sign in. They too are public clients with PKCE.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::app::AppState;

/// The Drusite app: desktop, Android and iOS.
pub const APP: &str = "drusite-app";

/// The web app (the browser version), served by this server. The server is its OAuth client:
/// it keeps the code verifier, redeems the code itself, and gives the browser a cookie, never a
/// token. Its one redirect is this server's own `/web/callback`, checked where the server's
/// address is known (`oauth::authorize`), and the token endpoint refuses it.
pub const WEB: &str = "drusite-web";

/// The private-use URI scheme the mobile apps catch the redirect on (RFC 8252 §7.1): the
/// app's reverse-DNS identifier.
pub const APP_SCHEME_REDIRECT: &str = "dev.drusite.app:/oauth/callback";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    App,
    /// The web app: see [`WEB`].
    Web,
    /// An MCP client, with the redirect URIs its metadata document lists.
    Agent {
        redirect_uris: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    /// What to call it on the consent page and in the activity list.
    pub name: String,
    pub kind: Kind,
}

impl Client {
    pub fn is_agent(&self) -> bool {
        matches!(self.kind, Kind::Agent { .. })
    }

    /// The client's name, with the site its ID is on, so a client can't pass itself off as
    /// another by its name alone: "Claude Code (claude.ai)".
    pub fn label(&self) -> String {
        match url::Url::parse(&self.id)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
        {
            Some(host) if self.is_agent() => format!("{} ({host})", self.name),
            _ => self.name.clone(),
        }
    }
}

fn app() -> Client {
    Client {
        id: APP.to_owned(),
        name: "Drusite".to_owned(),
        kind: Kind::App,
    }
}

/// The built-in apps, by ID. For agents, use [`resolve`].
pub fn find(id: &str) -> Option<Client> {
    match id {
        APP => Some(app()),
        WEB => Some(Client {
            id: WEB.to_owned(),
            name: "Drusite in a browser".to_owned(),
            kind: Kind::Web,
        }),
        _ => None,
    }
}

/// Fetched metadata documents, kept a few minutes.
#[derive(Debug, Default)]
pub struct ClientCache(Mutex<HashMap<String, (Instant, Client)>>);

const CACHE_FOR: Duration = Duration::from_secs(300);
/// The draft recommends at most 5 KB.
const MAX_DOCUMENT: usize = 5 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("the client ID isn't a URL this server can fetch: {0}")]
    BadId(&'static str),
    #[error("the client's metadata document couldn't be fetched: {0}")]
    Fetch(String),
    #[error("the client's metadata document isn't valid: {0}")]
    Document(String),
}

/// Finds the client `id` names: the app, or an agent whose metadata document is at `id`.
pub async fn resolve(state: &AppState, id: &str) -> Result<Client, ClientError> {
    if let Some(app) = find(id) {
        return Ok(app);
    }
    if !state.config().agents.enabled {
        return Err(ClientError::BadId("agents can't connect to this server"));
    }
    let cache = &state.auth().clients;
    if let Some((at, client)) = cache.0.lock().unwrap_or_else(|e| e.into_inner()).get(id)
        && at.elapsed() < CACHE_FOR
    {
        return Ok(client.clone());
    }
    let client = fetch(id, state.config().agents.allow_insecure_client_ids).await?;
    let mut map = cache.0.lock().unwrap_or_else(|e| e.into_inner());
    if map.len() > 1000 {
        map.retain(|_, (at, _)| at.elapsed() < CACHE_FOR);
    }
    map.insert(id.to_owned(), (Instant::now(), client.clone()));
    Ok(client)
}

#[derive(Debug, Deserialize)]
struct Document {
    client_id: String,
    client_name: Option<String>,
    #[serde(default)]
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: Option<String>,
    client_secret: Option<serde_json::Value>,
}

/// Whether an address is one an outside request must not reach: this host, the local network,
/// and the like.
fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                // Shared address space (RFC 6598) and 0/8.
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || o[0] == 0
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_internal(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique local (fc00::/7) and link-local (fe80::/10).
                || (seg[0] & 0xfe00) == 0xfc00
                || (seg[0] & 0xffc0) == 0xfe80
        }
    }
}

async fn fetch(id: &str, insecure: bool) -> Result<Client, ClientError> {
    let url = url::Url::parse(id).map_err(|_| ClientError::BadId("not a URL"))?;
    let scheme_ok = url.scheme() == "https" || (insecure && url.scheme() == "http");
    if !scheme_ok {
        return Err(ClientError::BadId("it must be an https URL"));
    }
    if url.path() == "/" || url.fragment().is_some() || !url.username().is_empty() {
        return Err(ClientError::BadId(
            "it must have a path, and no fragment or user name",
        ));
    }
    if url.path().split('/').any(|s| s == "." || s == "..") {
        return Err(ClientError::BadId("it must not have dot segments"));
    }
    let host = url
        .host_str()
        .ok_or(ClientError::BadId("it has no host"))?
        .to_owned();
    let port = url
        .port_or_known_default()
        .ok_or(ClientError::BadId("it has no port"))?;
    // Look the name up once, check every address, and connect to the one checked: a second
    // lookup could answer differently (DNS rebinding).
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| ClientError::Fetch(e.to_string()))?
        .collect();
    if addrs.is_empty() || (!insecure && addrs.iter().any(|a| is_internal(a.ip()))) {
        return Err(ClientError::BadId(
            "its host is on a private network or this server",
        ));
    }
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .resolve(&host, addrs[0])
        .user_agent(concat!("drusite-server/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| ClientError::Fetch(e.to_string()))?;
    let mut response = http
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| ClientError::Fetch(e.to_string()))?;
    if !response.status().is_success() {
        return Err(ClientError::Fetch(format!(
            "it answered {}",
            response.status()
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| ClientError::Fetch(e.to_string()))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_DOCUMENT {
            return Err(ClientError::Document("it is too long".to_owned()));
        }
    }
    let doc: Document =
        serde_json::from_slice(&body).map_err(|e| ClientError::Document(e.to_string()))?;
    if doc.client_id != id {
        return Err(ClientError::Document(
            "its client_id isn't the URL it is at".to_owned(),
        ));
    }
    if doc.client_secret.is_some()
        || doc
            .token_endpoint_auth_method
            .as_deref()
            .is_some_and(|m| m != "none")
    {
        return Err(ClientError::Document(
            "only public clients (token_endpoint_auth_method none) can connect".to_owned(),
        ));
    }
    if doc.redirect_uris.is_empty() || doc.redirect_uris.len() > 20 {
        return Err(ClientError::Document(
            "it must list between 1 and 20 redirect_uris".to_owned(),
        ));
    }
    let name = doc
        .client_name
        .map(|n| {
            n.trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect::<String>()
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| host.clone());
    Ok(Client {
        id: id.to_owned(),
        name,
        kind: Kind::Agent {
            redirect_uris: doc.redirect_uris,
        },
    })
}

fn is_loopback_url(url: &url::Url) -> bool {
    url.scheme() == "http"
        && match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(d)) => d == "localhost",
            None => false,
        }
}

/// Whether `client` may be sent back to `redirect_uri`.
///
/// The app's redirects are the private-use scheme, or a loopback address on any port (RFC
/// 8252 §7.3: desktop apps listen on a port the OS picks). `localhost` isn't accepted for the
/// app, since it can resolve elsewhere (§8.3).
///
/// An agent's are the ones its metadata document lists, exactly, except that a loopback one may
/// be on any port, as agents on a laptop listen where they can.
pub fn redirect_allowed(client: &Client, redirect_uri: &str) -> bool {
    let Ok(url) = url::Url::parse(redirect_uri) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return false;
    }
    match &client.kind {
        // Only this server's own address, which `oauth::authorize` checks.
        Kind::Web => false,
        Kind::App => {
            if redirect_uri == APP_SCHEME_REDIRECT {
                return true;
            }
            let ip_loopback = matches!(url.host(), Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)))
                && is_loopback_url(&url);
            ip_loopback && url.port().is_some() && url.path() == "/oauth/callback"
        }
        Kind::Agent { redirect_uris } => redirect_uris.iter().any(|r| {
            if r == redirect_uri {
                return true;
            }
            let Ok(registered) = url::Url::parse(r) else {
                return false;
            };
            is_loopback_url(&registered)
                && is_loopback_url(&url)
                && registered.host() == url.host()
                && registered.path() == url.path()
                && registered.query() == url.query()
        }),
    }
}

/// The scopes Drusite knows. The app asks for `read write`; agents may also ask for `journal`
/// (writing journal entries), and the person chooses on the consent page.
pub const SCOPES: [&str; 3] = ["read", "write", "journal"];

/// Checks a requested scope; an empty one means everything the app needs.
pub fn normalize_scope(scope: Option<&str>) -> Option<String> {
    let requested: Vec<&str> = scope.unwrap_or_default().split_whitespace().collect();
    if requested.is_empty() {
        return Some("read write".to_owned());
    }
    if requested.iter().any(|s| !SCOPES.contains(s)) {
        return None;
    }
    // In a fixed order, without repeats.
    Some(
        SCOPES
            .iter()
            .filter(|s| requested.contains(s))
            .copied()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn only_the_apps_redirects_are_allowed() {
        let app = find(APP).unwrap();
        for ok in [
            "http://127.0.0.1:49152/oauth/callback",
            "http://[::1]:8000/oauth/callback",
            APP_SCHEME_REDIRECT,
        ] {
            assert!(redirect_allowed(&app, ok), "{ok}");
        }
        for bad in [
            "http://localhost:49152/oauth/callback",
            "http://127.0.0.1/oauth/callback",
            "http://127.0.0.1:1234/elsewhere",
            "https://evil.example/oauth/callback",
            "http://user@127.0.0.1:1234/oauth/callback",
            "http://127.0.0.1:1234/oauth/callback#x",
            "dev.drusite.app:/other",
            "http://10.0.0.1:1234/oauth/callback",
        ] {
            assert!(!redirect_allowed(&app, bad), "{bad}");
        }
        assert!(find("someone-else").is_none());
    }

    #[test]
    fn an_agents_redirects_are_its_own_on_any_loopback_port() {
        let agent = Client {
            id: "https://agent.example/client.json".into(),
            name: "Agent".into(),
            kind: Kind::Agent {
                redirect_uris: vec![
                    "http://localhost:3118/callback".into(),
                    "https://agent.example/done".into(),
                ],
            },
        };
        for ok in [
            "http://localhost:3118/callback",
            "http://localhost:50000/callback",
            "https://agent.example/done",
        ] {
            assert!(redirect_allowed(&agent, ok), "{ok}");
        }
        for bad in [
            "http://127.0.0.1:3118/callback",
            "http://localhost:3118/other",
            "https://agent.example/done?x=1",
            "https://agent.example:444/done",
            "https://evil.example/done",
        ] {
            assert!(!redirect_allowed(&agent, bad), "{bad}");
        }
        assert_eq!(agent.label(), "Agent (agent.example)");
    }

    #[test]
    fn internal_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "0.0.0.0",
        ] {
            assert!(is_internal(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "2606:4700::1111"] {
            assert!(!is_internal(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn scopes() {
        assert_eq!(normalize_scope(None).unwrap(), "read write");
        assert_eq!(
            normalize_scope(Some("write read read")).unwrap(),
            "read write"
        );
        assert_eq!(normalize_scope(Some("read")).unwrap(), "read");
        assert_eq!(
            normalize_scope(Some("journal read")).unwrap(),
            "read journal"
        );
        assert!(normalize_scope(Some("admin")).is_none());
    }
}
