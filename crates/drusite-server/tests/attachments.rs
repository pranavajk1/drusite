// SPDX-License-Identifier: AGPL-3.0-or-later

//! Attachments end to end: a device attaches a file and syncs, the server keeps it sealed
//! (found by a keyed hash, never the file's own), another device of the same account fetches
//! it when asked, and nobody else can.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use drusite_core::api::{Command, Edit, ErrorCode, Response};
use drusite_core::{Core, StoreKey};
use drusite_dpop::SoftwareKey;
use drusite_model::{UserId, WorkspaceId};
use drusite_server::app::AppState;
use drusite_server::auth::session::Method;
use drusite_server::auth::tokens::{self, NewGrant};
use url::Url;

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
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
    async fn new(state: &AppState, user: UserId, scope: &str) -> Self {
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
            _dir: dir,
        }
    }

    fn handle(&self, command: Command) -> Result<Response, drusite_core::api::ApiError> {
        self.core.lock().unwrap().handle(command)
    }

    fn connect(&self, server: &Url, join: Option<WorkspaceId>) {
        let core = Arc::clone(&self.core);
        let server = server.clone();
        let key = self.key.clone();
        let access = self.access.clone();
        tokio::spawn(async move {
            drusite_core::sync::net::run(core, &server, &access, &key, join, now()).await
        });
    }

    fn workspace(&self) -> WorkspaceId {
        self.core.lock().unwrap().workspace_id()
    }

    /// A raw request to the attachment endpoint, with this device's token.
    async fn request(
        &self,
        server: &Url,
        method: &str,
        ws: WorkspaceId,
        blob: &str,
        body: Option<Vec<u8>>,
    ) -> u16 {
        let url = server
            .join(&format!("api/v1/workspaces/{ws}/attachments/{blob}"))
            .unwrap();
        let proof =
            drusite_core::auth::proof(&self.key, method, &url, Some(&self.access), now()).unwrap();
        let client = reqwest::Client::new();
        let request = match body {
            Some(b) => client.put(url).body(b),
            None => client.get(url),
        };
        request
            .header("authorization", format!("DPoP {}", self.access))
            .header("dpop", proof)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }
}

async fn until(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..150 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("never happened: {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_goes_up_sealed_and_comes_down_on_another_device() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let Some(server) = common::serve(&[("DRUSITE__AUTH__PASSWORD__ENABLED", "true")]).await else {
        return;
    };
    let alice = account(&server.state, "alice@example.com").await;
    let mac = Device::new(&server.state, alice, "read write").await;
    let picture = b"\x89PNG not really a picture".to_vec();
    let Response::Attachment { kind } = mac
        .handle(Command::AddAttachment {
            name: "diagram.png".into(),
            mime: "image/png".into(),
            data: STANDARD.encode(&picture),
        })
        .unwrap()
    else {
        panic!()
    };
    let drusite_model::BlockKind::Attachment { blob, .. } = kind.clone() else {
        panic!()
    };
    let Response::PageCreated { page } = mac
        .handle(Command::CreatePage {
            parent: None,
            title: "Plans".into(),
        })
        .unwrap()
    else {
        panic!()
    };
    mac.handle(Command::OpenPage { page: page.id }).unwrap();
    mac.handle(Command::Edit {
        page: page.id,
        edits: vec![Edit::InsertBlock { after: None, kind }],
    })
    .unwrap();
    mac.handle(Command::ClosePage { page: page.id }).unwrap();
    mac.connect(&server.url, None);

    // It reaches the server, found by a keyed hash rather than its own.
    let admin = server.db.admin.pool().clone();
    for _ in 0..150 {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM attachments")
            .fetch_one(&admin)
            .await
            .unwrap();
        if n == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (lookup, address, size): (String, String, i64) =
        sqlx::query_as("SELECT lookup, address, size FROM attachments")
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(size, picture.len() as i64);
    assert_ne!(lookup, blob);
    assert_ne!(address, blob);
    until("the mac's copy is marked as on the server", || {
        mac.core
            .lock()
            .unwrap()
            .attachments_to_upload()
            .unwrap()
            .is_empty()
    })
    .await;

    // Another device of the account asks for it, and it comes.
    let ws = mac.workspace();
    let phone = Device::new(&server.state, alice, "read write").await;
    phone.connect(&server.url, Some(ws));
    until("the phone has the page", || {
        matches!(phone.handle(Command::ListPages), Ok(Response::Pages { pages }) if pages.iter().any(|p| p.id == page.id))
    })
    .await;
    let first = phone.handle(Command::ReadAttachment { blob: blob.clone() });
    if let Err(e) = &first {
        assert_eq!(e.code, ErrorCode::NotHere);
    }
    until("the phone has the file", || {
        matches!(phone.handle(Command::ReadAttachment { blob: blob.clone() }), Ok(Response::AttachmentData { data }) if STANDARD.decode(&data).unwrap() == picture)
    })
    .await;

    // Someone else can't fetch it, or add to the workspace.
    let bob = account(&server.state, "bob@example.com").await;
    let other = Device::new(&server.state, bob, "read write").await;
    assert_eq!(
        other.request(&server.url, "GET", ws, &blob, None).await,
        404
    );
    assert_eq!(
        other
            .request(
                &server.url,
                "PUT",
                ws,
                &blake3::hash(b"x").to_hex(),
                Some(b"x".to_vec())
            )
            .await,
        404
    );
    // A file must match its ID, and a read-only sign-in can't add one.
    assert_eq!(
        mac.request(
            &server.url,
            "PUT",
            ws,
            &blob,
            Some(b"something else".to_vec())
        )
        .await,
        400
    );
    let reader = Device::new(&server.state, alice, "read").await;
    assert_eq!(
        reader
            .request(
                &server.url,
                "PUT",
                ws,
                &blake3::hash(b"y").to_hex(),
                Some(b"y".to_vec())
            )
            .await,
        403
    );
    assert_eq!(
        reader.request(&server.url, "GET", ws, &blob, None).await,
        200
    );
}
