// SPDX-License-Identifier: AGPL-3.0-or-later

//! Server search end to end: a device syncs its pages, the indexer picks them up, and searches
//! over HTTP (with DPoP) find them by their words and by their meaning, for their owner only.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{Json, Router, routing::post};
use drusite_core::api::{Command, Edit, Response};
use drusite_core::{Core, StoreKey};
use drusite_dpop::SoftwareKey;
use drusite_model::{PageId, UserId};
use drusite_server::app::AppState;
use drusite_server::auth::session::Method;
use drusite_server::auth::tokens::{self, NewGrant};
use url::Url;

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// The fake model's vector space: one dimension per idea, so words for the same idea are close.
const IDEAS: [&[&str]; 4] = [
    &["dog", "puppy", "hound", "barked"],
    &["cat", "kitten"],
    &["lake", "river", "swam", "swim"],
    &["bread", "baked", "oven"],
];

fn fake_vector(text: &str) -> Vec<f32> {
    let lower = text.to_lowercase();
    let mut v: Vec<f32> = IDEAS
        .iter()
        .map(|words| words.iter().filter(|w| lower.contains(**w)).count() as f32)
        .collect();
    // Never all zeros: cosine distance needs a direction.
    v.push(0.01);
    v
}

/// An OpenAI-compatible embeddings endpoint backed by `fake_vector`.
async fn fake_embeddings() -> Url {
    async fn embed(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
        let data: Vec<_> = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, t)| serde_json::json!({"index": i, "embedding": fake_vector(t.as_str().unwrap())}))
            .collect();
        Json(serde_json::json!({"data": data}))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/v1", listener.local_addr().unwrap())).unwrap();
    tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/v1/embeddings", post(embed)))
            .await
            .unwrap();
    });
    url
}

async fn account(state: &AppState, email: &str) -> UserId {
    let id = UserId::new();
    sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
        .bind(id.as_uuid())
        .bind(email)
        .execute(state.db().pool())
        .await
        .unwrap();
    id
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

    async fn search(&self, server: &Url, q: &str) -> serde_json::Value {
        let mut url = server.join("api/v1/search").unwrap();
        url.query_pairs_mut().append_pair("q", q);
        let mut htu = url.clone();
        htu.set_query(None);
        let proof =
            drusite_core::auth::proof(&self.key, "GET", &htu, Some(&self.access), now()).unwrap();
        let response = reqwest::Client::new()
            .get(url)
            .header("authorization", format!("DPoP {}", self.access))
            .header("dpop", proof)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
        response.json().await.unwrap()
    }
}

fn pages(result: &serde_json::Value) -> Vec<(String, Vec<String>)> {
    result["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["title"].as_str().unwrap().to_owned(),
                h["matched"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| m.as_str().unwrap().to_owned())
                    .collect(),
            )
        })
        .collect()
}

