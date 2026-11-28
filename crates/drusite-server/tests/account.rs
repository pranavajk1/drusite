// SPDX-License-Identifier: AGPL-3.0-or-later

//! The apps' account supervisor (`drusite_core::account`) against a real server: signing in
//! through the browser flow, syncing, refreshing after a restart, and every way of being signed
//! out.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::browser::{Browser, location};
use drusite_core::account::{SharedKey, Supervisor};
use drusite_core::api::{AccountStatus, Command, Response, SyncPhase};
use drusite_core::auth::APP_REDIRECT;
use drusite_core::{Core, StoreKey};
use drusite_dpop::SoftwareKey;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::sync::watch;
use url::Url;

const WAIT: Duration = Duration::from_secs(20);

fn password_server() -> Vec<(&'static str, &'static str)> {
    vec![
        ("DRUSITE__AUTH__PASSWORD__ENABLED", "true"),
        ("DRUSITE__AUTH__SELF_SIGNUP", "true"),
    ]
}

/// One device running the app's supervisor.
struct Device {
    core: Arc<Mutex<Core>>,
    key: SoftwareKey,
    store_key: StoreKey,
    supervisor: Supervisor,
    status: watch::Receiver<AccountStatus>,
    dir: tempfile::TempDir,
}

fn start(
    core: &Arc<Mutex<Core>>,
    key: &SoftwareKey,
) -> (Supervisor, watch::Receiver<AccountStatus>) {
    let (tx, rx) = watch::channel(AccountStatus::signed_out(None));
    let shared: SharedKey = Arc::new(key.clone());
    let supervisor = Supervisor::start(
        &tokio::runtime::Handle::current(),
        Arc::clone(core),
        shared,
        "Test phone",
        move |s| {
            let _ = tx.send(s.clone());
        },
    );
    (supervisor, rx)
}

