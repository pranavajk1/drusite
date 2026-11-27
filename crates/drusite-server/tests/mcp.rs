// SPDX-License-Identifier: AGPL-3.0-or-later

//! Agents end to end: an MCP client with a Client ID Metadata Document signs in, the person
//! chooses what it may do, it reads and writes a page over MCP, a device sees the change, and
//! the person undoes it from the app.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{Json, Router, routing::get};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::browser::{Browser, location};
use drusite_core::api::{Command, Edit, Response};
use drusite_core::{Core, StoreKey};
use drusite_dpop::SoftwareKey;
use drusite_model::{PageId, UserId};
use drusite_server::app::AppState;
use drusite_server::auth::session::Method;
use drusite_server::auth::tokens::{self, NewGrant};
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use url::Url;

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// The agent's metadata document, served where its client ID says.
async fn agent_client(name: &str) -> (String, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let id = format!("{base}/oauth/client.json");
    let redirect = "http://localhost:41999/callback".to_owned();
    let doc = json!({
        "client_id": id,
        "client_name": name,
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_method": "none",
    });
    tokio::spawn(async move {
        let app = Router::new().route(
            "/oauth/client.json",
            get(move || {
                let doc = doc.clone();
                async move { Json(doc) }
            }),
        );
        axum::serve(listener, app).await.unwrap();
    });
    (id, redirect)
}

struct Device {
    core: Arc<Mutex<Core>>,
    key: SoftwareKey,
    access: String,
    _dir: tempfile::TempDir,
}

