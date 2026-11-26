// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sync end to end: real cores with their own encrypted stores, syncing through a real server
//! (or two) and real PostgreSQL over WebSockets, with DPoP-bound tokens.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use drusite_core::api::{Command, Edit, Event, Response};
use drusite_core::sync::net::{self, NetError};
use drusite_core::{Core, StoreKey};
use drusite_dpop::SoftwareKey;
use drusite_model::{PageId, UserId, WorkspaceId};
use drusite_server::app::AppState;
use drusite_server::auth::session::Method;
use drusite_server::auth::tokens::{self, NewGrant};
use url::Url;

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// An account, made directly in the database.
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

/// One device: its own store, its own DPoP key, and a signed-in grant for `user`.
struct Device {
    core: Arc<Mutex<Core>>,
    key: SoftwareKey,
    access: String,
    events: Arc<Mutex<Vec<Event>>>,
    task: Option<tokio::task::JoinHandle<Result<(), NetError>>>,
    _dir: tempfile::TempDir,
}

impl Device {
    async fn new(state: &AppState, user: UserId, scope: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let core = Core::open(
            dir.path(),
            &StoreKey::generate().unwrap(),
            Box::new(move |e| sink.lock().unwrap().push(e)),
        )
        .unwrap();
        let key = SoftwareKey::generate().unwrap();
        let mut tx = state.db().pool().begin().await.unwrap();
        let (_, tokens) = tokens::create_grant(
            state,
            &mut tx,
            &NewGrant {
                user,
                client_id: "drusite-app",
                scope: scope.to_owned(),
                dpop_jkt: &key.jwk().thumbprint(),
                method: Method::Password,
                device_name: "test",
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        Self {
            core: Arc::new(Mutex::new(core)),
            key,
            access: tokens.access_token,
            events,
            task: None,
            _dir: dir,
        }
    }

    fn handle(&self, command: Command) -> Response {
        self.core.lock().unwrap().handle(command).unwrap()
    }

    fn workspace(&self) -> WorkspaceId {
        self.core.lock().unwrap().workspace_id()
    }

    fn connect(&mut self, server: &Url, join: Option<WorkspaceId>) {
        let core = Arc::clone(&self.core);
        let server = server.clone();
        let key = self.key.clone();
        let access = self.access.clone();
        self.task = Some(tokio::spawn(async move {
            net::run(core, &server, &access, &key, join, now()).await
        }));
    }

    /// Drops the connection, as if the network went away.
    fn disconnect(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    fn create_page(&self, title: &str) -> PageId {
        match self.handle(Command::CreatePage {
            parent: None,
            title: title.to_owned(),
        }) {
            Response::PageCreated { page } => page.id,
            other => panic!("{other:?}"),
        }
    }

    /// Types `text` at the end of the page's first block.
    fn type_text(&self, page: PageId, text: &str) {
        let Response::Opened { view } = self.handle(Command::OpenPage { page }) else {
            panic!()
        };
        let block = &view.blocks[0];
        let pos = u32::try_from(block.text().encode_utf16().count()).unwrap();
        self.handle(Command::Edit {
            page,
            edits: vec![Edit::InsertText {
                block: block.id,
                pos,
                text: text.to_owned(),
            }],
        });
        self.handle(Command::ClosePage { page });
    }

    fn titles(&self) -> Vec<String> {
        let Response::Pages { pages } = self.handle(Command::ListPages) else {
            panic!()
        };
        let mut titles: Vec<String> = pages.into_iter().map(|p| p.title).collect();
        titles.sort();
        titles
    }

    /// The text of a page, or `None` if the device doesn't have it (yet).
    fn text(&self, page: PageId) -> Option<String> {
        let mut core = self.core.lock().unwrap();
        let Ok(Response::Opened { view }) = core.handle(Command::OpenPage { page }) else {
            return None;
        };
        core.handle(Command::ClosePage { page }).unwrap();
        Some(
            view.blocks
                .iter()
                .map(drusite_model::Block::text)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

/// Waits until `check` holds, for up to ten seconds.
async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never happened: {what}");
}

fn password_server() -> Vec<(&'static str, &'static str)> {
    vec![("DRUSITE__AUTH__PASSWORD__ENABLED", "true")]
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_stay_in_sync_and_merge_offline_edits() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let user = account(&server.state, "ada@example.com").await;

    // The Mac has notes before it ever syncs.
    let mut mac = Device::new(&server.state, user, "read write").await;
    let notes = mac.create_page("Notes");
    mac.type_text(notes, "written offline on the Mac");
    mac.connect(&server.url, None);

    // The phone is new: it takes the account's workspace instead of its own empty one.
    let mut phone = Device::new(&server.state, user, "read write").await;
    assert!(phone.core.lock().unwrap().is_blank().unwrap());
    let ws = mac.workspace();
    eventually("the Mac's workspace is on the server", || {
        server_has_doc(&server, *ws.as_uuid())
    })
    .await;
    phone.connect(&server.url, Some(ws));
    eventually("the phone has the Mac's page", || {
        phone.text(notes).as_deref() == Some("written offline on the Mac")
    })
    .await;
    assert_eq!(phone.workspace(), ws);
    assert_eq!(phone.titles(), ["Notes"]);
    assert!(
        phone
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, Event::PagesChanged { .. })),
        "the phone's UI heard about the new tree"
    );

    // Live, both ways, including a page made on the phone.
    phone.type_text(notes, ", and on the phone");
    eventually("the Mac sees the phone's typing", || {
        mac.text(notes).as_deref() == Some("written offline on the Mac, and on the phone")
    })
    .await;
    let todo = phone.create_page("To do");
    phone.type_text(todo, "milk");
    eventually("the Mac sees the phone's new page", || {
        mac.text(todo).as_deref() == Some("milk")
    })
    .await;
    assert_eq!(mac.titles(), ["Notes", "To do"]);

    // Both go offline and edit the same page; nothing is lost when they come back.
    mac.disconnect();
    phone.disconnect();
    mac.type_text(todo, " eggs");
    phone.type_text(todo, " bread");
    let away = mac.create_page("Written on a plane");
    mac.connect(&server.url, None);
    phone.connect(&server.url, None);
    let merged = |t: &str| t.starts_with("milk") && t.contains(" eggs") && t.contains(" bread");
    eventually("both have both offline edits", || {
        mac.text(todo).is_some_and(|t| merged(&t)) && phone.text(todo).is_some_and(|t| merged(&t))
    })
    .await;
    assert_eq!(mac.text(todo), phone.text(todo), "they agree on the order");
    eventually("the plane page reached the phone", || {
        phone.text(away).is_some()
    })
    .await;

    // A restarted device asks only for what's new: its store remembers how far it got.
    mac.disconnect();
    let seq_before: i64 = doc_head(&server, *todo.as_uuid()).await;
    phone.type_text(todo, "!");
    for _ in 0..200 {
        if doc_head(&server, *todo.as_uuid()).await > seq_before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    mac.connect(&server.url, None);
    eventually("the Mac catches up after reconnecting", || {
        mac.text(todo).is_some_and(|t| t.ends_with('!'))
    })
    .await;
    assert!(doc_head(&server, *todo.as_uuid()).await > seq_before);
}

fn server_has_doc(server: &common::Server, doc: uuid::Uuid) -> bool {
    let pool = server.db.admin.pool().clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM documents WHERE id = $1)")
                .bind(doc)
                .fetch_one(&pool)
                .await
                .unwrap()
        })
    })
}

