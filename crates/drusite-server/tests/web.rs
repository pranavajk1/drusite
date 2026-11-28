// SPDX-License-Identifier: AGPL-3.0-or-later

//! The web app: served at `/`, signing in through the server (a backend-for-frontend) with an
//! HttpOnly, SameSite=Strict cookie that the API takes only from this server's own pages.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use common::browser::{Browser, location};
use reqwest::StatusCode;
use serde_json::{Value, json};
use url::Url;

/// A built web app, as `ui/dist-web` holds one.
fn web_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><title>Drusite</title>",
    )
    .unwrap();
    std::fs::write(dir.path().join("sw.js"), "// sw").unwrap();
    std::fs::create_dir(dir.path().join("app")).unwrap();
    std::fs::write(dir.path().join("app/index-abc.js"), "console.log(1)").unwrap();
    dir
}

fn web_server(dir: &tempfile::TempDir) -> Vec<(&'static str, String)> {
    vec![
        ("DRUSITE__AUTH__PASSWORD__ENABLED", "true".to_owned()),
        ("DRUSITE__AUTH__SELF_SIGNUP", "true".to_owned()),
        ("DRUSITE__WEB__DIR", dir.path().to_str().unwrap().to_owned()),
    ]
}

/// A request from the web app's own pages: its cookies, and its origin.
async fn from_app(
    browser: &Browser,
    method: reqwest::Method,
    path: &str,
    origin: Option<&str>,
) -> reqwest::Response {
    let mut request = browser
        .http
        .request(method, browser.url(path))
        .header("cookie", browser.cookie_header())
        .header("x-drusite", "web");
    let ours = browser.base.origin().ascii_serialization();
    request = request.header("origin", origin.unwrap_or(&ours));
    request.send().await.unwrap()
}