impl Device {
    async fn new(state: &AppState, user: UserId) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let core =
            Core::open(dir.path(), &StoreKey::generate().unwrap(), Box::new(|_| {})).unwrap();
        let key = SoftwareKey::generate().unwrap();
        let mut tx = state.db().pool().begin().await.unwrap();
        let (_, tokens) = tokens::create_grant(
            state,
            &mut tx,
            &NewGrant {
                user,
                client_id: "drusite-app",
                scope: "read write".to_owned(),
                dpop_jkt: Some(&key.jwk().thumbprint()),
                method: Method::Password,
                device_name: "test",
                resource: None,
                access: None,
                client_name: "",
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        Self {
            core: Arc::new(Mutex::new(core)),
            key,
            access: tokens.access_token,
            _dir: dir,
        }
    }

    fn handle(&self, command: Command) -> Response {
        self.core.lock().unwrap().handle(command).unwrap()
    }

    fn connect(&self, server: &Url) {
        let core = Arc::clone(&self.core);
        let server = server.clone();
        let key = self.key.clone();
        let access = self.access.clone();
        tokio::spawn(async move {
            drusite_core::sync::net::run(core, &server, &access, &key, None, now()).await
        });
    }

    fn page(&self, title: &str, text: &str) -> PageId {
        let Response::PageCreated { page } = self.handle(Command::CreatePage {
            parent: None,
            title: title.into(),
        }) else {
            panic!()
        };
        let Response::Opened { view } = self.handle(Command::OpenPage { page: page.id }) else {
            panic!()
        };
        self.handle(Command::Edit {
            page: page.id,
            edits: vec![Edit::InsertText {
                block: view.blocks[0].id,
                pos: 0,
                text: text.into(),
            }],
        });
        self.handle(Command::ClosePage { page: page.id });
        page.id
    }

    fn texts(&self, page: PageId) -> Vec<String> {
        let Response::Opened { view } = self.handle(Command::OpenPage { page }) else {
            panic!()
        };
        self.handle(Command::ClosePage { page });
        view.blocks.iter().map(drusite_model::Block::text).collect()
    }

    fn workspace(&self) -> uuid::Uuid {
        *self.core.lock().unwrap().workspace_id().as_uuid()
    }

    /// A request to the server as the app, with a DPoP proof.
    async fn call(&self, server: &Url, method: &str, path: &str) -> (StatusCode, Value) {
        let url = server.join(path).unwrap();
        let proof =
            drusite_core::auth::proof(&self.key, method, &url, Some(&self.access), now()).unwrap();
        let response = reqwest::Client::new()
            .request(method.parse().unwrap(), url)
            .header("authorization", format!("DPoP {}", self.access))
            .header("dpop", proof)
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

/// An MCP client over plain HTTP.
struct Mcp {
    url: Url,
    token: String,
    http: reqwest::Client,
}

impl Mcp {
    async fn post(&self, body: &Value) -> reqwest::Response {
        self.http
            .post(self.url.clone())
            .header("authorization", format!("Bearer {}", self.token))
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-06-18")
            .json(body)
            .send()
            .await
            .unwrap()
    }

    async fn rpc(&self, method: &str, params: Value) -> Value {
        let response = self
            .post(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
            .await;
        let status = response.status();
        let text = response.text().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{method}: {text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        assert!(value.get("error").is_none(), "{method}: {value}");
        value["result"].clone()
    }

    async fn initialize(&self) -> Value {
        let result = self
            .rpc(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "test-agent", "version": "1" },
                }),
            )
            .await;
        let response = self
            .post(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        assert!(response.status().is_success(), "{}", response.status());
        result
    }

    /// Calls a tool; returns its text and whether it was an error.
    async fn tool(&self, name: &str, arguments: Value) -> (String, bool) {
        let result = self
            .rpc(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
        (
            result["content"][0]["text"].as_str().unwrap().to_owned(),
            result["isError"].as_bool().unwrap_or(false),
        )
    }
}

fn pkce() -> (String, String) {
    let verifier = URL_SAFE_NO_PAD.encode([7u8; 40]);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// Runs the agent's OAuth flow in `browser` (signed in), answering the consent page with
/// `answer`. Returns the token response, or the error the callback got.
async fn connect_agent(
    server: &Url,
    browser: &mut Browser,
    client_id: &str,
    redirect: &str,
    answer: &[(&str, &str)],
) -> Result<Value, String> {
    let (verifier, challenge) = pkce();
    let mut authorize = server.join("oauth/authorize").unwrap();
    authorize
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("scope", "read write")
        .append_pair("state", "xyz")
        .append_pair("resource", server.join("mcp").unwrap().as_str());
    let response = browser.get(&authorize).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let consent = location(&response, server);
    assert_eq!(consent.path(), "/oauth/consent", "{consent}");
    let page = browser.get(&consent).await;
    assert_eq!(page.status(), StatusCode::OK);
    let html = page.text().await.unwrap();
    assert!(html.contains("Connect Test Agent (127.0.0.1)?"), "{html}");
    let request = consent
        .query_pairs()
        .find(|(k, _)| k == "request")
        .unwrap()
        .1
        .into_owned();
    // The workspace checkbox's value, as the page lists it.
    let ws = html
        .split("name=\"workspace\" value=\"")
        .nth(1)
        .map(|rest| rest.split('"').next().unwrap().to_owned());
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.append_pair("request", &request);
    if let Some(ws) = &ws {
        form.append_pair("workspace", ws);
    }
    for (k, v) in answer {
        form.append_pair(k, v);
    }
    let response = browser
        .http
        .post(server.join("oauth/consent").unwrap())
        .header("cookie", browser.cookie_header())
        .header("origin", server.origin().ascii_serialization())
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form.finish())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let callback = location(&response, server);
    assert!(callback.as_str().starts_with(redirect), "{callback}");
    let pairs: std::collections::HashMap<_, _> = callback.query_pairs().into_owned().collect();
    assert_eq!(pairs["state"], "xyz");
    if let Some(error) = pairs.get("error") {
        return Err(error.clone());
    }
    let response = reqwest::Client::new()
        .post(server.join("oauth/token").unwrap())
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &pairs["code"]),
            ("redirect_uri", redirect),
            ("code_verifier", &verifier),
            ("client_id", client_id),
            ("resource", server.join("mcp").unwrap().as_str()),
        ])
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(body)
}

async fn until<F: Fn() -> bool>(what: &str, check: F) {
    for _ in 0..150 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{what} never happened");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_connects_reads_writes_and_the_person_undoes_it() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let Some(server) = common::serve(&[
        ("DRUSITE__AUTH__PASSWORD__ENABLED", "true"),
        ("DRUSITE__AUTH__SELF_SIGNUP", "true"),
        ("DRUSITE__AGENTS__ALLOW_INSECURE_CLIENT_IDS", "true"),
    ])
    .await
    else {
        return;
    };
    let base = server.url.clone();
    let mcp_url = base.join("mcp").unwrap();

    // Without a token, the endpoint says where to sign in (RFC 9728).
    let response = reqwest::Client::new()
        .post(mcp_url.clone())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response.headers()["www-authenticate"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        challenge.contains("/.well-known/oauth-protected-resource/mcp"),
        "{challenge}"
    );
    let metadata: Value = reqwest::get(
        base.join(".well-known/oauth-protected-resource/mcp")
            .unwrap(),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(metadata["resource"], mcp_url.as_str());
    assert_eq!(
        metadata["authorization_servers"][0],
        base.as_str().trim_end_matches('/')
    );
    let as_meta: Value = reqwest::get(base.join(".well-known/oauth-authorization-server").unwrap())
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(as_meta["client_id_metadata_document_supported"], true);

    // The person signs up in the browser; their Mac syncs a page.
    let mut browser = Browser::new(&base);
    let (status, body) = browser
        .post(
            "/api/v1/auth/password/signup",
            json!({ "email": "ada@example.com", "password": "correct horse battery",
                    "name": "Ada" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let user: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind("ada@example.com")
        .fetch_one(server.state.db().pool())
        .await
        .unwrap();
    let mac = Device::new(&server.state, UserId::from_uuid(user)).await;
    let page = mac.page("Groceries", "bread");
    mac.connect(&base);
    let ws = mac.workspace();
    let pool = server.state.db().pool().clone();
    for _ in 0..100 {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM documents WHERE id = $1")
            .bind(page.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
        if n == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A read-only connection can read and can't write.
    let (client_id, redirect) = agent_client("Test Agent").await;
    let tokens = connect_agent(
        &base,
        &mut browser,
        &client_id,
        &redirect,
        &[("access", "read"), ("decision", "allow")],
    )
    .await
    .unwrap();
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["scope"], "read");
    let reader = Mcp {
        url: mcp_url.clone(),
        token: tokens["access_token"].as_str().unwrap().to_owned(),
        http: reqwest::Client::new(),
    };
    let info = reader.initialize().await;
    assert_eq!(info["serverInfo"]["name"], "drusite");
    let tools = reader.rpc("tools/list", json!({})).await;
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"read_page") && names.contains(&"append_to_page"),
        "{names:?}"
    );
    let (md, err) = reader
        .tool("read_page", json!({ "page": page.to_string() }))
        .await;
    assert!(!err, "{md}");
    assert!(
        md.contains("drusite-title: Groceries") && md.contains("bread ^"),
        "{md}"
    );
    let (said, err) = reader
        .tool(
            "append_to_page",
            json!({ "page": page.to_string(), "markdown": "milk" }),
        )
        .await;
    assert!(err && said.contains("read only"), "{said}");

    // Saying no sends the agent away with nothing.
    let denied = connect_agent(
        &base,
        &mut browser,
        &client_id,
        &redirect,
        &[("access", "write"), ("decision", "deny")],
    )
    .await;
    assert_eq!(denied.unwrap_err(), "access_denied");

    // A writer adds to the page; the Mac sees it.
    let tokens = connect_agent(
        &base,
        &mut browser,
        &client_id,
        &redirect,
        &[("access", "write"), ("decision", "allow")],
    )
    .await
    .unwrap();
    assert_eq!(tokens["scope"], "read write");
    let writer = Mcp {
        url: mcp_url.clone(),
        token: tokens["access_token"].as_str().unwrap().to_owned(),
        http: reqwest::Client::new(),
    };
    writer.initialize().await;
    let (said, err) = writer
        .tool(
            "append_to_page",
            json!({ "page": page.to_string(), "markdown": "- milk\n- eggs" }),
        )
        .await;
    assert!(!err, "{said}");
    assert_eq!(said, "Added 2 blocks to “Groceries”.");
    until("the Mac getting the agent's edit", || {
        mac.texts(page) == ["bread", "milk", "eggs"]
    })
    .await;

    // A page the agent makes reaches the Mac's tree too.
    let (said, err) = writer
        .tool(
            "create_page",
            json!({ "title": "Recipes", "markdown": "Soup" }),
        )
        .await;
    assert!(!err, "{said}");

    // The app lists what the agent did, newest first, and undoes the append.
    let (status, list) = mac
        .call(
            &base,
            "GET",
            &format!("api/v1/workspaces/{ws}/agent-activity"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let items = list.as_array().unwrap();
    assert_eq!(items.len(), 2, "{list}");
    assert_eq!(items[0]["summary"], "Created “Recipes”");
    assert_eq!(items[1]["summary"], "Added 2 blocks to “Groceries”");
    assert_eq!(items[1]["agent"], "Test Agent (127.0.0.1)");
    let id = items[1]["id"].as_str().unwrap();
    let (status, body) = mac
        .call(
            &base,
            "POST",
            &format!("api/v1/workspaces/{ws}/agent-activity/{id}/undo"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    until("the Mac getting the undo", || mac.texts(page) == ["bread"]).await;
    let (_, list) = mac
        .call(
            &base,
            "GET",
            &format!("api/v1/workspaces/{ws}/agent-activity"),
        )
        .await;
    assert!(list[1]["undoneAt"].is_number(), "{list}");

    // Tokens stay where they belong: the agent's can't use the app's API, and the app's can't
    // use MCP.
    let response = reqwest::Client::new()
        .get(base.join("api/v1/me").unwrap())
        .header("authorization", format!("Bearer {}", writer.token))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let app_as_agent = Mcp {
        url: mcp_url,
        token: mac.access.clone(),
        http: reqwest::Client::new(),
    };
    let response = app_as_agent
        .post(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
