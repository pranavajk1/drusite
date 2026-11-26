// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in to a Drusite server: the client side of its OAuth 2.1 flow.
//!
//! The app opens [`Authorization::url`] in the system browser, catches the redirect (on a
//! loopback port on desktops, on the `dev.drusite.app:` scheme on phones), checks it with
//! [`Authorization::code_from`], and swaps the code for tokens at the token endpoint with a
//! DPoP proof. Everything here is pure; the app does the HTTP, so it works the same on every
//! platform and in tests.
//!
//! Proofs are made per connection and per refresh, never per edit: a phone's StrongBox takes
//! 70–90 ms a signature.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use drusite_dpop::{DpopError, Jwk, ProofRequest};
use sha2::{Digest as _, Sha256};
use url::Url;

/// The app's OAuth client ID.
pub const CLIENT_ID: &str = "drusite-app";

/// Where the server sends phones back to.
pub const APP_REDIRECT: &str = "dev.drusite.app:/oauth/callback";

/// Where the server sends a desktop back to: a loopback port the app listens on.
pub fn loopback_redirect(port: u16) -> String {
    format!("http://127.0.0.1:{port}/oauth/callback")
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("the OS has no randomness")]
    Random,
    #[error("the sign-in answer is for another request")]
    StateMismatch,
    #[error("the sign-in answer comes from another server")]
    IssuerMismatch,
    #[error("the server refused the sign-in: {0}")]
    Refused(String),
    #[error("the sign-in answer is malformed")]
    Malformed,
    #[error(transparent)]
    Dpop(#[from] DpopError),
}

fn random_text() -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| AuthError::Random)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// One sign-in in progress.
#[derive(Debug, Clone)]
pub struct Authorization {
    server: Url,
    redirect_uri: String,
    state: String,
    verifier: String,
    url: Url,
}

impl Authorization {
    /// Starts a sign-in to `server` (its public URL), coming back to `redirect_uri`, bound to
    /// the device key with thumbprint `jkt` from the start (RFC 9449 §10).
    pub fn new(server: &Url, redirect_uri: &str, jkt: &str) -> Result<Self, AuthError> {
        let verifier = random_text()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_text()?;
        let mut url = server
            .join("oauth/authorize")
            .map_err(|_| AuthError::Malformed)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("scope", "read write")
            .append_pair("dpop_jkt", jkt);
        Ok(Self {
            server: server.clone(),
            redirect_uri: redirect_uri.to_owned(),
            state,
            verifier,
            url,
        })
    }

    /// The server this sign-in is for.
    pub fn server(&self) -> &Url {
        &self.server
    }

    /// The page to open in the system browser.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The code from the redirect the browser came back with, once it's checked: the right
    /// `state`, and the right server (RFC 9207), so another server can't slip in its code.
    pub fn code_from(&self, callback: &Url) -> Result<String, AuthError> {
        let get = |name: &str| {
            callback
                .query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        if get("state").as_deref() != Some(self.state.as_str()) {
            return Err(AuthError::StateMismatch);
        }
        if get("iss").as_deref() != Some(self.server.as_str().trim_end_matches('/')) {
            return Err(AuthError::IssuerMismatch);
        }
        if let Some(error) = get("error") {
            let description = get("error_description").unwrap_or_default();
            return Err(AuthError::Refused(format!("{error}: {description}")));
        }
        get("code").ok_or(AuthError::Malformed)
    }

    /// The form to post to the token endpoint for `code`.
    pub fn token_form(&self, code: &str, device_name: &str) -> Vec<(&'static str, String)> {
        vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code.to_owned()),
            ("redirect_uri", self.redirect_uri.clone()),
            ("code_verifier", self.verifier.clone()),
            ("client_id", CLIENT_ID.to_owned()),
            ("device_name", device_name.to_owned()),
        ]
    }
}

/// The form to post to the token endpoint to refresh.
pub fn refresh_form(refresh_token: &str) -> Vec<(&'static str, String)> {
    vec![
        ("grant_type", "refresh_token".to_owned()),
        ("refresh_token", refresh_token.to_owned()),
        ("client_id", CLIENT_ID.to_owned()),
    ]
}

/// The token endpoint of `server`.
pub fn token_endpoint(server: &Url) -> Result<Url, AuthError> {
    server.join("oauth/token").map_err(|_| AuthError::Malformed)
}

/// A device key: the platform's secure hardware where there is one. It only signs bytes.
pub trait DeviceKey {
    fn jwk(&self) -> Jwk;
    /// An ECDSA P-256 signature over `message` (SHA-256), DER or raw `r || s`.
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthError>;
}

impl DeviceKey for drusite_dpop::SoftwareKey {
    fn jwk(&self) -> Jwk {
        Self::jwk(self)
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthError> {
        Ok(Self::sign(self, message).to_vec())
    }
}

/// A DPoP proof for one request: `method` on `url`, carrying `access_token` if there is one.
pub fn proof(
    key: &(impl DeviceKey + ?Sized),
    method: &str,
    url: &Url,
    access_token: Option<&str>,
    now: i64,
) -> Result<String, AuthError> {
    let mut htu = url.clone();
    htu.set_query(None);
    htu.set_fragment(None);
    let input = drusite_dpop::signing_input(
        &key.jwk(),
        &ProofRequest {
            htm: method,
            htu: htu.as_str(),
            iat: now,
            access_token,
            nonce: None,
        },
    )?;
    let signature = key.sign(input.as_bytes())?;
    Ok(drusite_dpop::finish(&input, &signature)?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_callback_must_match_state_and_server() {
        let server = Url::parse("https://notes.example/").unwrap();
        let key = drusite_dpop::SoftwareKey::generate().unwrap();
        let auth = Authorization::new(&server, APP_REDIRECT, &key.jwk().thumbprint()).unwrap();
        let q = |s: &str| Url::parse(&format!("{APP_REDIRECT}?{s}")).unwrap();
        let state = auth.state.clone();
        assert_eq!(
            auth.code_from(&q(&format!(
                "code=c1&state={state}&iss=https://notes.example"
            )))
            .unwrap(),
            "c1"
        );
        assert!(matches!(
            auth.code_from(&q("code=c1&state=other&iss=https://notes.example")),
            Err(AuthError::StateMismatch)
        ));
        assert!(matches!(
            auth.code_from(&q(&format!(
                "code=c1&state={state}&iss=https://evil.example"
            ))),
            Err(AuthError::IssuerMismatch)
        ));
        assert!(matches!(
            auth.code_from(&q(&format!(
                "error=access_denied&state={state}&iss=https://notes.example"
            ))),
            Err(AuthError::Refused(_))
        ));
        assert!(auth.url().as_str().contains("code_challenge_method=S256"));
    }

    #[test]
    fn proofs_verify_for_the_request() {
        let key = drusite_dpop::SoftwareKey::generate().unwrap();
        let url = Url::parse("https://notes.example/oauth/token?x=1").unwrap();
        let p = proof(&key, "POST", &url, None, 1000).unwrap();
        let ok = drusite_dpop::verify(
            &p,
            &drusite_dpop::Expected {
                htm: "POST",
                htu: "https://notes.example/oauth/token",
                now: 1000,
                max_skew: 60,
                access_token: None,
            },
        )
        .unwrap();
        assert_eq!(ok.jkt, key.jwk().thumbprint());
    }
}