/// Signs the browser in to the web app with a new password account; returns the callback URL
/// the sign-in came back to (already followed).
async fn sign_in(browser: &mut Browser, email: &str) -> Url {
    let base = browser.base.clone();
    let start = browser.get(&browser.url("/web/login")).await;
    assert_eq!(start.status(), StatusCode::SEE_OTHER);
    let authorize = location(&start, &base);
    assert_eq!(authorize.path(), "/oauth/authorize");
    let q = |k: &str| {
        authorize
            .query_pairs()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.into_owned())
    };
    assert_eq!(q("client_id").as_deref(), Some("drusite-web"));
    assert_eq!(q("redirect_uri"), Some(format!("{}web/callback", base)));

    let login = browser.get(&authorize).await;
    let login = location(&login, &base);
    assert_eq!(login.path(), "/login");
    let request = login
        .query_pairs()
        .find(|(k, _)| k == "request")
        .unwrap()
        .1
        .into_owned();
    let (status, step) = browser
        .post(
            "/api/v1/auth/password/signup",
            json!({ "email": email, "password": "correct horse battery", "request": request }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{step}");
    let next = base.join(step["next"].as_str().unwrap()).unwrap();
    let back = browser.get(&next).await;
    let callback = location(&back, &base);
    assert_eq!(callback.path(), "/web/callback");
    callback
}

async fn finish(browser: &mut Browser, callback: &Url) -> reqwest::Response {
    browser.get(callback).await
}

#[tokio::test]
async fn the_web_app_is_served_and_signs_in_with_a_cookie() {
    let dir = web_dir();
    let overrides = web_server(&dir);
    let o: Vec<(&str, &str)> = overrides.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let Some(server) = common::serve(&o).await else {
        return;
    };
    let mut browser = Browser::new(&server.url);

    // The page, strict about what it runs; its files, kept for good.
    let page = browser.get(&server.url).await;
    assert_eq!(page.status(), StatusCode::OK);
    let csp = page.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        csp.contains("script-src 'self' 'wasm-unsafe-eval'"),
        "{csp}"
    );
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert_eq!(page.headers()["cache-control"], "no-cache");
    let file = browser
        .get(&server.url.join("app/index-abc.js").unwrap())
        .await;
    assert_eq!(file.status(), StatusCode::OK);
    assert!(
        file.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("immutable")
    );
    assert_eq!(
        browser
            .get(&server.url.join("app/nothing.js").unwrap())
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    // Not signed in yet.
    let none = from_app(&browser, reqwest::Method::GET, "/api/v1/web/session", None).await;
    assert_eq!(none.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        none.json::<Value>().await.unwrap()["error"],
        "not_signed_in"
    );

    let callback = sign_in(&mut browser, "ada@example.com").await;

    // The code is for this server alone: the token endpoint won't redeem it.
    let code = callback
        .query_pairs()
        .find(|(k, _)| k == "code")
        .unwrap()
        .1
        .into_owned();
    let refused: Value = browser
        .http
        .post(server.url.join("oauth/token").unwrap())
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", &format!("{}web/callback", server.url)),
            ("code_verifier", &"a".repeat(43)),
            ("client_id", "drusite-web"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(refused["error"], "invalid_client", "{refused}");

    // Another browser (without the nonce cookie) can't finish this sign-in.
    let mut other = Browser::new(&server.url);
    assert_eq!(
        finish(&mut other, &callback).await.status(),
        StatusCode::BAD_REQUEST
    );

    let done = finish(&mut browser, &callback).await;
    assert_eq!(done.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&done, &server.url).path(), "/");
    let set: Vec<String> = done
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .collect();
    let web = set.iter().find(|c| c.starts_with("drusite_web=")).unwrap();
    assert!(
        web.contains("HttpOnly") && web.contains("SameSite=Strict"),
        "{web}"
    );
    assert!(web.contains("drw_"), "{web}");

    // Signed in: who, and the API with the cookie, from this server's own pages.
    let me = from_app(&browser, reqwest::Method::GET, "/api/v1/web/session", None).await;
    assert_eq!(me.status(), StatusCode::OK);
    let me: Value = me.json().await.unwrap();
    assert_eq!(me["email"], "ada@example.com");
    assert_eq!(me["server"], server.url.as_str());
    let list = from_app(&browser, reqwest::Method::GET, "/api/v1/workspaces", None).await;
    assert_eq!(list.status(), StatusCode::OK);

    // A sibling subdomain is the "same site", so the cookie would ride along: refused by origin.
    let port = server.url.port().unwrap();
    let sibling = format!("http://evil.localhost:{port}");
    let from_sibling = from_app(
        &browser,
        reqwest::Method::GET,
        "/api/v1/workspaces",
        Some(&sibling),
    )
    .await;
    assert_eq!(from_sibling.status(), StatusCode::UNAUTHORIZED);
    let fetched = browser
        .http
        .get(browser.url("/api/v1/workspaces"))
        .header("cookie", browser.cookie_header())
        .header("sec-fetch-site", "same-site")
        .send()
        .await
        .unwrap();
    assert_eq!(fetched.status(), StatusCode::UNAUTHORIZED);

    // It's a device on the account page, and signing it out there ends the cookie's session:
    // the app is told it was signed out, so its copy of the notes goes.
    let devices: Value = browser
        .get(&browser.url("/api/v1/account/devices"))
        .await
        .json()
        .await
        .unwrap();
    let device = devices
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "A browser")
        .unwrap_or_else(|| panic!("{devices}"))
        .clone();
    assert_eq!(
        browser
            .delete(&format!(
                "/api/v1/account/devices/{}",
                device["id"].as_str().unwrap()
            ))
            .await,
        StatusCode::OK
    );
    let gone = from_app(&browser, reqwest::Method::GET, "/api/v1/web/session", None).await;
    assert_eq!(gone.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(gone.json::<Value>().await.unwrap()["error"], "signed_out");
    let refused = from_app(&browser, reqwest::Method::GET, "/api/v1/workspaces", None).await;
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);

    // Signing in again, then out from the app: the cookie, the grant and the sign-in pages'
    // session all go.
    browser.cookies.clear();
    let callback = sign_in(&mut browser, "grace@example.com").await;
    finish(&mut browser, &callback).await;
    assert_eq!(
        from_app(&browser, reqwest::Method::GET, "/api/v1/web/session", None)
            .await
            .status(),
        StatusCode::OK
    );
    let foreign = from_app(
        &browser,
        reqwest::Method::POST,
        "/api/v1/web/logout",
        Some(&sibling),
    )
    .await;
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
    let out = from_app(&browser, reqwest::Method::POST, "/api/v1/web/logout", None).await;
    assert_eq!(out.status(), StatusCode::OK);
    browser.absorb(out.headers());
    assert!(!browser.cookies.contains_key("drusite_web"));
    assert!(!browser.cookies.contains_key("drusite_session"));
    let grants: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oauth_grants WHERE client_id = 'drusite-web' AND revoked_at IS NULL",
    )
    .fetch_one(server.state.db().pool())
    .await
    .unwrap();
    assert_eq!(grants, 0);
    // No token was ever kept for a browser's grant.
    let tokens: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM access_tokens a JOIN oauth_grants g ON g.id = a.grant_id
                 WHERE g.client_id = 'drusite-web')
              + (SELECT count(*) FROM refresh_tokens r JOIN oauth_grants g ON g.id = r.grant_id
                 WHERE g.client_id = 'drusite-web')",
    )
    .fetch_one(server.state.db().pool())
    .await
    .unwrap();
    assert_eq!(tokens, 0);
}

#[tokio::test]
async fn without_a_web_app_there_is_nothing_to_sign_in_to() {
    let Some(server) = common::serve(&[
        ("DRUSITE__AUTH__PASSWORD__ENABLED", "true"),
        ("DRUSITE__AUTH__SELF_SIGNUP", "true"),
    ])
    .await
    else {
        return;
    };
    let mut browser = Browser::new(&server.url);
    assert_eq!(
        browser.get(&server.url).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        browser.get(&browser.url("/web/login")).await.status(),
        StatusCode::NOT_FOUND
    );
    // The built-in web client can't be used to sign in either.
    let mut authorize = server.url.join("oauth/authorize").unwrap();
    authorize
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", "drusite-web")
        .append_pair("redirect_uri", &format!("{}web/callback", server.url))
        .append_pair("code_challenge", &"a".repeat(43))
        .append_pair("code_challenge_method", "S256");
    assert_eq!(
        browser.get(&authorize).await.status(),
        StatusCode::BAD_REQUEST
    );
}
