// SPDX-License-Identifier: AGPL-3.0-or-later

//! The local MCP endpoint: an agent on this computer with a token from the app reads and
//! writes a page; the change shows in the app and can be undone; nothing else gets in.

#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use drusite_core::api::{AgentAccess, AgentSource, Command, Edit, Response};
use drusite_core::{Core, StoreKey};
use drusite_model::PageId;
use serde_json::{Value, json};

struct Fixture {
    core: Arc<Mutex<Core>>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let core =
            Core::open(dir.path(), &StoreKey::generate().unwrap(), Box::new(|_| {})).unwrap();
        Self {
            core: Arc::new(Mutex::new(core)),
            _dir: dir,
        }
    }

    fn handle(&self, command: Command) -> Response {
        self.core.lock().unwrap().handle(command).unwrap()
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

    fn token(&self, name: &str, write: bool) -> String {
        let Response::AgentToken { token, .. } = self.handle(Command::CreateAgentToken {
            name: name.into(),
            access: AgentAccess {
                write,
                journal: false,
            },
        }) else {
            panic!()
        };
        token
    }
}

/// One JSON-RPC call; returns the status and the body.
async fn post(url: &str, token: Option<&str>, origin: Option<&str>, body: Value) -> (u16, Value) {
    let url = url.to_owned();
    let token = token.map(str::to_owned);
    let origin = origin.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut request = agent
            .post(&url)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-06-18");
        if let Some(t) = &token {
            request = request.header("authorization", &format!("Bearer {t}"));
        }
        if let Some(o) = &origin {
            request = request.header("origin", o);
        }
        let mut response = request.send_json(&body).unwrap();
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        (status, body)
    })
    .await
    .unwrap()
}

async fn tool(url: &str, token: &str, name: &str, arguments: Value) -> (String, bool) {
    let (status, body) = post(
        url,
        Some(token),
        None,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": arguments } }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let result = &body["result"];
    (
        result["content"][0]["text"].as_str().unwrap().to_owned(),
        result["isError"].as_bool().unwrap_or(false),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_here_reads_writes_and_is_undone() {
    let f = Fixture::new();
    let page = f.page("Inbox", "first");
    let writer = f.token("Claude Code", true);
    let reader = f.token("Reader", false);
    let server = drusite_core::agents::local::serve(Arc::clone(&f.core), 0, "test")
        .await
        .unwrap();
    let url = format!("http://127.0.0.1:{}/mcp", server.port);

    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": { "name": "t", "version": "1" } } });
    // No token, a wrong one, or a web page: nothing.
    assert_eq!(post(&url, None, None, init.clone()).await.0, 401);
    assert_eq!(
        post(&url, Some("drusite_local_nope"), None, init.clone())
            .await
            .0,
        401
    );
    let (status, _) = post(
        &url,
        Some(&writer),
        Some("https://evil.example"),
        init.clone(),
    )
    .await;
    assert_eq!(status, 403);

    let (status, body) = post(&url, Some(&writer), None, init).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["serverInfo"]["name"], "drusite");

    let (md, err) = tool(
        &url,
        &reader,
        "read_page",
        json!({ "page": page.to_string() }),
    )
    .await;
    assert!(!err && md.contains("drusite-title: Inbox"), "{md}");
    let (said, err) = tool(
        &url,
        &reader,
        "append_to_page",
        json!({ "page": page.to_string(), "markdown": "nope" }),
    )
    .await;
    assert!(err && said.contains("read only"), "{said}");

    let (said, err) = tool(
        &url,
        &writer,
        "append_to_page",
        json!({ "page": page.to_string(), "markdown": "second" }),
    )
    .await;
    assert!(!err, "{said}");
    assert_eq!(f.texts(page), ["first", "second"]);

    let Response::Agents {
        activity, tokens, ..
    } = f.handle(Command::Agents)
    else {
        panic!()
    };
    assert_eq!(tokens.len(), 2);
    assert!(tokens.iter().all(|t| t.last_used_at.is_some()));
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].agent, "Claude Code");
    assert_eq!(activity[0].summary, "Added a block to “Inbox”");
    assert_eq!(activity[0].pages, [page]);

    f.handle(Command::UndoAgentCall {
        id: activity[0].id.clone(),
        source: AgentSource::Device,
    });
    assert_eq!(f.texts(page), ["first"]);
    let Response::Agents { activity, .. } = f.handle(Command::Agents) else {
        panic!()
    };
    assert!(activity[0].undone_at.is_some());

    // A disconnected agent is out at once.
    let Response::Agents { tokens, .. } = f.handle(Command::Agents) else {
        panic!()
    };
    let id = tokens
        .iter()
        .find(|t| t.name == "Claude Code")
        .unwrap()
        .id
        .clone();
    f.handle(Command::RevokeAgentToken { id });
    let (status, _) = post(
        &url,
        Some(&writer),
        None,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    )
    .await;
    assert_eq!(status, 401);
    drop(server);
}
