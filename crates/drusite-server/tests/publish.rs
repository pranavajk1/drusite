// SPDX-License-Identifier: AGPL-3.0-or-later

//! Publish to web, end to end: a device writes pages and syncs, its person publishes one, and
//! a plain HTTP client (as a browser would) reads it at the public link. Every option, the
//! access rules (expiry, password, subpages, unpublishing, a new link, the trash), and the
//! public host serving nothing but published pages. Synthetic content only.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use drusite_core::api::{Command, Edit, Response};
use drusite_core::{Core, StoreKey};
use drusite_dpop::SoftwareKey;
use drusite_model::{BlockId, BlockKind, HeadingLevel, Mark, PageId, UserId, WorkspaceId};
use drusite_server::app::AppState;
use drusite_server::auth::session::Method;
use drusite_server::auth::tokens::{self, NewGrant};
use serde_json::{Value, json};
use url::Url;

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn now_ms() -> i64 {
    now() * 1000
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

    fn page(&self, parent: Option<PageId>, title: &str) -> PageId {
        let Response::PageCreated { page } = self.handle(Command::CreatePage {
            parent,
            title: title.into(),
        }) else {
            panic!()
        };
        page.id
    }

    /// Writes blocks onto an empty page: kind, indent, text, link.
    fn write(&self, page: PageId, blocks: &[(BlockKind, u8, &str, Option<String>)]) {
        let Response::Opened { view } = self.handle(Command::OpenPage { page }) else {
            panic!()
        };
        let mut after: Option<BlockId> = None;
        let first = view.blocks[0].id;
        for (i, (kind, indent, text, link)) in blocks.iter().enumerate() {
            let id = if i == 0 {
                self.handle(Command::Edit {
                    page,
                    edits: vec![Edit::SetKind {
                        block: first,
                        kind: kind.clone(),
                    }],
                });
                first
            } else {
                let Response::Edited { outcome } = self.handle(Command::Edit {
                    page,
                    edits: vec![Edit::InsertBlock {
                        after,
                        kind: kind.clone(),
                    }],
                }) else {
                    panic!()
                };
                outcome.created[0]
            };
            let mut edits = vec![];
            if !text.is_empty() {
                edits.push(Edit::InsertText {
                    block: id,
                    pos: 0,
                    text: (*text).to_owned(),
                });
            }
            if *indent > 0 {
                edits.push(Edit::SetIndent {
                    block: id,
                    indent: *indent,
                });
            }
            if let Some(href) = link {
                edits.push(Edit::Format {
                    block: id,
                    start: 0,
                    end: u32::try_from(text.encode_utf16().count()).unwrap(),
                    mark: Mark::Link { href: href.clone() },
                    on: true,
                });
            }
            if !edits.is_empty() {
                self.handle(Command::Edit { page, edits });
            }
            after = Some(id);
        }
        self.handle(Command::ClosePage { page });
    }

    /// The share API, as this device's sign-in.
    async fn api(
        &self,
        server: &Url,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let url = server.join(path).unwrap();
        let proof =
            drusite_core::auth::proof(&self.key, method, &url, Some(&self.access), now()).unwrap();
        let client = reqwest::Client::new();
        let mut request = client
            .request(method.parse().unwrap(), url)
            .header("authorization", format!("DPoP {}", self.access))
            .header("dpop", proof);
        if let Some(b) = body {
            request = request.json(&b);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

async fn until(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("never happened: {what}");
}

/// A reader: a plain HTTP client, as a browser with no JavaScript would be.
fn reader() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

struct Page {
    status: u16,
    headers: reqwest::header::HeaderMap,
    body: String,
}

async fn fetch(client: &reqwest::Client, url: &str, cookie: Option<&str>) -> Page {
    let mut r = client.get(url);
    if let Some(c) = cookie {
        r = r.header("cookie", c);
    }
    let response = r.send().await.unwrap();
    Page {
        status: response.status().as_u16(),
        headers: response.headers().clone(),
        body: response.text().await.unwrap(),
    }
}

fn header<'a>(page: &'a Page, name: &str) -> &'a str {
    page.headers.get(name).map_or("", |v| v.to_str().unwrap())
}

/// The one 404, for comparing: status, body and headers are the same for every dead link.
async fn assert_dead(client: &reqwest::Client, url: &str, dead: &Page) {
    let page = fetch(client, url, None).await;
    assert_eq!(page.status, 404, "{url}: {}", page.body);
    assert_eq!(page.body, dead.body, "{url}");
    let mut a: Vec<_> = page
        .headers
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "date" | "content-length" | "x-request-id"))
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
        .collect();
    let mut b: Vec<_> = dead
        .headers
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "date" | "content-length" | "x-request-id"))
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
        .collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "{url}");
}

