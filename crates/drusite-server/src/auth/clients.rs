// SPDX-License-Identifier: AGPL-3.0-or-later

//! OAuth clients: who may ask for tokens, and where the answer may go.
//!
//! For now there is one, the Drusite app on every platform. It's a public client (an app can't
//! keep a secret), so it proves itself with PKCE and binds its tokens to a device key with
//! DPoP. MCP clients will register with Client ID Metadata Documents in Phase 3.

/// The Drusite app: desktop, Android and iOS.
pub const APP: &str = "drusite-app";

/// The private-use URI scheme the mobile apps catch the redirect on (RFC 8252 §7.1): the
/// app's reverse-DNS identifier.
pub const APP_SCHEME_REDIRECT: &str = "dev.drusite.app:/oauth/callback";

#[derive(Debug, Clone, Copy)]
pub struct Client {
    pub id: &'static str,
    pub name: &'static str,
}

pub fn find(id: &str) -> Option<Client> {
    (id == APP).then_some(Client {
        id: APP,
        name: "Drusite",
    })
}

/// Whether `client` may be sent back to `redirect_uri`.
///
/// The app's redirects are the private-use scheme, or a loopback address on any port (RFC
/// 8252 §7.3: desktop apps listen on a port the OS picks). `localhost` isn't accepted, since
/// it can resolve elsewhere (§8.3).
pub fn redirect_allowed(client: &Client, redirect_uri: &str) -> bool {
    if client.id != APP {
        return false;
    }
    if redirect_uri == APP_SCHEME_REDIRECT {
        return true;
    }
    let Ok(url) = url::Url::parse(redirect_uri) else {
        return false;
    };
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    url.scheme() == "http"
        && loopback
        && url.port().is_some()
        && url.path() == "/oauth/callback"
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

/// The scopes Drusite knows. Phase 3 adds per-workspace scopes for agents.
pub const SCOPES: [&str; 2] = ["read", "write"];

/// Checks a requested scope; an empty one means everything the app needs.
pub fn normalize_scope(scope: Option<&str>) -> Option<String> {
    let requested: Vec<&str> = scope.unwrap_or_default().split_whitespace().collect();
    if requested.is_empty() {
        return Some(SCOPES.join(" "));
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
    fn scopes() {
        assert_eq!(normalize_scope(None).unwrap(), "read write");
        assert_eq!(
            normalize_scope(Some("write read read")).unwrap(),
            "read write"
        );
        assert_eq!(normalize_scope(Some("read")).unwrap(), "read");
        assert!(normalize_scope(Some("admin")).is_none());
    }
}