impl Device {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store_key = StoreKey::generate().unwrap();
        let core = Arc::new(Mutex::new(
            Core::open(dir.path(), &store_key, Box::new(|_| {})).unwrap(),
        ));
        let key = SoftwareKey::generate().unwrap();
        let (supervisor, status) = start(&core, &key);
        Self {
            core,
            key,
            store_key,
            supervisor,
            status,
            dir,
        }
    }

    /// Stops the supervisor and starts the app again on the same store and key.
    fn restart(&mut self) {
        self.supervisor.stop();
        let core = Arc::new(Mutex::new(
            Core::open(self.dir.path(), &self.store_key, Box::new(|_| {})).unwrap(),
        ));
        let (supervisor, status) = start(&core, &self.key);
        self.core = core;
        self.supervisor = supervisor;
        self.status = status;
    }

    async fn wait_for(&mut self, phase: SyncPhase) -> AccountStatus {
        let mut rx = self.status.clone();
        let waited = tokio::time::timeout(WAIT, rx.wait_for(|s| s.phase == phase)).await;
        match waited {
            Ok(status) => status.unwrap().clone(),
            Err(_) => panic!("waited for {phase:?}, still {:?}", *self.status.borrow()),
        }
    }

    /// Signs in through the browser: signing up as `email` when `new`, else logging in.
    async fn sign_in(&self, server: &Url, email: &str, new: bool) {
        let url = self
            .supervisor
            .begin_sign_in(server.as_str(), APP_REDIRECT)
            .await
            .unwrap();
        let mut browser = Browser::new(server);
        let response = browser.get(&url).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let login = location(&response, server);
        let request = login
            .query_pairs()
            .find(|(k, _)| k == "request")
            .unwrap()
            .1
            .into_owned();
        let path = if new {
            "/api/v1/auth/password/signup"
        } else {
            "/api/v1/auth/password/login"
        };
        let (status, step) = browser
            .post(
                path,
                json!({ "email": email, "password": "correct horse battery", "name": "Ada",
                        "request": request }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{step}");
        assert_eq!(step["status"], "signed_in", "{step}");
        let next = server.join(step["next"].as_str().unwrap()).unwrap();
        let response = browser.get(&next).await;
        let callback = location(&response, server);
        assert!(callback.as_str().starts_with(APP_REDIRECT), "{callback}");
        self.supervisor.finish_sign_in(&callback).await.unwrap();
    }

    fn handle(&self, command: Command) -> Response {
        self.core.lock().unwrap().handle(command).unwrap()
    }

    fn create_page(&self, title: &str) {
        self.handle(Command::CreatePage {
            parent: None,
            title: title.to_owned(),
        });
    }

    fn titles(&self) -> Vec<String> {
        match self.handle(Command::ListPages) {
            Response::Pages { pages } => {
                let mut t: Vec<String> = pages.into_iter().map(|p| p.title).collect();
                t.sort();
                t
            }
            other => panic!("{other:?}"),
        }
    }

    async fn wait_for_titles(&self, want: &[&str]) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let titles = self.titles();
            if titles == want {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "pages are {titles:?}, want {want:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn refresh_token(&self) -> Option<String> {
        // Not in the public API: read it back through a second store connection.
        let store = drusite_core::store::Store::open(
            &self.dir.path().join(drusite_core::store::STORE_FILE),
            &self.store_key,
        )
        .unwrap();
        store.account().unwrap().and_then(|a| a.refresh_token)
    }
}

async fn refresh_with(server: &Url, key: &SoftwareKey, token: &str) -> StatusCode {
    let endpoint = server.join("oauth/token").unwrap();
    let proof = drusite_core::auth::proof(key, "POST", &endpoint, None, now()).unwrap();
    reqwest::Client::new()
        .post(endpoint)
        .header("DPoP", proof)
        .form(&drusite_core::auth::refresh_form(token))
        .send()
        .await
        .unwrap()
        .status()
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_sign_in_and_stay_in_sync() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    // The Mac already has notes; the account is new, so they become the account's.
    let mut mac = Device::new();
    mac.create_page("From the Mac");
    mac.sign_in(&server.url, "ada@example.com", true).await;
    let status = mac.wait_for(SyncPhase::Online).await;
    assert_eq!(status.email.as_deref(), Some("ada@example.com"));
    assert_eq!(
        status.server.as_deref(),
        Some(server.url.as_str()),
        "the server's own address, from its metadata"
    );
    assert_eq!(
        status.server_version.as_deref(),
        Some(env!("CARGO_PKG_VERSION")),
        "the version the server gave when sync connected"
    );

    // A new phone takes the account's notes instead of starting its own, giving up the empty
    // page a new install opens on.
    let mut phone = Device::new();
    phone.create_page("");
    phone.sign_in(&server.url, "ada@example.com", false).await;
    phone.wait_for(SyncPhase::Online).await;
    phone.wait_for_titles(&["From the Mac"]).await;
    assert_eq!(
        phone.core.lock().unwrap().workspace_id(),
        mac.core.lock().unwrap().workspace_id()
    );

    phone.create_page("From the phone");
    mac.wait_for_titles(&["From the Mac", "From the phone"])
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_refreshes_and_signing_out_revokes() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let mut phone = Device::new();
    phone.sign_in(&server.url, "ada@example.com", true).await;
    phone.wait_for(SyncPhase::Online).await;
    let first = phone.refresh_token().unwrap();

    // The app starts again: it refreshes with the saved token, which rotates.
    phone.restart();
    phone.wait_for(SyncPhase::Online).await;
    let second = phone.refresh_token().unwrap();
    assert_ne!(first, second, "every refresh rotates the token");

    phone.supervisor.sign_out().await.unwrap();
    let status = phone.wait_for(SyncPhase::SignedOut).await;
    assert_eq!(
        status.message, None,
        "signing out on purpose isn't an error"
    );
    assert_eq!(phone.refresh_token(), None);
    assert_eq!(
        refresh_with(&server.url, &phone.key, &second).await,
        StatusCode::BAD_REQUEST,
        "the server revoked it"
    );
    // The notes stay on the device.
    phone.create_page("Still here");
    assert_eq!(phone.titles(), ["Still here"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_signed_out_from_the_server_says_so() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let mut phone = Device::new();
    phone.sign_in(&server.url, "ada@example.com", true).await;
    phone.wait_for(SyncPhase::Online).await;

    // Signed out from the account page in a browser.
    let mut browser = Browser::new(&server.url);
    let (status, _) = browser
        .post(
            "/api/v1/auth/password/login",
            json!({ "email": "ada@example.com", "password": "correct horse battery" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let devices: Value = browser
        .get(&browser.url("/api/v1/account/devices"))
        .await
        .json()
        .await
        .unwrap();
    let id = devices[0]["id"].as_str().unwrap();
    assert_eq!(
        browser
            .delete(&format!("/api/v1/account/devices/{id}"))
            .await,
        StatusCode::OK
    );

    // The connection learns within a minute; coming back to the foreground, at once.
    phone.supervisor.kick();
    let status = phone.wait_for(SyncPhase::SignedOut).await;
    assert!(
        status
            .message
            .as_deref()
            .is_some_and(|m| m.contains("signed this device out")),
        "{status:?}"
    );
    assert_eq!(phone.refresh_token(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_with_its_own_notes_doesnt_take_another_workspace() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let mut mac = Device::new();
    mac.create_page("From the Mac");
    mac.sign_in(&server.url, "ada@example.com", true).await;
    mac.wait_for(SyncPhase::Online).await;

    let mut other = Device::new();
    other.create_page("Somewhere else");
    other.sign_in(&server.url, "ada@example.com", false).await;
    let status = other.wait_for(SyncPhase::Failed).await;
    assert!(
        status
            .message
            .as_deref()
            .is_some_and(|m| m.contains("can't merge")),
        "{status:?}"
    );
    assert_eq!(other.titles(), ["Somewhere else"]);
    assert_eq!(mac.titles(), ["From the Mac"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn signing_in_checks_the_server_and_the_answer() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let mut phone = Device::new();
    // Not a Drusite server (nothing answers there).
    let elsewhere = server.url.join("not-here/").unwrap();
    let err = phone
        .supervisor
        .begin_sign_in(elsewhere.as_str(), APP_REDIRECT)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("doesn't look like a Drusite server"),
        "{err}"
    );

    // Reached under another name: sign-in moves to the server's own address (its issuer)…
    let by_ip = server.url.as_str().replace("localhost", "127.0.0.1");
    let url = phone
        .supervisor
        .begin_sign_in(&by_ip, APP_REDIRECT)
        .await
        .unwrap();
    assert_eq!(url.host_str(), Some("localhost"), "{url}");
    phone.wait_for(SyncPhase::SigningIn).await;
    phone.supervisor.cancel_sign_in();
    phone.wait_for(SyncPhase::SignedOut).await;

    // An answer for another sign-in is refused, and says so.
    phone
        .supervisor
        .begin_sign_in(server.url.as_str(), APP_REDIRECT)
        .await
        .unwrap();
    phone.wait_for(SyncPhase::SigningIn).await;
    let forged = Url::parse(&format!(
        "{APP_REDIRECT}?code=x&state=forged&iss={}",
        server.url.as_str().trim_end_matches('/')
    ))
    .unwrap();
    assert!(phone.supervisor.finish_sign_in(&forged).await.is_err());
    let status = phone.wait_for(SyncPhase::SignedOut).await;
    assert!(status.message.is_some(), "{status:?}");

    // Cancelling a sign-in goes back to signed out.
    phone
        .supervisor
        .begin_sign_in(server.url.as_str(), APP_REDIRECT)
        .await
        .unwrap();
    phone.wait_for(SyncPhase::SigningIn).await;
    phone.supervisor.cancel_sign_in();
    phone.wait_for(SyncPhase::SignedOut).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_whose_own_address_cant_be_reached_says_so() {
    // Configured with a public URL nothing answers at, as when it's set to localhost on a server
    // a phone reaches by IP address.
    let mut overrides = password_server();
    overrides.push(("DRUSITE__PUBLIC_URL", "http://127.0.0.1:1"));
    let Some(server) = common::serve(&overrides).await else {
        return;
    };
    let phone = Device::new();
    let err = phone
        .supervisor
        .begin_sign_in(server.url.as_str(), APP_REDIRECT)
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("can't reach http://127.0.0.1:1/"), "{text}");
    assert!(text.contains("public_url"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_refresh_a_token_the_server_no_longer_takes() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let mut phone = Device::new();
    phone.sign_in(&server.url, "ada@example.com", true).await;
    phone.wait_for(SyncPhase::Online).await;
    let before = phone.refresh_token().unwrap();
    let ok = phone.supervisor.ask_status().await;
    assert!(ok.signed_in && ok.reason.as_deref() != Some("unreachable"), "{ok:?}");

    // The access token runs out on the server while the sync connection stays open: the
    // server's clock is ahead, or the device slept past it.
    sqlx::query("UPDATE access_tokens SET expires_at = now() - interval '1 second'")
        .execute(server.state.db().pool())
        .await
        .unwrap();

    // Ask's calls refresh and try again rather than show the refusal.
    let status = phone.supervisor.ask_status().await;
    assert!(status.signed_in, "{status:?}");
    assert_ne!(status.reason.as_deref(), Some("unreachable"), "{status:?}");
    let after = phone.refresh_token().unwrap();
    assert_ne!(after, before, "it refreshed once");
    phone.supervisor.conversations().await.unwrap();
    assert_eq!(phone.refresh_token().unwrap(), after, "and only once");
    // Sync never noticed.
    assert_eq!(phone.status.borrow().phase, SyncPhase::Online);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_short_lived_token_is_refreshed_before_it_runs_out() {
    let mut settings = password_server();
    settings.push(("DRUSITE__AUTH__ACCESS_TOKEN_MINUTES", "1"));
    let Some(server) = common::serve(&settings).await else {
        return;
    };
    let mut phone = Device::new();
    phone.sign_in(&server.url, "ada@example.com", true).await;
    phone.wait_for(SyncPhase::Online).await;
    let first = phone.refresh_token().unwrap();

    // A one-minute token is refreshed halfway, while the connection stays open.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(50);
    while phone.refresh_token().unwrap() == first {
        assert!(tokio::time::Instant::now() < deadline, "never refreshed");
        assert_eq!(phone.status.borrow().phase, SyncPhase::Online);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(phone.status.borrow().phase, SyncPhase::Online);
    // Past the first token's minute, requests still work without a refusal to retry.
    tokio::time::sleep(Duration::from_secs(35)).await;
    let status = phone.supervisor.ask_status().await;
    assert_ne!(status.reason.as_deref(), Some("unreachable"), "{status:?}");
    phone.create_page("Still syncing");
    assert_eq!(phone.status.borrow().phase, SyncPhase::Online);
}