/// Searches until `check` holds, for up to twenty seconds (the indexer waits for edits to
/// settle).
async fn until(
    device: &Device,
    server: &Url,
    q: &str,
    check: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    for _ in 0..100 {
        let result = device.search(server, q).await;
        if check(&result) {
            return result;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!(
        "never matched for {q:?}: {}",
        device.search(server, q).await
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn finds_by_words_and_meaning_for_the_owner_only() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let embeddings = fake_embeddings().await;
    let Some(server) = common::serve(&[
        ("DRUSITE__AUTH__PASSWORD__ENABLED", "true"),
        ("DRUSITE__SEARCH__EMBEDDINGS__BASE_URL", embeddings.as_str()),
        ("DRUSITE__SEARCH__EMBEDDINGS__MODEL", "fake-ideas"),
    ])
    .await
    else {
        return;
    };
    drusite_server::search::spawn_indexer(server.state.clone());
    let alice = account(&server.state, "alice@example.com").await;
    let bob = account(&server.state, "bob@example.com").await;
    let mac = Device::new(&server.state, alice).await;
    mac.page("Pets", "My dog barked at the mailman");
    let trips = mac.page("Trips", "Swam in the lake at dawn");
    mac.connect(&server.url);

    // By its words: the block, its page, the word marked.
    let found = until(&mac, &server.url, "barked", |r| {
        !r["hits"].as_array().unwrap().is_empty()
    })
    .await;
    let hit = &found["hits"][0];
    assert_eq!(hit["title"], "Pets");
    assert!(hit["block"].is_string());
    assert!(hit["matched"].as_array().unwrap().contains(&"words".into()));
    assert!(
        hit["snippet"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["highlight"] == true && p["text"] == "barked")
    );
    assert_eq!(found["semantic"], true);
    assert_eq!(found["model"], "fake-ideas");

    // By meaning alone: "puppy" isn't on any page.
    let found = until(&mac, &server.url, "puppy", |r| {
        !r["hits"].as_array().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        pages(&found)[0],
        ("Pets".to_owned(), vec!["meaning".to_owned()])
    );

    // A title matches as the page itself.
    let found = until(&mac, &server.url, "trips", |r| {
        !r["hits"].as_array().unwrap().is_empty()
    })
    .await;
    assert_eq!(found["hits"][0]["title"], "Trips");
    assert!(found["hits"][0]["block"].is_null());

    // Someone else's search never sees them.
    let other = Device::new(&server.state, bob).await;
    let theirs = other.search(&server.url, "dog").await;
    assert!(theirs["hits"].as_array().unwrap().is_empty(), "{theirs}");

    // A page in the trash leaves the index, and comes back when restored.
    mac.handle(Command::DeletePage { page: trips });
    until(&mac, &server.url, "lake", |r| {
        r["hits"].as_array().unwrap().is_empty()
    })
    .await;
    mac.handle(Command::RestorePage { page: trips });
    until(&mac, &server.url, "lake", |r| {
        !r["hits"].as_array().unwrap().is_empty()
    })
    .await;
    mac.handle(Command::DeletePage { page: trips });
    mac.handle(Command::DeleteForever { page: trips });
    until(&mac, &server.url, "lake", |r| {
        r["hits"].as_array().unwrap().is_empty()
    })
    .await;

    // What's stored: words and vectors, keyed hashes, never the text.
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT tsv::text, encode(content_hash, 'hex') FROM search_blocks")
            .fetch_all(server.db.admin.pool())
            .await
            .unwrap();
    assert!(rows.iter().any(|(tsv, _)| tsv.contains("'mailman'")));
    let plain = blake3::hash(b"My dog barked at the mailman")
        .to_hex()
        .to_string();
    assert!(rows.iter().all(|(_, h)| *h != plain));
    let models: Vec<String> = sqlx::query_scalar("SELECT DISTINCT model FROM search_embeddings")
        .fetch_all(server.db.admin.pool())
        .await
        .unwrap();
    assert_eq!(models, ["fake-ideas"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn words_only_without_a_model_and_off_when_disabled() {
    let Some(server) = common::serve(&[("DRUSITE__AUTH__PASSWORD__ENABLED", "true")]).await else {
        return;
    };
    drusite_server::search::spawn_indexer(server.state.clone());
    let alice = account(&server.state, "alice@example.com").await;
    let mac = Device::new(&server.state, alice).await;
    mac.page("Baking", "Sourdough baked in the oven");
    mac.connect(&server.url);
    let found = until(&mac, &server.url, "sourdough", |r| {
        !r["hits"].as_array().unwrap().is_empty()
    })
    .await;
    assert_eq!(found["semantic"], false);
    assert_eq!(
        pages(&found)[0],
        ("Baking".to_owned(), vec!["words".to_owned()])
    );

    let Some(off) = common::serve(&[
        ("DRUSITE__AUTH__PASSWORD__ENABLED", "true"),
        ("DRUSITE__SEARCH__ENABLED", "false"),
    ])
    .await
    else {
        return;
    };
    let bob = account(&off.state, "bob@example.com").await;
    let device = Device::new(&off.state, bob).await;
    let mut url = off.url.join("api/v1/search").unwrap();
    url.query_pairs_mut().append_pair("q", "x");
    let proof = drusite_core::auth::proof(
        &device.key,
        "GET",
        &off.url.join("api/v1/search").unwrap(),
        Some(&device.access),
        now(),
    )
    .unwrap();
    let response = reqwest::Client::new()
        .get(url)
        .header("authorization", format!("DPoP {}", device.access))
        .header("dpop", proof)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}