async fn doc_head(server: &common::Server, doc: uuid::Uuid) -> i64 {
    sqlx::query_scalar("SELECT head_seq FROM documents WHERE id = $1")
        .bind(doc)
        .fetch_one(server.db.admin.pool())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn other_accounts_and_read_only_tokens_are_refused() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let ada = account(&server.state, "ada@example.com").await;
    let eve = account(&server.state, "eve@example.com").await;

    let mut mac = Device::new(&server.state, ada, "read write").await;
    let page = mac.create_page("Private");
    mac.type_text(page, "secret plans");
    mac.connect(&server.url, None);
    let ws = mac.workspace();
    eventually("the page is on the server", || {
        server_has_doc(&server, *page.as_uuid())
    })
    .await;

    // Another account can't join it.
    let mut eve_phone = Device::new(&server.state, eve, "read write").await;
    eve_phone.connect(&server.url, Some(ws));
    let result = eve_phone.task.take().unwrap().await.unwrap();
    assert!(matches!(result, Err(NetError::Sync(_))), "{result:?}");
    assert_eq!(eve_phone.text(page), None);

    // A token with only `read` can sync down but its pushes go nowhere.
    let mut reader = Device::new(&server.state, ada, "read").await;
    reader.connect(&server.url, Some(ws));
    eventually("the reader has the page", || {
        reader.text(page).as_deref() == Some("secret plans")
    })
    .await;
    let head = doc_head(&server, *page.as_uuid()).await;
    reader.type_text(page, " (edited by the reader)");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(doc_head(&server, *page.as_uuid()).await, head);
    assert_eq!(mac.text(page).as_deref(), Some("secret plans"));

    // A device that has been signed out can't connect.
    let mut again = Device::new(&server.state, ada, "read write").await;
    sqlx::query("UPDATE oauth_grants SET revoked_at = now()")
        .execute(server.db.admin.pool())
        .await
        .unwrap();
    again.connect(&server.url, None);
    let result = again.task.take().unwrap().await.unwrap();
    assert!(matches!(result, Err(NetError::Unauthorized)), "{result:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn what_the_server_stores_is_sealed_and_compacted() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let user = account(&server.state, "ada@example.com").await;
    let mut mac = Device::new(&server.state, user, "read write").await;
    let page = mac.create_page("Diary");
    mac.connect(&server.url, None);
    eventually("the page is on the server", || {
        server_has_doc(&server, *page.as_uuid())
    })
    .await;

    // Past the compaction threshold, one stored push per keystroke: pushes made while one is
    // in flight are batched, so each keystroke waits for the last to be stored.
    let mut expected = String::new();
    let mut head = doc_head(&server, *page.as_uuid()).await;
    let Response::Opened { view } = mac.handle(Command::OpenPage { page }) else {
        panic!()
    };
    let block = view.blocks[0].id;
    for i in 0..(drusite_server::sync::store::COMPACT_AFTER + 20) {
        let c = char::from(b'a' + u8::try_from(i % 26).unwrap());
        mac.handle(Command::Edit {
            page,
            edits: vec![Edit::InsertText {
                block,
                pos: u32::try_from(expected.len()).unwrap(),
                text: c.to_string(),
            }],
        });
        expected.push(c);
        for _ in 0..200 {
            let now = doc_head(&server, *page.as_uuid()).await;
            if now > head {
                head = now;
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    mac.handle(Command::ClosePage { page });
    eventually("compacted", || {
        let pool = server.db.admin.pool().clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                sqlx::query_scalar::<_, i64>("SELECT snapshot_seq FROM documents WHERE id = $1")
                    .bind(page.as_uuid())
                    .fetch_one(&pool)
                    .await
                    .unwrap()
                    > 0
            })
        })
    })
    .await;

    // Nothing stored reads as the text.
    let rows: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT data FROM document_updates UNION ALL SELECT data FROM document_snapshots",
    )
    .fetch_all(server.db.admin.pool())
    .await
    .unwrap();
    assert!(!rows.is_empty());
    for row in rows {
        assert!(
            !row.windows(6).any(|w| w == b"abcdef"),
            "plaintext in the database"
        );
    }

    // A new device gets the snapshot plus what came after, and the whole text.
    let mut phone = Device::new(&server.state, user, "read write").await;
    phone.connect(&server.url, Some(mac.workspace()));
    eventually("the phone has everything", || {
        phone.text(page).as_deref() == Some(expected.as_str())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn devices_on_different_server_nodes_see_each_other() {
    let Some(server) = common::serve(&password_server()).await else {
        return;
    };
    let (node2, _) = server.second_node().await;
    let user = account(&server.state, "ada@example.com").await;
    let mut mac = Device::new(&server.state, user, "read write").await;
    let page = mac.create_page("Shared");
    mac.connect(&server.url, None);
    eventually("on the server", || server_has_doc(&server, *page.as_uuid())).await;
    let mut phone = Device::new(&server.state, user, "read write").await;
    phone.connect(&node2, Some(mac.workspace()));
    eventually("the phone, on node 2, has the page", || {
        phone.text(page).is_some()
    })
    .await;
    mac.type_text(page, "from node 1");
    eventually("node 2 passes it on", || {
        phone.text(page).as_deref() == Some("from node 1")
    })
    .await;
    phone.type_text(page, " and back");
    eventually("node 1 passes it on", || {
        mac.text(page).as_deref() == Some("from node 1 and back")
    })
    .await;
}