/// An href from the page, made absolute.
fn hrefs(body: &str) -> Vec<String> {
    body.split("href=\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap().replace("&amp;", "&"))
        .collect()
}

struct World {
    server: common::Server,
    mac: Device,
    ws: WorkspaceId,
    shared: PageId,
    child: PageId,
    grandchild: PageId,
    private: PageId,
    blob: String,
}

/// A workspace with a page to share, a page inside it (with a picture) and one inside that,
/// and a private page beside it, all on the server.
async fn world(overrides: &[(&str, &str)]) -> Option<World> {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let mut all = vec![("DRUSITE__AUTH__PASSWORD__ENABLED", "true")];
    all.extend_from_slice(overrides);
    let server = common::serve(&all).await?;
    let alice = account(&server.state, "alice@example.com").await;
    let mac = Device::new(&server.state, alice, "read write").await;
    let shared = mac.page(None, "Trip <plan> & notes");
    let child = mac.page(Some(shared), "Packing");
    let grandchild = mac.page(Some(child), "Snacks");
    let private = mac.page(None, "Diary");
    let picture = b"\x89PNG synthetic picture bytes".to_vec();
    let Response::Attachment { kind } = mac.handle(Command::AddAttachment {
        name: "map.png".into(),
        mime: "image/png".into(),
        data: STANDARD.encode(&picture),
    }) else {
        panic!()
    };
    let BlockKind::Attachment { blob, .. } = kind.clone() else {
        panic!()
    };
    mac.write(
        shared,
        &[
            (
                BlockKind::Heading {
                    level: HeadingLevel::H1,
                },
                0,
                "Day one",
                None,
            ),
            (BlockKind::Paragraph, 0, "<script>alert('x')</script>", None),
            (BlockKind::Todo { checked: true }, 0, "Book the train", None),
            (
                BlockKind::Code {
                    language: "rust".into(),
                },
                0,
                "fn main() {}",
                None,
            ),
            (BlockKind::Toggle, 0, "Details", None),
            (BlockKind::Bulleted, 1, "inside the toggle", None),
            (
                BlockKind::Paragraph,
                0,
                "see packing",
                Some(format!("drusite:page/{child}")),
            ),
            (
                BlockKind::Paragraph,
                0,
                "see diary",
                Some(format!("drusite:page/{private}")),
            ),
            (
                BlockKind::Paragraph,
                0,
                "bad link",
                Some("javascript:alert(1)".into()),
            ),
        ],
    );
    mac.write(
        child,
        &[
            (BlockKind::Paragraph, 0, "Socks and a map", None),
            (kind, 0, "The route", None),
        ],
    );
    mac.write(grandchild, &[(BlockKind::Paragraph, 0, "Apples", None)]);
    mac.write(
        private,
        &[(BlockKind::Paragraph, 0, "secret diary words", None)],
    );
    mac.connect(&server.url, None);
    let ws = mac.workspace();
    let admin = server.db.admin.pool().clone();
    for _ in 0..200 {
        let pages: i64 = sqlx::query_scalar("SELECT count(*) FROM documents WHERE kind = 'page'")
            .fetch_one(&admin)
            .await
            .unwrap();
        let files: i64 = sqlx::query_scalar("SELECT count(*) FROM attachments")
            .fetch_one(&admin)
            .await
            .unwrap();
        if pages >= 4 && files == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Some(World {
        server,
        mac,
        ws,
        shared,
        child,
        grandchild,
        private,
        blob,
    })
}

impl World {
    fn share_path(&self, page: PageId) -> String {
        format!("api/v1/workspaces/{}/pages/{page}/share", self.ws)
    }

    /// Publishes (or changes) a page, waiting for the server to have all of it.
    async fn publish(&self, page: PageId, change: Value) -> Value {
        for _ in 0..100 {
            let (status, body) = self
                .mac
                .api(
                    &self.server.url,
                    "PUT",
                    &self.share_path(page),
                    Some(change.clone()),
                )
                .await;
            if status == 200 {
                return body;
            }
            assert_eq!(body["error"], "not_synced", "{status} {body}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the page never reached the server");
    }

    fn dead_link(&self, url: &str) -> String {
        // Another token of the right shape that nobody has.
        let (base, _) = url.rsplit_once("/p/").unwrap();
        format!("{base}/p/{}", "A".repeat(43))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_page_is_read_in_a_browser_with_every_option() {
    let Some(w) = world(&[]).await else { return };
    let client = reader();

    // Off by default: no subpages, no search engines, no expiry, no password.
    let share = w.publish(w.shared, json!({})).await;
    assert_eq!(share["published"], true);
    assert_eq!(share["includeSubpages"], false);
    assert_eq!(share["allowIndexing"], false);
    assert_eq!(share["hasPassword"], false);
    assert_eq!(share["canPublish"], true);
    let url = share["url"].as_str().unwrap().to_owned();
    let token = url.rsplit('/').next().unwrap().to_owned();
    assert_eq!(token.len(), 43);
    assert!(
        !url.contains(&w.shared.to_string()),
        "the link isn't the page's ID"
    );
    assert!(
        url.starts_with(w.server.url.as_str()),
        "no public_url: the server's own address"
    );

    let dead = fetch(&client, &w.dead_link(&url), None).await;
    assert_eq!(dead.status, 404);

    let page = fetch(&client, &url, None).await;
    assert_eq!(page.status, 200, "{}", page.body);
    let b = &page.body;
    assert!(
        b.contains("<h1 class=\"title\">Trip &lt;plan&gt; &amp; notes</h1>"),
        "{b}"
    );
    assert!(b.contains("<h2>Day one</h2>"));
    assert!(b.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"));
    assert!(!b.contains("<script"));
    assert!(b.contains("<input type=\"checkbox\" disabled checked> <span>Book the train</span>"));
    assert!(b.contains("<span class=\"tok-k\">fn</span>"));
    assert!(b.contains("<details><summary>Details</summary>"));
    assert!(b.contains("<li>inside the toggle</li>"));
    assert!(
        b.contains("<p>see packing</p>"),
        "an unshared subpage is plain text: {b}"
    );
    assert!(b.contains("<p>see diary</p>"));
    assert!(b.contains("<p>bad link</p>"));
    assert!(!b.contains("secret diary"));
    assert!(b.contains("og:title\" content=\"Trip &lt;plan&gt; &amp; notes\""));
    for id in [w.shared, w.child, w.grandchild, w.private] {
        assert!(!b.contains(&id.to_string()), "no page ID in the HTML");
    }
    assert!(!b.contains(w.ws.to_string().as_str()));
    assert!(header(&page, "content-security-policy").starts_with("default-src 'none'"));
    assert_eq!(
        header(&page, "x-robots-tag"),
        "noindex, nofollow, noarchive"
    );
    assert!(b.contains("<meta name=\"robots\" content=\"noindex"));
    assert_eq!(header(&page, "referrer-policy"), "no-referrer");
    assert_eq!(header(&page, "x-content-type-options"), "nosniff");
    assert_eq!(
        header(&page, "cache-control"),
        "private, max-age=15, must-revalidate"
    );
    let etag = header(&page, "etag").to_owned();
    assert!(etag.starts_with('"'));
    let again = client
        .get(&url)
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 304);

    // The stylesheet comes from the same host.
    let css = hrefs(b)
        .into_iter()
        .find(|h| h.starts_with("/assets/"))
        .unwrap();
    let base = w.server.url.as_str().trim_end_matches('/');
    let sheet = fetch(&client, &format!("{base}{css}"), None).await;
    assert_eq!(sheet.status, 200);
    assert!(header(&sheet, "content-type").starts_with("text/css"));
    assert!(sheet.body.contains("prefers-color-scheme: dark"));
    assert_dead(&client, &format!("{base}/assets/other.css"), &dead).await;

    // Live: an edit on the device shows on the web.
    let Response::Opened { view } = w.mac.handle(Command::OpenPage { page: w.shared }) else {
        panic!()
    };
    w.mac.handle(Command::Edit {
        page: w.shared,
        edits: vec![Edit::InsertText {
            block: view.blocks[0].id,
            pos: 0,
            text: "Fresh ".into(),
        }],
    });
    w.mac.handle(Command::ClosePage { page: w.shared });
    let mut live = false;
    for _ in 0..100 {
        if fetch(&client, &url, None)
            .await
            .body
            .contains("Fresh Day one")
        {
            live = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(live, "the edit never showed");

    // The share state reached the device through the workspace document.
    until("the device shows the page as published", || {
        let Response::Pages { pages } = w.mac.handle(Command::ListPages) else {
            panic!()
        };
        pages.iter().any(|p| p.id == w.shared && p.published)
    })
    .await;
    let Response::Share { share: mirrored } = w.mac.handle(Command::LocalShare { page: w.shared })
    else {
        panic!()
    };
    assert_eq!(mirrored.url.as_deref(), Some(url.as_str()));

    // Subpages: a small site. The private page stays out.
    let share = w
        .publish(w.shared, json!({ "includeSubpages": true }))
        .await;
    assert_eq!(
        share["url"],
        url.as_str(),
        "changing an option keeps the link"
    );
    let page = fetch(&client, &url, None).await;
    assert!(page.body.contains("<nav class=\"subpages\""));
    let links = hrefs(&page.body);
    let child_href = links
        .iter()
        .find(|h| h.starts_with(&format!("/p/{token}/")) && !h.contains("/file/"))
        .unwrap()
        .clone();
    assert!(page.body.contains(&format!(
        "<a href=\"{child_href}\" class=\"page-link\">see packing</a>"
    )));
    assert!(
        page.body.contains("<p>see diary</p>"),
        "the private page is still plain text"
    );
    let child = fetch(&client, &format!("{base}{child_href}"), None).await;
    assert_eq!(child.status, 200);
    assert!(child.body.contains("Socks and a map"));
    assert!(child.body.contains(&format!(
        "<li><a href=\"/p/{token}\">Trip &lt;plan&gt; &amp; notes</a></li>"
    )));
    let img = hrefs(&child.body.replace("src=\"", "href=\""))
        .into_iter()
        .find(|h| h.contains("/file/"))
        .unwrap();
    assert!(!img.contains(&w.blob), "files are named by keyed hashes");
    let file = client.get(format!("{base}{img}")).send().await.unwrap();
    assert_eq!(file.status(), 200);
    assert_eq!(file.headers()["content-type"], "image/png");
    assert!(
        file.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("sandbox")
    );
    assert_eq!(
        file.bytes().await.unwrap().as_ref(),
        b"\x89PNG synthetic picture bytes"
    );
    let grandchild_href = hrefs(&child.body)
        .into_iter()
        .find(|h| {
            h.starts_with(&format!("/p/{token}/")) && *h != child_href && !h.contains("/file/")
        })
        .unwrap();
    let grandchild = fetch(&client, &format!("{base}{grandchild_href}"), None).await;
    assert!(grandchild.body.contains("Apples"));
    // Made-up subpage and file names are the same 404.
    assert_dead(&client, &format!("{url}/AAAAAAAAAAAAAAAA"), &dead).await;
    assert_dead(
        &client,
        &format!("{base}{child_href}/file/AAAAAAAAAAAAAAAA"),
        &dead,
    )
    .await;
    assert_dead(&client, &format!("{url}/{}", w.private), &dead).await;

    // The trash: a subpage in it is gone from the web, and back when restored.
    w.mac.handle(Command::DeletePage { page: w.child });
    let mut gone = false;
    for _ in 0..100 {
        if fetch(&client, &format!("{base}{child_href}"), None)
            .await
            .status
            == 404
        {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone);
    assert_dead(&client, &format!("{base}{child_href}"), &dead).await;
    assert_dead(&client, &format!("{base}{grandchild_href}"), &dead).await;
    assert_dead(&client, &format!("{base}{img}"), &dead).await;
    w.mac.handle(Command::RestorePage { page: w.child });
    let mut back = false;
    for _ in 0..100 {
        if fetch(&client, &format!("{base}{child_href}"), None)
            .await
            .status
            == 200
        {
            back = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(back);

    // Turning subpages off again takes them down.
    w.publish(w.shared, json!({ "includeSubpages": false }))
        .await;
    assert_dead(&client, &format!("{base}{child_href}"), &dead).await;

    // Search engines: no noindex, a canonical link, and robots.txt lets them crawl.
    w.publish(w.shared, json!({ "allowIndexing": true })).await;
    let page = fetch(&client, &url, None).await;
    assert_eq!(header(&page, "x-robots-tag"), "all");
    assert!(!page.body.contains("noindex"));
    assert!(
        page.body
            .contains(&format!("<link rel=\"canonical\" href=\"{url}\">"))
    );
    let robots = fetch(&client, &format!("{base}/robots.txt"), None).await;
    assert!(robots.body.contains("Allow: /p/"));
    assert!(
        !robots.body.contains(&token),
        "robots.txt never lists links"
    );

    // An expiry date: in the past is refused; once it passes, the link is dead.
    let (status, body) = w
        .mac
        .api(
            &w.server.url,
            "PUT",
            &w.share_path(w.shared),
            Some(json!({ "expiresAt": now_ms() - 1000 })),
        )
        .await;
    assert_eq!((status, body["error"].as_str()), (400, Some("bad_expiry")));
    let share = w
        .publish(w.shared, json!({ "expiresAt": now_ms() + 1500 }))
        .await;
    assert_eq!(share["expired"], false);
    assert_eq!(fetch(&client, &url, None).await.status, 200);
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_dead(&client, &url, &dead).await;
    let (_, share) = w
        .mac
        .api(&w.server.url, "GET", &w.share_path(w.shared), None)
        .await;
    assert_eq!(share["expired"], true);
    w.publish(w.shared, json!({ "expiresAt": null })).await;
    assert_eq!(fetch(&client, &url, None).await.status, 200);

    // A password: a form with nothing of the page, a signed cookie, never in the URL.
    let (status, body) = w
        .mac
        .api(
            &w.server.url,
            "PUT",
            &w.share_path(w.shared),
            Some(json!({ "password": "ab" })),
        )
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("bad_password"))
    );
    w.publish(
        w.shared,
        json!({ "password": "correct horse", "includeSubpages": true }),
    )
    .await;
    let gate = fetch(&client, &url, None).await;
    assert_eq!(gate.status, 200);
    assert!(gate.body.contains("This page needs a password"));
    assert!(
        !gate.body.contains("Trip"),
        "the form shows nothing of the page"
    );
    assert!(!gate.body.contains("Day one"));
    assert!(gate.body.contains("noindex"));
    assert_eq!(header(&gate, "cache-control"), "no-store");
    assert_eq!(
        client
            .get(format!("{base}{img}"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let wrong = client
        .post(format!("{url}/unlock"))
        .form(&[("password", "wrong")])
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 403);
    assert!(
        wrong
            .text()
            .await
            .unwrap()
            .contains("That isn't the password.")
    );
    let right = client
        .post(format!("{url}/unlock"))
        .form(&[("password", "correct horse")])
        .send()
        .await
        .unwrap();
    assert_eq!(right.status(), 303);
    assert_eq!(right.headers()["location"], format!("/p/{token}"));
    let set = right.headers()["set-cookie"].to_str().unwrap().to_owned();
    assert!(
        set.contains("HttpOnly")
            && set.contains("SameSite=Lax")
            && set.contains(&format!("Path=/p/{token}"))
    );
    let cookie = set.split(';').next().unwrap().to_owned();
    let page = fetch(&client, &url, Some(&cookie)).await;
    assert!(page.body.contains("Fresh Day one"));
    assert_eq!(header(&page, "cache-control"), "private, no-cache");
    assert!(
        page.body.contains("noindex"),
        "a page with a password is never indexed"
    );
    let file = client
        .get(format!("{base}{img}"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(file.status(), 200);
    let forged = format!("{}.{}", cookie.split_once('.').unwrap().0, "A".repeat(43));
    assert!(
        fetch(&client, &url, Some(&forged))
            .await
            .body
            .contains("needs a password")
    );
    // A new password ends the old cookies.
    w.publish(w.shared, json!({ "password": "battery staple" }))
        .await;
    assert!(
        fetch(&client, &url, Some(&cookie))
            .await
            .body
            .contains("needs a password")
    );
    // Too many wrong tries: the link refuses more for a while, shared by every replica.
    sqlx::query("UPDATE published_pages SET failed_unlocks = 20, unlock_window_start = now()")
        .execute(w.server.db.admin.pool())
        .await
        .unwrap();
    let limited = client
        .post(format!("{url}/unlock"))
        .form(&[("password", "battery staple")])
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), 429);
    sqlx::query("UPDATE published_pages SET unlock_window_start = now() - interval '1 hour'")
        .execute(w.server.db.admin.pool())
        .await
        .unwrap();
    let right = client
        .post(format!("{url}/unlock"))
        .form(&[("password", "battery staple")])
        .send()
        .await
        .unwrap();
    assert_eq!(right.status(), 303);
    let cookie = right.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    w.publish(w.shared, json!({ "password": null })).await;
    assert!(
        fetch(&client, &url, None)
            .await
            .body
            .contains("Fresh Day one")
    );

    // A new link: the old one is dead at once, and so are cookies for it.
    let (status, share) = w
        .mac
        .api(
            &w.server.url,
            "POST",
            &format!("{}/regenerate", w.share_path(w.shared)),
            None,
        )
        .await;
    assert_eq!(status, 200);
    let new_url = share["url"].as_str().unwrap().to_owned();
    assert_ne!(new_url, url);
    assert_dead(&client, &url, &dead).await;
    assert_dead(&client, &format!("{base}{child_href}"), &dead).await;
    assert_eq!(fetch(&client, &new_url, Some(&cookie)).await.status, 200);

    // Unpublishing takes it down at once, and the devices hear.
    let (status, share) = w
        .mac
        .api(&w.server.url, "DELETE", &w.share_path(w.shared), None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(share["published"], false);
    assert_dead(&client, &new_url, &dead).await;
    until("the device shows the page as unpublished", || {
        let Response::Pages { pages } = w.mac.handle(Command::ListPages) else {
            panic!()
        };
        pages.iter().any(|p| p.id == w.shared && !p.published)
    })
    .await;

    // A page in the trash can't be published, and a published one that goes there is dead.
    let share = w.publish(w.shared, json!({})).await;
    let url = share["url"].as_str().unwrap().to_owned();
    w.mac.handle(Command::DeletePage { page: w.shared });
    let mut gone = false;
    for _ in 0..100 {
        if fetch(&client, &url, None).await.status == 404 {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone);
    assert_dead(&client, &url, &dead).await;
    let mut refused = false;
    for _ in 0..100 {
        let (status, body) = w
            .mac
            .api(
                &w.server.url,
                "PUT",
                &w.share_path(w.shared),
                Some(json!({})),
            )
            .await;
        if status == 400 && body["error"] == "in_trash" {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(refused);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_editors_publish_and_strangers_see_nothing() {
    let Some(w) = world(&[]).await else { return };
    let share = w.publish(w.shared, json!({})).await;
    let url = share["url"].as_str().unwrap().to_owned();

    // A viewer of the workspace sees the share state, but can't change it.
    let bob = account(&w.server.state, "bob@example.com").await;
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'viewer')",
    )
    .bind(w.ws.as_uuid())
    .bind(bob.as_uuid())
    .execute(w.server.db.admin.pool())
    .await
    .unwrap();
    let viewer = Device::new(&w.server.state, bob, "read write").await;
    let (status, body) = viewer
        .api(&w.server.url, "GET", &w.share_path(w.shared), None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["url"], url.as_str());
    assert_eq!(body["canPublish"], false);
    for (method, path) in [
        ("PUT", w.share_path(w.shared)),
        ("DELETE", w.share_path(w.shared)),
        ("POST", format!("{}/regenerate", w.share_path(w.shared))),
    ] {
        let (status, _) = viewer
            .api(&w.server.url, method, &path, Some(json!({})))
            .await;
        assert_eq!(status, 403, "{method} {path}");
    }

    // Someone outside it: as if there were nothing there.
    let eve = account(&w.server.state, "eve@example.com").await;
    let stranger = Device::new(&w.server.state, eve, "read write").await;
    let (status, _) = stranger
        .api(&w.server.url, "GET", &w.share_path(w.shared), None)
        .await;
    assert_eq!(status, 404);
    let (status, _) = stranger
        .api(
            &w.server.url,
            "PUT",
            &w.share_path(w.private),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, 404);
    let (status, _) = stranger
        .api(
            &w.server.url,
            "GET",
            &format!("api/v1/workspaces/{}/shares", w.ws),
            None,
        )
        .await;
    assert_eq!(status, 404);

    // A read-only sign-in of the owner can't publish either.
    let alice: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'alice@example.com'")
            .fetch_one(w.server.db.admin.pool())
            .await
            .unwrap();
    let reading = Device::new(&w.server.state, UserId::from_uuid(alice), "read").await;
    let (status, body) = reading
        .api(
            &w.server.url,
            "PUT",
            &w.share_path(w.private),
            Some(json!({})),
        )
        .await;
    assert_eq!((status, body["error"].as_str()), (403, Some("read_only")));

    // The list.
    let (status, list) = w
        .mac
        .api(
            &w.server.url,
            "GET",
            &format!("api/v1/workspaces/{}/shares", w.ws),
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(list.as_array().unwrap().len(), 1);

    // An agent publishes only when its person allowed it (Cedar's publish_document).
    use drusite_server::policy::AgentRights;
    use drusite_server::publish::{Actor, ShareChange};
    let agent = |publish| Actor {
        user: UserId::from_uuid(alice),
        agent: Some(AgentRights { publish }),
    };
    let refused = drusite_server::publish::publish(
        &w.server.state,
        agent(false),
        *w.ws.as_uuid(),
        *w.private.as_uuid(),
        ShareChange::default(),
    )
    .await;
    assert!(matches!(
        refused,
        Err(drusite_server::auth::AuthError::Forbidden(..))
    ));
    let allowed = drusite_server::publish::publish(
        &w.server.state,
        agent(true),
        *w.ws.as_uuid(),
        *w.private.as_uuid(),
        ShareChange::default(),
    )
    .await
    .unwrap();
    assert!(allowed.published);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_public_host_serves_published_pages_and_nothing_else() {
    // The public host's name points at the same port; a deployment would put a proxy (or the
    // separate listener, below) in front of only that.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_port = listener.local_addr().unwrap().port();
    let public_url = format!("http://pages.localhost:{public_port}/");
    let Some(w) = world(&[
        ("DRUSITE__PUBLISH__PUBLIC_URL", public_url.as_str()),
        ("DRUSITE__AGENTS__ENABLED", "true"),
    ])
    .await
    else {
        return;
    };
    let share = w
        .publish(w.shared, json!({ "includeSubpages": true }))
        .await;
    let url = share["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with(&format!("{public_url}p/")), "{url}");
    assert_eq!(share["publicBase"], public_url.as_str());
    let token = url.rsplit('/').next().unwrap().to_owned();

    // Its own listener: the public router and nothing more, as `[publish] listen` serves it.
    let app = drusite_server::publish::public_router(w.server.state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve("pages.localhost", ([127, 0, 0, 1], public_port).into())
        .build()
        .unwrap();
    let page = fetch(&client, &url, None).await;
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("Day one"));
    let dead = fetch(&client, &format!("{public_url}p/{}", "A".repeat(43)), None).await;
    assert_eq!(dead.status, 404);

    // Everything the app serves is the same 404 here, whatever the method.
    let app_paths = [
        "",
        "health",
        "health/ready",
        "metrics",
        "signin",
        "account",
        "login",
        "oauth/authorize",
        "oauth/token",
        ".well-known/openid-configuration",
        ".well-known/oauth-authorization-server",
        ".well-known/oauth-protected-resource",
        "mcp",
        "sync",
        "api/v1/ask",
        &format!("api/v1/workspaces/{}/shares", w.ws),
        &format!("api/v1/workspaces/{}/pages/{}/share", w.ws, w.shared),
        &format!("api/v1/workspaces/{}/attachments/{}", w.ws, w.blob),
        "p",
        "p/",
        &format!("p/{token}/"),
        "assets/signin.css",
        "favicon.ico",
    ];
    for path in app_paths {
        assert_dead(&client, &format!("{public_url}{path}"), &dead).await;
        for method in ["POST", "PUT", "DELETE"] {
            let r = client
                .request(method.parse().unwrap(), format!("{public_url}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 404, "{method} /{path}");
            assert_eq!(r.text().await.unwrap(), dead.body, "{method} /{path}");
        }
    }
    let r = client.put(&url).send().await.unwrap();
    assert_eq!(r.status(), 404, "a page with the wrong method");

    // The same name on the main listener gets the same: only published pages.
    let main_port = w.server.url.port().unwrap();
    let via_main = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve("pages.localhost", ([127, 0, 0, 1], main_port).into())
        .build()
        .unwrap();
    let main_public = format!("http://pages.localhost:{main_port}/");
    let page = fetch(&via_main, &format!("{main_public}p/{token}"), None).await;
    assert_eq!(page.status, 200);
    for path in app_paths {
        let r = fetch(&via_main, &format!("{main_public}{path}"), None).await;
        assert_eq!(
            r.status, 404,
            "/{path} on the main port, by the public name"
        );
        assert_eq!(r.body, dead.body, "/{path}");
    }

    // And the app's own address no longer serves published pages.
    let app_page = fetch(&reader(), &format!("{}p/{token}", w.server.url), None).await;
    assert_eq!(app_page.status, 404);
    assert_ne!(app_page.body, page.body);
    let robots = fetch(&reader(), &format!("{}robots.txt", w.server.url), None).await;
    assert_eq!(robots.body, "User-agent: *\nDisallow: /\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn readers_are_rate_limited() {
    let Some(w) = world(&[("DRUSITE__PUBLISH__PER_MINUTE", "5")]).await else {
        return;
    };
    let share = w.publish(w.shared, json!({})).await;
    let url = share["url"].as_str().unwrap().to_owned();
    let client = reader();
    for _ in 0..5 {
        assert_eq!(fetch(&client, &url, None).await.status, 200);
    }
    let over = fetch(&client, &url, None).await;
    assert_eq!(over.status, 429);
    assert_eq!(header(&over, "retry-after"), "60");
    // The app itself isn't limited by it.
    let (status, _) = w
        .mac
        .api(&w.server.url, "GET", &w.share_path(w.shared), None)
        .await;
    assert_eq!(status, 200);
}
