// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identity end to end: a real server on a loopback port, a browser that keeps cookies, and an
//! app that signs in with OAuth 2.1, PKCE and a DPoP device key, for every login method.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use common::browser::{Browser, location};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use drusite_core::auth::{self as client, Authorization, DeviceKey};
use drusite_dpop::SoftwareKey;
use reqwest::StatusCode;
use reqwest::header::LOCATION;
use serde_json::{Value, json};
use url::Url;

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// The app's half: a device key, and the OAuth client from drusite-core.
struct App {
    server: Url,
    key: SoftwareKey,
    http: reqwest::Client,
}

#[derive(Debug, Clone)]
struct Tokens {
    access: String,
    refresh: String,
}

impl App {
    fn new(server: &Url) -> Self {
        Self {
            server: server.clone(),
            key: SoftwareKey::generate().unwrap(),
            http: reqwest::Client::new(),
        }
    }

    fn start(&self) -> Authorization {
        Authorization::new(
            &self.server,
            &client::loopback_redirect(49152),
            &self.key.jwk().thumbprint(),
        )
        .unwrap()
    }

    async fn token(&self, form: &[(&str, String)], key: &impl DeviceKey) -> (StatusCode, Value) {
        let endpoint = client::token_endpoint(&self.server).unwrap();
        let proof = client::proof(key, "POST", &endpoint, None, now()).unwrap();
        let response = self
            .http
            .post(endpoint)
            .header("dpop", proof)
            .form(form)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.headers()["cache-control"],
            "no-store",
            "token responses aren't cached"
        );
        let status = response.status();
        (status, response.json().await.unwrap())
    }

    async fn exchange(&self, auth: &Authorization, code: &str) -> Tokens {
        let (status, body) = self
            .token(&auth.token_form(code, "Test Mac"), &self.key)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["token_type"], "DPoP");
        Tokens {
            access: body["access_token"].as_str().unwrap().to_owned(),
            refresh: body["refresh_token"].as_str().unwrap().to_owned(),
        }
    }

    async fn refresh(&self, refresh: &str) -> (StatusCode, Value) {
        self.token(&client::refresh_form(refresh), &self.key).await
    }

    /// `GET /api/v1/me` with the access token and a fresh proof.
    async fn me(&self, access: &str) -> (StatusCode, Value) {
        let url = self.server.join("api/v1/me").unwrap();
        let proof = client::proof(&self.key, "GET", &url, Some(access), now()).unwrap();
        self.me_with(access, &proof).await
    }

    async fn me_with(&self, access: &str, proof: &str) -> (StatusCode, Value) {
        let url = self.server.join("api/v1/me").unwrap();
        let response = self
            .http
            .get(url)
            .header("authorization", format!("DPoP {access}"))
            .header("dpop", proof)
            .send()
            .await
            .unwrap();
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            assert!(
                response.headers()["www-authenticate"]
                    .to_str()
                    .unwrap()
                    .starts_with("DPoP ")
            );
        }
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

/// Opens the app's authorize URL in a browser with no session: it lands on the sign-in page.
/// Returns the waiting request's ID.
async fn open_authorize(browser: &mut Browser, auth: &Authorization) -> String {
    let base = browser.base.clone();
    let response = browser.get(auth.url()).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let login = location(&response, &base);
    assert_eq!(login.path(), "/login");
    login
        .query_pairs()
        .find(|(k, _)| k == "request")
        .unwrap()
        .1
        .into_owned()
}

/// Follows a signed-in step's `next` back to the app; returns the code.
async fn finish(browser: &mut Browser, auth: &Authorization, step: &Value) -> String {
    assert_eq!(step["status"], "signed_in", "{step}");
    let base = browser.base.clone();
    let next = base.join(step["next"].as_str().unwrap()).unwrap();
    let response = browser.get(&next).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let callback = location(&response, &base);
    auth.code_from(&callback).unwrap()
}

fn password_server() -> Vec<(&'static str, &'static str)> {
    vec![
        ("DRUSITE__AUTH__PASSWORD__ENABLED", "true"),
        ("DRUSITE__AUTH__SELF_SIGNUP", "true"),
    ]
}

async fn signup(browser: &mut Browser, email: &str, request: Option<&str>) -> Value {
    let (status, body) = browser
        .post(
            "/api/v1/auth/password/signup",
            json!({ "email": email, "password": "correct horse battery", "name": "Ada",
                    "request": request }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn an_app_signs_in_and_its_tokens_are_bound_to_its_key() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let app = App::new(&server.url);
    let mut browser = Browser::new(&server.url);

    let metadata: Value = reqwest::get(
        server
            .url
            .join(".well-known/oauth-authorization-server")
            .unwrap(),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        metadata["code_challenge_methods_supported"],
        json!(["S256"])
    );
    assert_eq!(
        metadata["dpop_signing_alg_values_supported"],
        json!(["ES256"])
    );

    // Sign up in the browser the app opened, then back to the app with a code.
    let auth = app.start();
    let request = open_authorize(&mut browser, &auth).await;
    let (status, info) = browser
        .get(&browser.url(&format!("/api/v1/auth/request/{request}")))
        .await
        .json::<Value>()
        .await
        .map(|v| (StatusCode::OK, v))
        .unwrap();
    assert_eq!(
        (status, info["client"].clone()),
        (StatusCode::OK, json!("Drusite"))
    );
    let step = signup(&mut browser, "Ada@Example.com", Some(&request)).await;
    let code = finish(&mut browser, &auth, &step).await;

    // A wrong PKCE verifier gets nothing, and doesn't spend the code.
    let other = app.start();
    let (status, body) = app.token(&other.token_form(&code, "x"), &app.key).await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::BAD_REQUEST, json!("invalid_grant"))
    );

    // The code is bound to the app's key from the start: another key can't redeem it.
    let thief = SoftwareKey::generate().unwrap();
    let (status, body) = app.token(&auth.token_form(&code, "x"), &thief).await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::BAD_REQUEST, json!("invalid_dpop_proof"))
    );

    let tokens = app.exchange(&auth, &code).await;
    let (status, me) = app.me(&tokens.access).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["email"], "Ada@Example.com");
    assert_eq!(me["emailVerified"], false);

    // Replaying a proof, a proof from another key, a Bearer token, a proof for another URL:
    // all refused.
    let url = server.url.join("api/v1/me").unwrap();
    let proof = client::proof(&app.key, "GET", &url, Some(&tokens.access), now()).unwrap();
    assert_eq!(app.me_with(&tokens.access, &proof).await.0, StatusCode::OK);
    assert_eq!(
        app.me_with(&tokens.access, &proof).await.0,
        StatusCode::UNAUTHORIZED
    );
    let stolen = client::proof(&thief, "GET", &url, Some(&tokens.access), now()).unwrap();
    assert_eq!(
        app.me_with(&tokens.access, &stolen).await.0,
        StatusCode::UNAUTHORIZED
    );
    let elsewhere = client::proof(
        &app.key,
        "GET",
        &server.url.join("api/v1/other").unwrap(),
        Some(&tokens.access),
        now(),
    )
    .unwrap();
    assert_eq!(
        app.me_with(&tokens.access, &elsewhere).await.0,
        StatusCode::UNAUTHORIZED
    );
    let old = client::proof(&app.key, "GET", &url, Some(&tokens.access), now() - 600).unwrap();
    assert_eq!(
        app.me_with(&tokens.access, &old).await.0,
        StatusCode::UNAUTHORIZED
    );
    let bearer = reqwest::Client::new()
        .get(url.clone())
        .header("authorization", format!("Bearer {}", tokens.access))
        .send()
        .await
        .unwrap();
    assert_eq!(bearer.status(), StatusCode::UNAUTHORIZED);

    // The code works once; using it again revokes what it gave.
    let (status, _) = app.token(&auth.token_form(&code, "x"), &app.key).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(app.me(&tokens.access).await.0, StatusCode::UNAUTHORIZED);

    // A fresh sign-in: the browser is still signed in, so it goes straight back to the app.
    let auth = app.start();
    let response = browser.get(auth.url()).await;
    let code = auth.code_from(&location(&response, &server.url)).unwrap();
    let first = app.exchange(&auth, &code).await;

    // Refresh rotates both tokens; the old access token stops working.
    let (status, body) = app.refresh(&first.refresh).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let second = Tokens {
        access: body["access_token"].as_str().unwrap().to_owned(),
        refresh: body["refresh_token"].as_str().unwrap().to_owned(),
    };
    assert_ne!(second.refresh, first.refresh);
    assert_eq!(app.me(&first.access).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(app.me(&second.access).await.0, StatusCode::OK);

    // A refresh token is useless without the key.
    let (status, body) = app
        .token(&client::refresh_form(&second.refresh), &thief)
        .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::BAD_REQUEST, json!("invalid_dpop_proof"))
    );
    assert_eq!(
        app.me(&second.access).await.0,
        StatusCode::OK,
        "a stolen token doesn't hurt the device"
    );

    // Within a minute, the device may retry a refresh whose answer it lost: it gets a new
    // pair, and the pair it never received stops working.
    let (status, body) = app.refresh(&first.refresh).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let third = Tokens {
        access: body["access_token"].as_str().unwrap().to_owned(),
        refresh: body["refresh_token"].as_str().unwrap().to_owned(),
    };
    // `second` was retired unused, so it's simply unknown now; the chain goes on from `third`.
    assert_eq!(
        app.refresh(&second.refresh).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(app.me(&third.access).await.0, StatusCode::OK);

    // Later than that, an old refresh token coming back means two copies: the grant is revoked.
    sqlx::query("UPDATE refresh_tokens SET used_at = now() - interval '5 minutes' WHERE used_at IS NOT NULL")
        .execute(server.db.db.pool())
        .await
        .unwrap();
    let (status, body) = app.refresh(&first.refresh).await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::BAD_REQUEST, json!("invalid_grant"))
    );
    assert_eq!(app.me(&third.access).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(app.refresh(&third.refresh).await.0, StatusCode::BAD_REQUEST);

    // Revoking from the app signs the device out.
    let auth = app.start();
    let response = browser.get(auth.url()).await;
    let code = auth.code_from(&location(&response, &server.url)).unwrap();
    let tokens = app.exchange(&auth, &code).await;
    let revoked = reqwest::Client::new()
        .post(server.url.join("oauth/revoke").unwrap())
        .form(&[("token", tokens.refresh.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);
    assert_eq!(app.me(&tokens.access).await.0, StatusCode::UNAUTHORIZED);
}

type Pairs = Vec<(String, String)>;

#[tokio::test]
async fn bad_authorization_requests_never_redirect_to_unchecked_places() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let mut browser = Browser::new(&server.url);
    let app = App::new(&server.url);
    let good = app.start();
    let with = |change: &dyn Fn(&mut Pairs)| {
        let mut pairs: Vec<(String, String)> = good
            .url()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        change(&mut pairs);
        let mut url = good.url().clone();
        url.query_pairs_mut().clear().extend_pairs(pairs);
        url
    };
    let set = |key: &'static str, value: &'static str| {
        move |p: &mut Vec<(String, String)>| {
            p.retain(|(k, _)| k != key);
            p.push((key.to_owned(), value.to_owned()));
        }
    };

    // Unknown client or redirect: an error page, not a redirect.
    for url in [
        with(&set("client_id", "evil")),
        with(&set("redirect_uri", "https://evil.example/oauth/callback")),
        with(&set("redirect_uri", "http://localhost:4000/oauth/callback")),
    ] {
        let response = browser.get(&url).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{url}");
        assert!(response.headers().get(LOCATION).is_none());
    }
    // Known client and redirect, bad request: sent back with an error.
    for (url, error) in [
        (
            with(&set("code_challenge_method", "plain")),
            "invalid_request",
        ),
        (
            with(&|p| p.retain(|(k, _)| k != "code_challenge")),
            "invalid_request",
        ),
        (with(&set("scope", "admin")), "invalid_scope"),
        (
            with(&set("response_type", "token")),
            "unsupported_response_type",
        ),
        (
            with(&set("resource", "https://other.example")),
            "invalid_target",
        ),
    ] {
        let response = browser.get(&url).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let back = location(&response, &server.url);
        assert_eq!(back.host_str(), Some("127.0.0.1"));
        assert!(
            matches!(good.code_from(&back), Err(client::AuthError::Refused(e)) if e.starts_with(error)),
            "{url}"
        );
    }
}

#[tokio::test]
async fn password_logins_are_limited_and_can_require_totp() {
    let mut overrides = password_server();
    overrides.push(("DRUSITE__AUTH__PASSWORD__REQUIRE_TOTP", "true"));
    let Some(server) = common::serve(&overrides).await else {
        return;
    };
    let mut browser = Browser::new(&server.url);

    // Sign-up wants an authenticator before it completes.
    let (status, setup) = browser
        .post(
            "/api/v1/auth/password/signup",
            json!({ "email": "grace@example.com", "password": "correct horse battery" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(setup["status"], "totp_setup_required", "{setup}");
    assert!(
        setup["otpauthUri"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/")
    );
    let secret = base32_decode(setup["secret"].as_str().unwrap());
    let code_at = |offset: i64| {
        let step = u64::try_from(now() + offset).unwrap() / drusite_server::auth::totp::STEP;
        format!("{:06}", drusite_server::auth::totp::code(&secret, step))
    };
    let (status, _) = browser
        .post(
            "/api/v1/auth/password/totp-setup",
            json!({ "challenge": setup["challenge"], "code": "000000" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, done) = browser
        .post(
            "/api/v1/auth/password/totp-setup",
            json!({ "challenge": setup["challenge"], "code": code_at(0) }),
        )
        .await;
    assert_eq!(
        (status, done["status"].clone()),
        (StatusCode::OK, json!("signed_in"))
    );

    // Next time: password, then a code. The code just used can't be used again.
    let mut other = Browser::new(&server.url);
    let (status, step) = other
        .post(
            "/api/v1/auth/password/login",
            json!({ "email": "GRACE@example.com", "password": "correct horse battery" }),
        )
        .await;
    assert_eq!(
        (status, step["status"].clone()),
        (StatusCode::OK, json!("totp_required"))
    );
    let (status, _) = other
        .post(
            "/api/v1/auth/password/totp",
            json!({ "challenge": step["challenge"], "code": code_at(0) }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a code works once");
    let (status, done) = other
        .post(
            "/api/v1/auth/password/totp",
            json!({ "challenge": step["challenge"], "code": code_at(30) }),
        )
        .await;
    assert_eq!(
        (status, done["status"].clone()),
        (StatusCode::OK, json!("signed_in")),
        "{done}"
    );

    // Wrong passwords and unknown emails look the same, and are limited.
    let mut attacker = Browser::new(&server.url);
    let (status, unknown) = attacker
        .post(
            "/api/v1/auth/password/login",
            json!({ "email": "nobody@example.com", "password": "whatever-whatever" }),
        )
        .await;
    let (_, wrong) = attacker
        .post(
            "/api/v1/auth/password/login",
            json!({ "email": "grace@example.com", "password": "whatever-whatever" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown, wrong);
    for _ in 0..9 {
        attacker
            .post(
                "/api/v1/auth/password/login",
                json!({ "email": "grace@example.com", "password": "whatever-whatever" }),
            )
            .await;
    }
    let (status, _) = attacker
        .post(
            "/api/v1/auth/password/login",
            json!({ "email": "grace@example.com", "password": "correct horse battery" }),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // Requests from other sites are refused.
    let (status, _) = attacker
        .post_from(
            "/api/v1/auth/password/login",
            json!({ "email": "a@b.c", "password": "x" }),
            "https://evil.example",
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Signing up needs a sensible password, and an email nobody has.
    let (status, body) = attacker
        .post(
            "/api/v1/auth/password/signup",
            json!({ "email": "new@example.com", "password": "short" }),
        )
        .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::BAD_REQUEST, json!("weak_password"))
    );
    let (status, body) = attacker
        .post(
            "/api/v1/auth/password/signup",
            json!({ "email": "Grace@Example.com", "password": "another long password" }),
        )
        .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::BAD_REQUEST, json!("account_exists"))
    );
}

fn base32_decode(text: &str) -> Vec<u8> {
    const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = Vec::new();
    let (mut buffer, mut bits) = (0u32, 0);
    for c in text.chars() {
        buffer = (buffer << 5) | u32::try_from(ALPHABET.find(c).unwrap()).unwrap();
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xff).unwrap());
        }
    }
    out
}

fn magic_server() -> Vec<(&'static str, &'static str)> {
    vec![
        ("DRUSITE__AUTH__MAGIC_LINK__ENABLED", "true"),
        ("DRUSITE__SMTP__HOST", "smtp.invalid"),
        ("DRUSITE__SMTP__FROM", "Drusite <notes@example.com>"),
    ]
}

async fn wait_for_mail(server: &common::Server, count: usize) -> String {
    for _ in 0..100 {
        let sent = server.sent.lock().unwrap().clone();
        if sent.len() >= count {
            return sent[count - 1].body.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("no email was sent");
}

fn token_in(body: &str) -> String {
    body.split("#token=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn magic_links_sign_in_once_and_verify_the_email() {
    let mut overrides = magic_server();
    overrides.push(("DRUSITE__AUTH__SELF_SIGNUP", "true"));
    let Some(server) = common::serve(&overrides).await else {
        return;
    };
    let app = App::new(&server.url);
    let mut browser = Browser::new(&server.url);
    let auth = app.start();
    let request = open_authorize(&mut browser, &auth).await;

    let (status, _) = browser
        .post(
            "/api/v1/auth/magic-link/start",
            json!({ "email": "lin@example.com", "request": request }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let mail = wait_for_mail(&server, 1).await;
    assert_eq!(server.sent.lock().unwrap()[0].to, "lin@example.com");
    let token = token_in(&mail);
    assert!(mail.contains(&format!("{}auth/magic#token=", server.url)));

    // Looking doesn't use it; the page asks first.
    let (status, peek) = browser
        .post("/api/v1/auth/magic-link/peek", json!({ "token": token }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        peek,
        json!({ "email": "lin@example.com", "newAccount": true })
    );
    let (status, step) = browser
        .post(
            "/api/v1/auth/magic-link/finish",
            json!({ "token": token, "name": "Lin" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{step}");
    let code = finish(&mut browser, &auth, &step).await;
    let tokens = app.exchange(&auth, &code).await;
    let (_, me) = app.me(&tokens.access).await;
    assert_eq!(
        (me["email"].clone(), me["emailVerified"].clone()),
        (json!("lin@example.com"), json!(true))
    );

    // Once only.
    let (status, body) = browser
        .post("/api/v1/auth/magic-link/finish", json!({ "token": token }))
        .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::BAD_REQUEST, json!("link_used"))
    );

    // Sends are limited per address.
    for _ in 0..2 {
        browser
            .post(
                "/api/v1/auth/magic-link/start",
                json!({ "email": "lin@example.com" }),
            )
            .await;
    }
    let (status, _) = browser
        .post(
            "/api/v1/auth/magic-link/start",
            json!({ "email": "lin@example.com" }),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn magic_links_dont_say_who_has_an_account() {
    let Some(server) = common::serve(&magic_server()).await else {
        return;
    };
    let mut browser = Browser::new(&server.url);
    // Sign-up is off and nobody has this address: the same answer, and no email.
    let (status, body) = browser
        .post(
            "/api/v1/auth/magic-link/start",
            json!({ "email": "stranger@example.com" }),
        )
        .await;
    assert_eq!(
        (status, body),
        (StatusCode::ACCEPTED, json!({ "status": "sent" }))
    );
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(server.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn recovery_links_from_the_cli_sign_in_once() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let (_, token) = drusite_server::account::create(
        server.state.db(),
        "ops@example.com",
        "Ops",
        time::Duration::minutes(10),
    )
    .await
    .unwrap();
    let mut browser = Browser::new(&server.url);
    let (status, peek) = browser
        .post("/api/v1/auth/recover/peek", json!({ "token": token }))
        .await;
    assert_eq!(
        (status, peek["email"].clone()),
        (StatusCode::OK, json!("ops@example.com"))
    );
    let (status, step) = browser
        .post("/api/v1/auth/recover", json!({ "token": token }))
        .await;
    assert_eq!(
        (status, step["next"].clone()),
        (StatusCode::OK, json!("/account"))
    );
    let (status, me) = browser
        .get(&browser.url("/api/v1/account"))
        .await
        .json::<Value>()
        .await
        .map(|v| (StatusCode::OK, v))
        .unwrap();
    assert_eq!(
        (status, me["email"].clone(), me["hasPassword"].clone()),
        (StatusCode::OK, json!("ops@example.com"), json!(false))
    );

    // Now signed in, set a first password without an old one.
    let (status, body) = browser
        .post(
            "/api/v1/account/password",
            json!({ "password": "a long new password" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Changing it needs the old one.
    let (status, _) = browser
        .post(
            "/api/v1/account/password",
            json!({ "password": "another long password" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = browser
        .post("/api/v1/auth/recover", json!({ "token": token }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Signing out ends the browser session.
    let (status, _) = browser.post("/api/v1/account/logout", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let response = browser.get(&browser.url("/api/v1/account")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_account_page_lists_and_signs_out_devices() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let app = App::new(&server.url);
    let mut browser = Browser::new(&server.url);
    let auth = app.start();
    let request = open_authorize(&mut browser, &auth).await;
    let step = signup(&mut browser, "dev@example.com", Some(&request)).await;
    let code = finish(&mut browser, &auth, &step).await;
    let tokens = app.exchange(&auth, &code).await;

    let devices: Value = browser
        .get(&browser.url("/api/v1/account/devices"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(devices.as_array().unwrap().len(), 1);
    assert_eq!(devices[0]["name"], "Test Mac");
    let id = devices[0]["id"].as_str().unwrap();

    // Someone else can't sign it out.
    let mut other = Browser::new(&server.url);
    signup(&mut other, "other@example.com", None).await;
    assert_eq!(
        other.delete(&format!("/api/v1/account/devices/{id}")).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(app.me(&tokens.access).await.0, StatusCode::OK);

    assert_eq!(
        browser
            .delete(&format!("/api/v1/account/devices/{id}"))
            .await,
        StatusCode::OK
    );
    assert_eq!(app.me(&tokens.access).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.refresh(&tokens.refresh).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn disabled_accounts_lose_their_devices_at_once() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let app = App::new(&server.url);
    let mut browser = Browser::new(&server.url);
    let auth = app.start();
    let request = open_authorize(&mut browser, &auth).await;
    let step = signup(&mut browser, "gone@example.com", Some(&request)).await;
    let code = finish(&mut browser, &auth, &step).await;
    let tokens = app.exchange(&auth, &code).await;
    sqlx::query("UPDATE users SET disabled_at = now() WHERE email = 'gone@example.com'")
        .execute(server.db.db.pool())
        .await
        .unwrap();
    assert_eq!(app.me(&tokens.access).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.refresh(&tokens.refresh).await.0,
        StatusCode::BAD_REQUEST
    );
    let (status, _) = Browser::new(&server.url)
        .post(
            "/api/v1/auth/password/login",
            json!({ "email": "gone@example.com", "password": "correct horse battery" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

mod passkey {
    //! A software authenticator: enough of WebAuthn to register and use a passkey, with a
    //! P-256 key and "none" attestation.

    use super::*;
    use p256::ecdsa::signature::Signer as _;
    use sha2::{Digest as _, Sha256};

    pub(super) struct Authenticator {
        key: p256::ecdsa::SigningKey,
        pub(super) credential_id: Vec<u8>,
        user_handle: Option<Vec<u8>>,
        origin: String,
        rp_id: String,
        count: u32,
    }

    fn cbor_head(major: u8, n: u64, out: &mut Vec<u8>) {
        let m = major << 5;
        match n {
            0..=23 => out.push(m | u8::try_from(n).unwrap()),
            24..=0xff => out.extend([m | 24, u8::try_from(n).unwrap()]),
            _ => {
                out.push(m | 25);
                out.extend(u16::try_from(n).unwrap().to_be_bytes());
            }
        }
    }

    fn cbor_int(i: i64, out: &mut Vec<u8>) {
        if i >= 0 {
            cbor_head(0, u64::try_from(i).unwrap(), out);
        } else {
            cbor_head(1, u64::try_from(-1 - i).unwrap(), out);
        }
    }

    fn cbor_bytes(b: &[u8], out: &mut Vec<u8>) {
        cbor_head(2, b.len() as u64, out);
        out.extend(b);
    }

    fn cbor_text(t: &str, out: &mut Vec<u8>) {
        cbor_head(3, t.len() as u64, out);
        out.extend(t.as_bytes());
    }

    impl Authenticator {
        pub(super) fn new(server: &Url) -> Self {
            let key =
                p256::ecdsa::SigningKey::from_slice(&SoftwareKey::generate().unwrap().to_bytes())
                    .unwrap();
            Self {
                key,
                credential_id: (0..32).map(|_| fastrand::u8(..)).collect(),
                user_handle: None,
                origin: server.origin().ascii_serialization(),
                rp_id: server.host_str().unwrap().to_owned(),
                count: 0,
            }
        }

        fn client_data(&self, kind: &str, challenge: &str) -> Vec<u8> {
            json!({ "type": kind, "challenge": challenge, "origin": self.origin, "crossOrigin": false })
                .to_string()
                .into_bytes()
        }

        fn auth_data(&mut self, with_credential: bool) -> Vec<u8> {
            self.count += 1;
            let mut data = Sha256::digest(self.rp_id.as_bytes()).to_vec();
            // User present, user verified, and attested credential data when registering.
            data.push(0x01 | 0x04 | if with_credential { 0x40 } else { 0 });
            data.extend(self.count.to_be_bytes());
            if with_credential {
                data.extend([0u8; 16]);
                data.extend(
                    u16::try_from(self.credential_id.len())
                        .unwrap()
                        .to_be_bytes(),
                );
                data.extend(&self.credential_id);
                let point = self.key.verifying_key().to_sec1_point(false);
                let point = point.as_bytes();
                let mut cose = Vec::new();
                cbor_head(5, 5, &mut cose);
                cbor_int(1, &mut cose);
                cbor_int(2, &mut cose); // kty: EC2
                cbor_int(3, &mut cose);
                cbor_int(-7, &mut cose); // alg: ES256
                cbor_int(-1, &mut cose);
                cbor_int(1, &mut cose); // crv: P-256
                cbor_int(-2, &mut cose);
                cbor_bytes(&point[1..33], &mut cose);
                cbor_int(-3, &mut cose);
                cbor_bytes(&point[33..65], &mut cose);
                data.extend(cose);
            }
            data
        }

        /// `navigator.credentials.create`, answered as the browser would send it.
        pub(super) fn create(&mut self, options: &Value) -> Value {
            let public_key = &options["publicKey"];
            assert_eq!(
                public_key["authenticatorSelection"]["residentKey"],
                "required"
            );
            let challenge = public_key["challenge"].as_str().unwrap();
            self.user_handle = Some(
                URL_SAFE_NO_PAD
                    .decode(public_key["user"]["id"].as_str().unwrap())
                    .unwrap(),
            );
            let client_data = self.client_data("webauthn.create", challenge);
            let auth_data = self.auth_data(true);
            let mut attestation = Vec::new();
            cbor_head(5, 3, &mut attestation);
            cbor_text("fmt", &mut attestation);
            cbor_text("none", &mut attestation);
            cbor_text("attStmt", &mut attestation);
            cbor_head(5, 0, &mut attestation);
            cbor_text("authData", &mut attestation);
            cbor_bytes(&auth_data, &mut attestation);
            let id = URL_SAFE_NO_PAD.encode(&self.credential_id);
            json!({
                "id": id, "rawId": id, "type": "public-key",
                "response": {
                    "attestationObject": URL_SAFE_NO_PAD.encode(attestation),
                    "clientDataJSON": URL_SAFE_NO_PAD.encode(client_data),
                    "transports": ["internal"],
                },
                "extensions": {},
            })
        }

        /// `navigator.credentials.get`.
        pub(super) fn get(&mut self, options: &Value) -> Value {
            let challenge = options["publicKey"]["challenge"].as_str().unwrap();
            let client_data = self.client_data("webauthn.get", challenge);
            let auth_data = self.auth_data(false);
            let mut signed = auth_data.clone();
            signed.extend(Sha256::digest(&client_data));
            let signature: p256::ecdsa::Signature = self.key.sign(&signed);
            let id = URL_SAFE_NO_PAD.encode(&self.credential_id);
            json!({
                "id": id, "rawId": id, "type": "public-key",
                "response": {
                    "authenticatorData": URL_SAFE_NO_PAD.encode(auth_data),
                    "clientDataJSON": URL_SAFE_NO_PAD.encode(client_data),
                    "signature": URL_SAFE_NO_PAD.encode(signature.to_der().as_bytes()),
                    "userHandle": URL_SAFE_NO_PAD.encode(self.user_handle.as_ref().unwrap()),
                },
                "extensions": {},
            })
        }
    }
}

#[tokio::test]
async fn passkeys_sign_up_sign_in_and_can_be_added() {
    let Some(server) = common::serve(&[
        ("DRUSITE__AUTH__PASSKEYS__ENABLED", "true"),
        ("DRUSITE__AUTH__SELF_SIGNUP", "true"),
    ])
    .await
    else {
        return;
    };
    let mut phone = passkey::Authenticator::new(&server.url);

    // Sign up with a passkey.
    let mut browser = Browser::new(&server.url);
    let (status, start) = browser
        .post(
            "/api/v1/auth/passkeys/signup/start",
            json!({ "email": "kim@example.com", "name": "Kim" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let credential = phone.create(&start["options"]);
    let (status, step) = browser
        .post(
            "/api/v1/auth/passkeys/signup/finish",
            json!({ "challenge": start["challenge"], "credential": credential, "name": "Phone" }),
        )
        .await;
    assert_eq!(
        (status, step["status"].clone()),
        (StatusCode::OK, json!("signed_in")),
        "{step}"
    );

    // Sign in with it, in a new browser, through the app's flow.
    let app = App::new(&server.url);
    let mut laptop = Browser::new(&server.url);
    let auth = app.start();
    let request = open_authorize(&mut laptop, &auth).await;
    let (_, start) = laptop
        .post("/api/v1/auth/passkeys/login/start", json!({}))
        .await;
    assert!(
        start["options"].get("mediation").is_none(),
        "a button, not autofill"
    );
    let assertion = phone.get(&start["options"]);
    let (status, step) = laptop
        .post(
            "/api/v1/auth/passkeys/login/finish",
            json!({ "challenge": start["challenge"], "credential": assertion, "request": request }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{step}");
    let code = finish(&mut laptop, &auth, &step).await;
    let tokens = app.exchange(&auth, &code).await;
    assert_eq!(app.me(&tokens.access).await.1["email"], "kim@example.com");

    // A challenge works once.
    let assertion = phone.get(&start["options"]);
    let (status, _) = laptop
        .post(
            "/api/v1/auth/passkeys/login/finish",
            json!({ "challenge": start["challenge"], "credential": assertion }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Add a second passkey from the account page; then both are listed, and one can go.
    let mut key2 = passkey::Authenticator::new(&server.url);
    let (status, start) = laptop
        .post("/api/v1/account/passkeys/register/start", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let excluded = start["options"]["publicKey"]["excludeCredentials"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(excluded, 1, "the existing passkey isn't registered twice");
    let credential = key2.create(&start["options"]);
    let (status, body) = laptop
        .post(
            "/api/v1/account/passkeys/register/finish",
            json!({ "challenge": start["challenge"], "credential": credential, "name": "Laptop" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list: Value = laptop
        .get(&laptop.url("/api/v1/account/passkeys"))
        .await
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Phone", "Laptop"]);
    let id = URL_SAFE_NO_PAD.encode(&key2.credential_id);
    assert_eq!(
        laptop
            .delete(&format!("/api/v1/account/passkeys/{id}"))
            .await,
        StatusCode::OK
    );

    // A removed passkey signs nobody in.
    let (_, start) = laptop
        .post("/api/v1/auth/passkeys/login/start", json!({}))
        .await;
    let assertion = key2.get(&start["options"]);
    let (status, _) = laptop
        .post(
            "/api/v1/auth/passkeys/login/finish",
            json!({ "challenge": start["challenge"], "credential": assertion }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

mod provider {
    //! A small OIDC provider: discovery, keys, and an authorize endpoint that says yes at once,
    //! issuing ES256 ID tokens with whatever claims the test sets.

    use super::*;
    use axum::extract::{Form, Query, State};
    use axum::response::Redirect;
    use axum::routing::{get, post};
    use sha2::{Digest as _, Sha256};

    #[derive(Clone)]
    pub(super) struct Provider {
        pub(super) url: Url,
        /// What discovery and ID tokens say the issuer is.
        pub(super) issuer: String,
        pub(super) claims: Arc<Mutex<Value>>,
        key: Arc<SoftwareKey>,
        codes: Arc<Mutex<HashMap<String, (String, String)>>>,
    }

    /// A provider whose issuer has no trailing slash, like Keycloak's.
    pub(super) async fn start() -> Provider {
        start_with(false).await
    }

    /// A provider whose issuer ends in a slash, like Authentik's and Auth0's.
    pub(super) async fn start_with_slash() -> Provider {
        start_with(true).await
    }

    async fn start_with(slash: bool) -> Provider {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let base = url.as_str().trim_end_matches('/').to_owned();
        let provider = Provider {
            url,
            issuer: if slash { format!("{base}/") } else { base },
            claims: Arc::new(Mutex::new(json!({}))),
            key: Arc::new(SoftwareKey::generate().unwrap()),
            codes: Arc::default(),
        };
        let app = axum::Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/authorize", get(authorize))
            .route("/token", post(token))
            .with_state(provider.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        provider
    }

    fn issuer(p: &Provider) -> String {
        p.issuer.clone()
    }

    async fn discovery(State(p): State<Provider>) -> axum::Json<Value> {
        let i = p.url.as_str().trim_end_matches('/');
        axum::Json(json!({
            "issuer": issuer(&p),
            "authorization_endpoint": format!("{i}/authorize"),
            "token_endpoint": format!("{i}/token"),
            "jwks_uri": format!("{i}/jwks"),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["ES256"],
        }))
    }

    async fn jwks(State(p): State<Provider>) -> axum::Json<Value> {
        let jwk = p.key.jwk();
        axum::Json(json!({ "keys": [{
            "kty": "EC", "crv": "P-256", "x": jwk.x, "y": jwk.y,
            "kid": "k1", "use": "sig", "alg": "ES256",
        }]}))
    }

    async fn authorize(
        State(p): State<Provider>,
        Query(q): Query<HashMap<String, String>>,
    ) -> Redirect {
        assert_eq!(q["code_challenge_method"], "S256");
        let code = format!("code-{}", fastrand::u64(..));
        p.codes.lock().unwrap().insert(
            code.clone(),
            (q["nonce"].clone(), q["code_challenge"].clone()),
        );
        let mut back = Url::parse(&q["redirect_uri"]).unwrap();
        back.query_pairs_mut()
            .append_pair("code", &code)
            .append_pair("state", &q["state"]);
        Redirect::to(back.as_str())
    }

    async fn token(
        State(p): State<Provider>,
        Form(f): Form<HashMap<String, String>>,
    ) -> axum::Json<Value> {
        let (nonce, challenge) = p.codes.lock().unwrap().remove(&f["code"]).unwrap();
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(f["code_verifier"].as_bytes())),
            challenge
        );
        let now = super::now();
        let mut claims = json!({
            "iss": issuer(&p), "aud": "drusite", "iat": now, "exp": now + 300, "nonce": nonce,
        });
        for (k, v) in p.claims.lock().unwrap().as_object().unwrap() {
            claims[k] = v.clone();
        }
        let header = URL_SAFE_NO_PAD
            .encode(json!({ "alg": "ES256", "kid": "k1", "typ": "JWT" }).to_string());
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signature = p.key.sign(format!("{header}.{payload}").as_bytes());
        axum::Json(json!({
            "access_token": "at", "token_type": "Bearer", "expires_in": 300,
            "id_token": format!("{header}.{payload}.{}", URL_SAFE_NO_PAD.encode(signature)),
        }))
    }
}

/// Runs the browser through the provider and back; returns the final response from Drusite.
async fn oidc_sign_in(browser: &mut Browser, request: Option<&str>) -> reqwest::Response {
    let mut start = browser.url("/api/v1/auth/oidc/idp/start");
    if let Some(r) = request {
        start.query_pairs_mut().append_pair("request", r);
    }
    let response = browser.get(&start).await;
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "{:?}",
        response.text().await
    );
    let at_provider = location(&response, &browser.base);
    let response = browser.get(&at_provider).await;
    let callback = location(&response, &browser.base);
    browser.get(&callback).await
}

/// Issuers are compared exactly. Authentik's ends in a slash; Drusite used to drop it, and
/// then refused the provider's own (correct) issuer during discovery.
#[tokio::test]
async fn an_issuer_with_a_trailing_slash_is_kept_as_written() {
    let idp = provider::start_with_slash().await;
    assert!(idp.issuer.ends_with('/'));
    let Some(server) = common::serve(&[
        ("DRUSITE__AUTH__OIDC__IDP__DISPLAY_NAME", "Authentik"),
        ("DRUSITE__AUTH__OIDC__IDP__ISSUER", &idp.issuer),
        ("DRUSITE__AUTH__OIDC__IDP__CLIENT_ID", "drusite"),
        ("DRUSITE__AUTH__OIDC__IDP__CLIENT_SECRET", "s3cret"),
        ("DRUSITE__AUTH__OIDC__IDP__AUTO_CREATE", "true"),
    ])
    .await
    else {
        return;
    };
    *idp.claims.lock().unwrap() =
        json!({ "sub": "u1", "email": "sam@example.com", "email_verified": true });
    let response = oidc_sign_in(&mut Browser::new(&server.url), None).await;
    assert!(
        response.status().is_redirection() || response.status().is_success(),
        "{}",
        response.status()
    );
}

#[tokio::test]
async fn oidc_signs_in_and_is_careful_with_emails() {
    let idp = provider::start().await;
    let issuer = idp.issuer.clone();
    let Some(server) = common::serve(&[
        ("DRUSITE__AUTH__PASSWORD__ENABLED", "true"),
        ("DRUSITE__AUTH__SELF_SIGNUP", "true"),
        ("DRUSITE__AUTH__OIDC__IDP__DISPLAY_NAME", "Test IdP"),
        ("DRUSITE__AUTH__OIDC__IDP__ISSUER", &issuer),
        ("DRUSITE__AUTH__OIDC__IDP__CLIENT_ID", "drusite"),
        ("DRUSITE__AUTH__OIDC__IDP__CLIENT_SECRET", "s3cret"),
        ("DRUSITE__AUTH__OIDC__IDP__AUTO_CREATE", "true"),
        ("DRUSITE__AUTH__OIDC__IDP__LINK_BY_VERIFIED_EMAIL", "true"),
        ("DRUSITE__AUTH__OIDC__IDP__ALLOWED_GROUPS", r#"["notes"]"#),
    ])
    .await
    else {
        return;
    };
    let set = |claims: Value| *idp.claims.lock().unwrap() = claims;

    // Not in an allowed group: refused, whatever else is true.
    set(
        json!({ "sub": "u1", "email": "sam@example.com", "email_verified": true, "groups": ["other"] }),
    );
    let response = oidc_sign_in(&mut Browser::new(&server.url), None).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // In the group: an account is made, and the app gets its tokens.
    set(
        json!({ "sub": "u1", "email": "sam@example.com", "email_verified": true, "groups": ["notes"], "name": "Sam" }),
    );
    let app = App::new(&server.url);
    let mut browser = Browser::new(&server.url);
    let auth = app.start();
    let request = open_authorize(&mut browser, &auth).await;
    let response = oidc_sign_in(&mut browser, Some(&request)).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let next = location(&response, &server.url);
    let response = browser.get(&next).await;
    let code = auth.code_from(&location(&response, &server.url)).unwrap();
    let tokens = app.exchange(&auth, &code).await;
    let (_, me) = app.me(&tokens.access).await;
    assert_eq!(
        (
            me["email"].clone(),
            me["emailVerified"].clone(),
            me["displayName"].clone()
        ),
        (json!("sam@example.com"), json!(true), json!("Sam"))
    );

    // The same subject later with another email is still the same account.
    set(
        json!({ "sub": "u1", "email": "sam@new.example", "email_verified": true, "groups": ["notes"] }),
    );
    let mut again = Browser::new(&server.url);
    assert_eq!(
        oidc_sign_in(&mut again, None).await.status(),
        StatusCode::SEE_OTHER
    );
    let me: Value = again
        .get(&again.url("/api/v1/account"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(me["email"], "sam@example.com");

    // Someone signed up here with a password and an email they never proved. A provider
    // account with that email, even a verified one, doesn't get into it.
    signup(&mut Browser::new(&server.url), "pat@example.com", None).await;
    set(
        json!({ "sub": "u2", "email": "pat@example.com", "email_verified": true, "groups": ["notes"] }),
    );
    assert_eq!(
        oidc_sign_in(&mut Browser::new(&server.url), None)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    // An unverified provider email never joins an existing account either.
    sqlx::query("UPDATE users SET email_verified = true WHERE email = 'pat@example.com'")
        .execute(server.db.db.pool())
        .await
        .unwrap();
    set(
        json!({ "sub": "u3", "email": "pat@example.com", "email_verified": false, "groups": ["notes"] }),
    );
    assert_eq!(
        oidc_sign_in(&mut Browser::new(&server.url), None)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    // Both verified, and the provider trusted to: joined.
    set(
        json!({ "sub": "u4", "email": "pat@example.com", "email_verified": true, "groups": ["notes"] }),
    );
    let mut pat = Browser::new(&server.url);
    assert_eq!(
        oidc_sign_in(&mut pat, None).await.status(),
        StatusCode::SEE_OTHER
    );
    let me: Value = pat
        .get(&pat.url("/api/v1/account"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(me["email"], "pat@example.com");

    // A callback can't be finished in another browser (login CSRF).
    let mut victim = Browser::new(&server.url);
    let mut attacker = Browser::new(&server.url);
    let response = attacker
        .get(&attacker.url("/api/v1/auth/oidc/idp/start"))
        .await;
    let at_provider = location(&response, &server.url);
    let response = attacker.get(&at_provider).await;
    let callback = location(&response, &server.url);
    let response = victim.get(&callback).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(victim.cookies.keys().all(|k| !k.contains("session")));
}

/// Signs an app in with a password account; returns the app and its tokens.
async fn signed_in_app(server: &common::Server, email: &str) -> (App, Tokens) {
    let app = App::new(&server.url);
    let mut browser = Browser::new(&server.url);
    let auth = app.start();
    let request = open_authorize(&mut browser, &auth).await;
    let step = signup(&mut browser, email, Some(&request)).await;
    let code = finish(&mut browser, &auth, &step).await;
    let tokens = app.exchange(&auth, &code).await;
    (app, tokens)
}

#[tokio::test]
async fn racing_refreshes_leave_one_live_chain_and_no_errors() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let (app, tokens) = signed_in_app(&server, "race@example.com").await;
    let app = Arc::new(app);
    // A device that fires the same refresh eight times at once (a retry storm).
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let app = Arc::clone(&app);
        let refresh = tokens.refresh.clone();
        tasks.push(tokio::spawn(async move { app.refresh(&refresh).await }));
    }
    let mut issued = Vec::new();
    for task in tasks {
        let (status, body) = task.await.unwrap();
        assert!(
            status == StatusCode::OK || status == StatusCode::BAD_REQUEST,
            "{status} {body}"
        );
        if status == StatusCode::OK {
            issued.push(body["refresh_token"].as_str().unwrap().to_owned());
        }
    }
    assert!(!issued.is_empty());
    // The grant survives, and exactly one refresh token is still unused: one chain.
    let (revoked, unused): (bool, i64) = sqlx::query_as(
        "SELECT g.revoked_at IS NOT NULL,
                (SELECT count(*) FROM refresh_tokens r WHERE r.grant_id = g.id AND r.used_at IS NULL)
         FROM oauth_grants g",
    )
    .fetch_one(server.db.db.pool())
    .await
    .unwrap();
    assert!(!revoked);
    assert_eq!(unused, 1);
    let mut live = 0;
    for refresh in issued {
        if app.refresh(&refresh).await.0 == StatusCode::OK {
            live += 1;
            break;
        }
    }
    assert_eq!(live, 1, "the chain goes on from one of the racing answers");
}

#[tokio::test]
async fn the_sweeper_removes_only_what_has_expired() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let (app, tokens) = signed_in_app(&server, "sweep@example.com").await;
    let pool = server.db.db.pool();
    let count = |table: &'static str| async move {
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .unwrap()
    };
    // Something expired in each table, next to the live rows the sign-in made.
    for sql in [
        "INSERT INTO dpop_proofs VALUES ('jkt', 'old', now() - interval '1 minute')",
        "INSERT INTO auth_challenges (id, kind, data, expires_at)
             VALUES (gen_random_uuid(), 'x', '{}', now() - interval '1 minute')",
        "INSERT INTO auth_attempts (key, at) VALUES ('old', now() - interval '2 days')",
        "INSERT INTO access_tokens (token_hash, grant_id, expires_at)
             SELECT '\\x00', id, now() - interval '1 minute' FROM oauth_grants",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(pool)
            .await
            .unwrap();
    }
    let before = (
        count("dpop_proofs").await,
        count("auth_challenges").await,
        count("access_tokens").await,
    );
    drusite_server::auth::tokens::sweep(&server.state)
        .await
        .unwrap();
    let after = (
        count("dpop_proofs").await,
        count("auth_challenges").await,
        count("access_tokens").await,
    );
    assert_eq!(after, (before.0 - 1, before.1 - 1, before.2 - 1));
    assert_eq!(count("auth_attempts").await, 0);
    // The live sign-in is untouched.
    assert_eq!(app.me(&tokens.access).await.0, StatusCode::OK);
    assert_eq!(app.refresh(&tokens.refresh).await.0, StatusCode::OK);
}
