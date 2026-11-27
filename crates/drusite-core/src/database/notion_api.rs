// SPDX-License-Identifier: AGPL-3.0-or-later

//! Importing straight from Notion's API, with an internal integration's token.
//!
//! The API keeps what an export loses: every property's type and options, formulas (with
//! their expressions), rollups, two-way relations, Notion's IDs, who made and edited each row
//! and when. It does it in two steps: [`fetch`] reads everything shared with the integration
//! (network only, no lock on the core), then [`Core::import_notion_fetched`] makes the pages.
//!
//! The token: kept in the Keychain on Apple devices, and on others in the encrypted store, whose
//! key the operating system's key store holds. It is sent to Notion only, and never logged or
//! put in an error.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use drusite_model::database::{
    Color, DateTime, DateValue, FileRef, Grouping, Layout, NumberFormat, Person, PropertyDef,
    PropertyKind, RollupFunction, SelectOption, StatusGroup, TITLE, Value, View, ViewProperty,
    short_id,
};
use drusite_model::{BlockKind, HeadingLevel, Marks, PageId, Span};
use serde_json::Value as J;

use super::notion::{NotionReport, id_in};
use crate::api::{ApiError, ErrorCode};
use crate::attachments::{blob_id, guess_mime};
use crate::bridge::PAGE_LINK;
use crate::engine::Core;
use drusite_crdt::merge::{self, Wanted};

/// Notion's API.
pub const API: &str = "https://api.notion.com/v1";
/// The API version Drusite speaks.
pub const VERSION: &str = "2022-06-28";
/// The largest file downloaded from Notion.
const MAX_FILE: u64 = 100 * 1024 * 1024;
/// How deep nested blocks are followed.
const MAX_DEPTH: u8 = 8;

/// A block as fetched: Drusite's kind, or a reference to a page or database resolved later.
#[derive(Debug, Clone)]
pub enum FetchedKind {
    Block(BlockKind),
    ChildDatabase(String),
}

#[derive(Debug, Clone)]
pub struct FetchedBlock {
    pub kind: FetchedKind,
    pub indent: u8,
    /// Links to Notion pages are `notion:<id>` until they are resolved.
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone)]
pub struct FetchedPage {
    pub id: String,
    pub title: String,
    /// The Notion page or database it is under; none at the workspace's top.
    pub parent: Option<String>,
    pub blocks: Vec<FetchedBlock>,
    /// A row's properties, as the API gave them.
    pub properties: Option<J>,
    pub created: Option<i64>,
    pub edited: Option<i64>,
    pub created_by: Option<Person>,
    pub edited_by: Option<Person>,
}

#[derive(Debug, Clone)]
pub struct FetchedDatabase {
    pub id: String,
    pub title: String,
    pub parent: Option<String>,
    pub inline: bool,
    /// The schema, as the API gave it.
    pub properties: J,
    pub rows: Vec<FetchedPage>,
}

/// Everything shared with the integration.
#[derive(Debug, Default)]
pub struct Fetched {
    pub pages: Vec<FetchedPage>,
    pub databases: Vec<FetchedDatabase>,
    /// Files the pages hold: blob → (name, bytes).
    pub files: HashMap<String, (String, Vec<u8>)>,
    pub problems: Vec<String>,
}

fn net(m: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::Account, m)
}

/// A Notion API client. `base` is [`API`] except in tests.
pub struct Client {
    agent: ureq::Agent,
    base: String,
    token: zeroize::Zeroizing<String>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(base: &str, token: &str) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(60)))
                .http_status_as_error(false)
                .build()
                .into(),
            base: base.trim_end_matches('/').to_owned(),
            token: zeroize::Zeroizing::new(token.trim().to_owned()),
        }
    }

    /// One call, waiting out Notion's rate limit (three a second) when it says so.
    fn call(&self, method: &str, path: &str, body: Option<&J>) -> Result<J, ApiError> {
        let url = format!("{}{path}", self.base);
        for attempt in 0..6 {
            let auth = format!("Bearer {}", self.token.as_str());
            let response = match (method, body) {
                ("POST", Some(b)) => self
                    .agent
                    .post(&url)
                    .header("Authorization", &auth)
                    .header("Notion-Version", VERSION)
                    .send_json(b),
                _ => self
                    .agent
                    .get(&url)
                    .header("Authorization", &auth)
                    .header("Notion-Version", VERSION)
                    .call(),
            };
            // Never the URL's token or the headers: ureq's errors name neither.
            let mut response = response.map_err(|e| net(format!("couldn't reach Notion: {e}")))?;
            let status = response.status().as_u16();
            if status == 429 || status >= 500 {
                let wait = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1 << attempt);
                std::thread::sleep(Duration::from_secs(wait.min(30)));
                continue;
            }
            let json: J = response
                .body_mut()
                .with_config()
                .limit(64 * 1024 * 1024)
                .read_json()
                .map_err(|e| net(format!("Notion's answer couldn't be read: {e}")))?;
            if status == 401 {
                return Err(net(
                    "Notion didn't accept the token (check it, or make a new one)",
                ));
            }
            if !(200..300).contains(&status) {
                let message = json["message"].as_str().unwrap_or("an error");
                return Err(net(format!("Notion answered {status}: {message}")));
            }
            return Ok(json);
        }
        Err(net("Notion kept asking to slow down; try again later"))
    }

    /// Every page of a paginated list.
    fn all(&self, method: &str, path: &str, body: J) -> Result<Vec<J>, ApiError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let res = if method == "POST" {
                let mut b = body.clone();
                b["page_size"] = J::from(100);
                if let Some(c) = &cursor {
                    b["start_cursor"] = J::from(c.clone());
                }
                self.call("POST", path, Some(&b))?
            } else {
                let sep = if path.contains('?') { '&' } else { '?' };
                let p = match &cursor {
                    Some(c) => format!("{path}{sep}page_size=100&start_cursor={c}"),
                    None => format!("{path}{sep}page_size=100"),
                };
                self.call("GET", &p, None)?
            };
            if let Some(items) = res["results"].as_array() {
                out.extend(items.iter().cloned());
            }
            match (res["has_more"].as_bool(), res["next_cursor"].as_str()) {
                (Some(true), Some(c)) => cursor = Some(c.to_owned()),
                _ => return Ok(out),
            }
        }
    }

    fn download(&self, url: &str) -> Option<Vec<u8>> {
        // Notion's file URLs are signed S3 links: no token goes with them.
        let mut response = self.agent.get(url).call().ok()?;
        if !response.status().is_success() {
            return None;
        }
        response
            .body_mut()
            .with_config()
            .limit(MAX_FILE)
            .read_to_vec()
            .ok()
    }
}

fn plain(rich: &J) -> String {
    rich.as_array()
        .map(|a| a.iter().filter_map(|t| t["plain_text"].as_str()).collect())
        .unwrap_or_default()
}

/// Notion's rich text as spans; links to Notion pages become `notion:<id>`.
fn spans(rich: &J) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for t in rich.as_array().into_iter().flatten() {
        let text = t["plain_text"].as_str().unwrap_or_default().to_owned();
        if text.is_empty() {
            continue;
        }
        let a = &t["annotations"];
        let mut marks = Marks {
            bold: a["bold"].as_bool().unwrap_or(false),
            italic: a["italic"].as_bool().unwrap_or(false),
            underline: a["underline"].as_bool().unwrap_or(false),
            strikethrough: a["strikethrough"].as_bool().unwrap_or(false),
            code: a["code"].as_bool().unwrap_or(false),
            link: None,
        };
        let mention_page = t["mention"]["page"]["id"]
            .as_str()
            .or_else(|| t["mention"]["database"]["id"].as_str());
        marks.link = match mention_page {
            Some(id) => Some(format!("notion:{}", id.replace('-', ""))),
            None => t["href"].as_str().map(|h| match notion_page_in(h) {
                Some(id) => format!("notion:{id}"),
                None => h.to_owned(),
            }),
        };
        match out.last_mut() {
            Some(last) if last.marks == marks => last.text.push_str(&text),
            _ => out.push(Span { text, marks }),
        }
    }
    out
}

/// The page a link to notion.so names (`/Title-<32 hex>`, or a bare `/<32 hex>`).
fn notion_page_in(href: &str) -> Option<String> {
    if href.starts_with('/') || href.contains("notion.so") || href.contains("notion.site") {
        id_in(href.split(['?', '#']).next().unwrap_or(href))
    } else {
        None
    }
}

fn parent_of(obj: &J) -> Option<String> {
    let p = &obj["parent"];
    p["page_id"]
        .as_str()
        .or_else(|| p["database_id"].as_str())
        .or_else(|| p["block_id"].as_str())
        .map(|s| s.replace('-', ""))
}

fn person(u: &J) -> Option<Person> {
    let id = u["id"].as_str()?.to_owned();
    let email = u["person"]["email"].as_str().map(str::to_owned);
    Some(Person {
        id: email.clone().unwrap_or(id),
        name: u["name"].as_str().unwrap_or("Someone").to_owned(),
        email,
    })
}

fn ms(s: Option<&str>) -> Option<i64> {
    let t = time::OffsetDateTime::parse(s?, &time::format_description::well_known::Rfc3339).ok()?;
    Some(i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok()?)
}

fn id_of(obj: &J) -> String {
    obj["id"].as_str().unwrap_or_default().replace('-', "")
}

fn title_of_page(obj: &J) -> String {
    obj["properties"]
        .as_object()
        .and_then(|props| props.values().find(|p| p["type"] == "title"))
        .map(|p| plain(&p["title"]))
        .unwrap_or_default()
}

impl Client {
    /// A block's children, recursively, as Drusite blocks.
    fn blocks(
        &self,
        parent: &str,
        depth: u8,
        files: &mut HashMap<String, (String, Vec<u8>)>,
        problems: &mut Vec<String>,
    ) -> Result<Vec<FetchedBlock>, ApiError> {
        let mut out = Vec::new();
        if depth > MAX_DEPTH {
            return Ok(out);
        }
        for b in self.all("GET", &format!("/blocks/{parent}/children"), J::Null)? {
            let t = b["type"].as_str().unwrap_or_default();
            let body = &b[t];
            let text = spans(&body["rich_text"]);
            let mut push = |kind: FetchedKind, spans: Vec<Span>| {
                out.push(FetchedBlock {
                    kind,
                    indent: depth,
                    spans,
                });
            };
            let block = |k| FetchedKind::Block(k);
            let mut children_flat = false;
            match t {
                "paragraph" => push(block(BlockKind::Paragraph), text),
                "heading_1" | "heading_2" | "heading_3" => push(
                    block(BlockKind::Heading {
                        level: HeadingLevel::clamped(i64::from(t.as_bytes()[8] - b'0')),
                    }),
                    text,
                ),
                "bulleted_list_item" => push(block(BlockKind::Bulleted), text),
                "numbered_list_item" => push(block(BlockKind::Numbered), text),
                "to_do" => push(
                    block(BlockKind::Todo {
                        checked: body["checked"].as_bool().unwrap_or(false),
                    }),
                    text,
                ),
                "toggle" => push(block(BlockKind::Toggle), text),
                "quote" => push(block(BlockKind::Quote), text),
                "callout" => push(
                    block(BlockKind::Callout {
                        icon: body["icon"]["emoji"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    }),
                    text,
                ),
                "code" => push(
                    block(BlockKind::Code {
                        language: body["language"].as_str().unwrap_or_default().to_owned(),
                    }),
                    vec![Span::plain(plain(&body["rich_text"]))],
                ),
                "equation" => push(
                    block(BlockKind::Code {
                        language: "latex".into(),
                    }),
                    vec![Span::plain(body["expression"].as_str().unwrap_or_default())],
                ),
                "divider" => push(block(BlockKind::Divider), Vec::new()),
                "child_database" => push(FetchedKind::ChildDatabase(id_of(&b)), Vec::new()),
                // A subpage is imported as a page of its own, under this one.
                "child_page" => {}
                "image" | "file" | "pdf" | "video" | "audio" => {
                    let url = body["file"]["url"]
                        .as_str()
                        .or_else(|| body["external"]["url"].as_str())
                        .unwrap_or_default();
                    let caption = spans(&body["caption"]);
                    let name = body["name"].as_str().map(str::to_owned).unwrap_or_else(|| {
                        url.split(['?', '#'])
                            .next()
                            .and_then(|u| u.rsplit('/').next())
                            .map(super::notion::percent_decode)
                            .unwrap_or_else(|| "file".into())
                    });
                    let stored = body["type"] == "file";
                    match stored.then(|| self.download(url)).flatten() {
                        Some(bytes) => {
                            let blob = blob_id(&bytes);
                            let kind = BlockKind::Attachment {
                                blob: blob.clone(),
                                mime: guess_mime(&name).to_owned(),
                                size: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
                                name: name.clone(),
                            };
                            files.insert(blob, (name, bytes));
                            push(block(kind), caption);
                        }
                        None => {
                            if stored {
                                problems.push(format!("{name}: couldn't download it from Notion"));
                            }
                            push(
                                block(BlockKind::Paragraph),
                                vec![Span {
                                    text: if name.is_empty() {
                                        url.to_owned()
                                    } else {
                                        name
                                    },
                                    marks: Marks {
                                        link: Some(url.to_owned()),
                                        ..Marks::default()
                                    },
                                }],
                            );
                        }
                    }
                }
                "bookmark" | "embed" | "link_preview" => {
                    let url = body["url"].as_str().unwrap_or_default().to_owned();
                    let caption = plain(&body["caption"]);
                    push(
                        block(BlockKind::Paragraph),
                        vec![Span {
                            text: if caption.is_empty() {
                                url.clone()
                            } else {
                                caption
                            },
                            marks: Marks {
                                link: Some(url),
                                ..Marks::default()
                            },
                        }],
                    );
                }
                "table" => {
                    // A table has no block of its own here: its Markdown, as the Markdown
                    // reader keeps tables.
                    let rows =
                        self.all("GET", &format!("/blocks/{}/children", id_of(&b)), J::Null)?;
                    let mut md = Vec::new();
                    for (i, r) in rows.iter().enumerate() {
                        let cells: Vec<String> = r["table_row"]["cells"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|c| plain(c).replace('|', "\\|"))
                            .collect();
                        md.push(format!("| {} |", cells.join(" | ")));
                        if i == 0 {
                            md.push(format!("|{}", " --- |".repeat(cells.len())));
                        }
                    }
                    push(
                        block(BlockKind::Code {
                            language: "table".into(),
                        }),
                        vec![Span::plain(md.join("\n"))],
                    );
                }
                "column_list" | "column" | "synced_block" | "template" => children_flat = true,
                "table_of_contents" | "breadcrumb" | "link_to_page" | "unsupported" => {
                    if let Some(id) = body["page_id"]
                        .as_str()
                        .or_else(|| body["database_id"].as_str())
                    {
                        push(
                            block(BlockKind::Paragraph),
                            vec![Span {
                                text: "Linked page".into(),
                                marks: Marks {
                                    link: Some(format!("notion:{}", id.replace('-', ""))),
                                    ..Marks::default()
                                },
                            }],
                        );
                    } else if t == "unsupported" {
                        problems.push("a block Notion's API doesn't give out was left out".into());
                    }
                }
                other => problems.push(format!("a {other} block was left out")),
            }
            if b["has_children"].as_bool() == Some(true)
                && t != "child_page"
                && t != "child_database"
                && t != "table"
            {
                let next = if children_flat { depth } else { depth + 1 };
                out.extend(self.blocks(&id_of(&b), next, files, problems)?);
            }
        }
        Ok(out)
    }
}

/// Reads everything shared with the integration. `progress` hears (done, total, what).
pub fn fetch(client: &Client, progress: &dyn Fn(u32, u32, &str)) -> Result<Fetched, ApiError> {
    let mut out = Fetched::default();
    progress(0, 0, "Finding what's shared with Drusite");
    let found = client.all("POST", "/search", serde_json::json!({}))?;
    let total = u32::try_from(found.len()).unwrap_or(u32::MAX);
    let mut row_ids: HashSet<String> = HashSet::new();
    let mut db_ids: Vec<String> = Vec::new();
    let mut pages = Vec::new();
    for obj in &found {
        match obj["object"].as_str() {
            Some("database") => db_ids.push(id_of(obj)),
            Some("page") if obj["parent"]["type"] != "database_id" => pages.push(obj.clone()),
            _ => {}
        }
    }
    let mut done = 0;
    for id in &db_ids {
        let db = client.call("GET", &format!("/databases/{id}"), None)?;
        let title = plain(&db["title"]);
        progress(done, total, &title);
        let mut rows = Vec::new();
        for r in client.all(
            "POST",
            &format!("/databases/{id}/query"),
            serde_json::json!({}),
        )? {
            let rid = id_of(&r);
            row_ids.insert(rid.clone());
            let mut props = r["properties"].clone();
            // A relation of more than 25 rows comes in pages of its own.
            if let Some(map) = props.as_object_mut() {
                for (_, p) in map.iter_mut() {
                    if p["type"] == "relation" && p["has_more"].as_bool() == Some(true) {
                        let pid = p["id"].as_str().unwrap_or_default().to_owned();
                        let items = client.all(
                            "GET",
                            &format!("/pages/{rid}/properties/{pid}"),
                            J::Null,
                        )?;
                        p["relation"] =
                            J::Array(items.iter().map(|i| i["relation"].clone()).collect());
                    }
                }
            }
            let blocks = client.blocks(&rid, 0, &mut out.files, &mut out.problems)?;
            rows.push(FetchedPage {
                id: rid,
                title: title_of_page(&r),
                parent: Some(id.clone()),
                blocks,
                properties: Some(props),
                created: ms(r["created_time"].as_str()),
                edited: ms(r["last_edited_time"].as_str()),
                created_by: person(&r["created_by"]),
                edited_by: person(&r["last_edited_by"]),
            });
        }
        done += 1;
        out.databases.push(FetchedDatabase {
            id: id.clone(),
            title,
            parent: parent_of(&db),
            inline: db["is_inline"].as_bool().unwrap_or(false),
            properties: db["properties"].clone(),
            rows,
        });
    }
    for p in pages {
        let id = id_of(&p);
        if row_ids.contains(&id) {
            continue;
        }
        let title = title_of_page(&p);
        progress(done, total, &title);
        done += 1;
        let blocks = client.blocks(&id, 0, &mut out.files, &mut out.problems)?;
        out.pages.push(FetchedPage {
            id,
            title,
            parent: parent_of(&p),
            blocks,
            properties: None,
            created: ms(p["created_time"].as_str()),
            edited: ms(p["last_edited_time"].as_str()),
            created_by: None,
            edited_by: None,
        });
    }
    progress(total, total, "Fetched");
    Ok(out)
}

fn color(j: &J) -> Color {
    Color::from_name(j.as_str().unwrap_or("default"))
}

fn number_format(name: &str) -> NumberFormat {
    NumberFormat::ALL
        .into_iter()
        .find(|f| f.api_name() == name)
        .unwrap_or_default()
}

/// A Notion formula's expression, with its property references (`{{notion:block_property:
/// <id>:…}}`) written as `prop("Name")` again.
pub(crate) fn formula_text(expression: &str, names: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = expression;
    while let Some(start) = rest.find("{{notion:block_property:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "{{notion:block_property:".len()..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let id = super::notion::percent_decode(after[..end].split(':').next().unwrap_or_default());
        match names.get(&id) {
            Some(name) => out.push_str(&format!("prop(\"{}\")", name.replace('"', "\\\""))),
            None => out.push_str("prop(\"?\")"),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

impl Core {
    /// Makes pages and databases from what [`fetch`] read.
    #[allow(clippy::too_many_lines)]
    pub fn import_notion_fetched(&mut self, f: &Fetched) -> Result<NotionReport, ApiError> {
        let mut report = NotionReport {
            unmapped: f.problems.clone(),
            ..NotionReport::default()
        };
        let top = self.workspace.create_page(None, "Imported from Notion")?;
        self.store.create_page(&top)?;
        let top_id = top.id();
        report.page = Some(top_id);
        report.pages += 1;
        for (blob, (_, bytes)) in &f.files {
            self.store.put_blob(blob, bytes, false)?;
            report.attachments += 1;
        }

        // Pages and databases, parents first: a thing whose parent isn't here yet waits.
        let mut ids: HashMap<String, PageId> = HashMap::new();
        let mut pending: Vec<(String, Option<String>, String, bool)> = f
            .pages
            .iter()
            .map(|p| (p.id.clone(), p.parent.clone(), p.title.clone(), false))
            .chain(
                f.databases
                    .iter()
                    .map(|d| (d.id.clone(), d.parent.clone(), d.title.clone(), true)),
            )
            .collect();
        let known: HashSet<String> = pending.iter().map(|p| p.0.clone()).collect();
        // Blocks a database sits in (an inline one's parent is a block): look up their page.
        while !pending.is_empty() {
            let before = pending.len();
            let mut rest = Vec::new();
            for (id, parent, title, is_db) in pending {
                let under = match parent.as_ref().filter(|p| known.contains(*p)) {
                    None => Some(top_id),
                    Some(p) => ids.get(p).copied(),
                };
                let Some(under) = under else {
                    rest.push((id, parent, title, is_db));
                    continue;
                };
                let page = if is_db {
                    let inline = f.databases.iter().any(|d| d.id == id && d.inline);
                    let (page, _) = self
                        .workspace
                        .create_database(Some(under), &title, inline)?;
                    report.databases += 1;
                    page
                } else {
                    report.pages += 1;
                    self.workspace.create_page(Some(under), &title)?
                };
                self.store.create_page(&page)?;
                ids.insert(id, page.id());
            }
            if rest.len() == before {
                // A loop of parents (shouldn't happen): put them at the top.
                for (id, _, title, is_db) in rest {
                    let page = if is_db {
                        self.workspace
                            .create_database(Some(top_id), &title, false)?
                            .0
                    } else {
                        self.workspace.create_page(Some(top_id), &title)?
                    };
                    self.store.create_page(&page)?;
                    ids.insert(id, page.id());
                }
                break;
            }
            pending = rest;
        }
        for d in &f.databases {
            let db = ids[&d.id];
            for r in &d.rows {
                let page = self.workspace.create_page(Some(db), &r.title)?;
                self.store.create_page(&page)?;
                ids.insert(r.id.clone(), page.id());
                report.rows += 1;
            }
        }

        // Schemas: every property, with Notion's IDs kept as Drusite's where they fit.
        let mut prop_ids: HashMap<(String, String), String> = HashMap::new(); // (db, notion prop id) → ours
        for d in &f.databases {
            let props = d.properties.as_object().cloned().unwrap_or_default();
            for (_, p) in &props {
                let nid = p["id"].as_str().unwrap_or_default().to_owned();
                let ours = if p["type"] == "title" {
                    TITLE.to_owned()
                } else {
                    short_id()
                };
                prop_ids.insert((d.id.clone(), nid), ours);
            }
        }
        let db_of = |nid: &str| -> Option<String> {
            let n = nid.replace('-', "");
            f.databases.iter().find(|d| d.id == n).map(|d| d.id.clone())
        };
        for d in &f.databases {
            let db = ids[&d.id];
            let props = d.properties.as_object().cloned().unwrap_or_default();
            let names: HashMap<String, String> = props
                .iter()
                .map(|(name, p)| {
                    (
                        p["id"].as_str().unwrap_or_default().to_owned(),
                        name.clone(),
                    )
                })
                .collect();
            let mut position = 1.0;
            let mut shown = vec![ViewProperty {
                id: TITLE.into(),
                width: None,
            }];
            let mut board_by = None;
            let mut ordered: Vec<(&String, &J)> = props.iter().collect();
            ordered.sort_by_key(|(name, _)| (*name).clone());
            for (name, p) in ordered {
                let nid = p["id"].as_str().unwrap_or_default().to_owned();
                let ours = prop_ids[&(d.id.clone(), nid.clone())].clone();
                let t = p["type"].as_str().unwrap_or_default();
                let body = &p[t];
                let options = |groups: bool| -> Vec<SelectOption> {
                    let group_of = |oid: &str| -> Option<StatusGroup> {
                        body["groups"].as_array()?.iter().find_map(|g| {
                            g["option_ids"]
                                .as_array()?
                                .iter()
                                .any(|o| o.as_str() == Some(oid))
                                .then(|| match g["name"].as_str().unwrap_or_default() {
                                    "Complete" => StatusGroup::Complete,
                                    "In progress" => StatusGroup::InProgress,
                                    _ => StatusGroup::ToDo,
                                })
                        })
                    };
                    body["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                        .map(|(i, o)| {
                            let oid = o["id"].as_str().unwrap_or_default();
                            SelectOption {
                                id: clean_id(oid),
                                name: o["name"].as_str().unwrap_or_default().to_owned(),
                                color: color(&o["color"]),
                                group: if groups {
                                    group_of(oid).or(Some(StatusGroup::ToDo))
                                } else {
                                    None
                                },
                                position: i as f64,
                            }
                        })
                        .collect()
                };
                let (kind, opts) = match t {
                    "title" => {
                        let mut title = self
                            .workspace
                            .schema(db)?
                            .property(TITLE)
                            .cloned()
                            .ok_or_else(|| ApiError::new(ErrorCode::Internal, "no title"))?;
                        title.name.clone_from(name);
                        self.workspace.put_property(db, &title)?;
                        continue;
                    }
                    "rich_text" => (PropertyKind::Text, Vec::new()),
                    "number" => (
                        PropertyKind::Number {
                            format: number_format(body["format"].as_str().unwrap_or("number")),
                        },
                        Vec::new(),
                    ),
                    "select" => (PropertyKind::Select, options(false)),
                    "multi_select" => (PropertyKind::MultiSelect, options(false)),
                    "status" => (PropertyKind::Status, options(true)),
                    "date" => (PropertyKind::Date, Vec::new()),
                    "checkbox" => (PropertyKind::Checkbox, Vec::new()),
                    "url" => (PropertyKind::Url, Vec::new()),
                    "email" => (PropertyKind::Email, Vec::new()),
                    "phone_number" => (PropertyKind::Phone, Vec::new()),
                    "files" => (PropertyKind::Files, Vec::new()),
                    "people" => (PropertyKind::People, Vec::new()),
                    "created_time" => (PropertyKind::CreatedTime, Vec::new()),
                    "created_by" => (PropertyKind::CreatedBy, Vec::new()),
                    "last_edited_time" => (PropertyKind::EditedTime, Vec::new()),
                    "last_edited_by" => (PropertyKind::EditedBy, Vec::new()),
                    "unique_id" => (
                        PropertyKind::UniqueId {
                            prefix: body["prefix"].as_str().map(str::to_owned),
                        },
                        Vec::new(),
                    ),
                    "relation" => {
                        let Some(target) = body["database_id"].as_str().and_then(db_of) else {
                            report.unmapped.push(format!(
                                "{} › {name}: relates to a database that isn't shared with Drusite",
                                d.title
                            ));
                            continue;
                        };
                        let twin = body["dual_property"]["synced_property_id"]
                            .as_str()
                            .and_then(|tid| prop_ids.get(&(target.clone(), percent(tid))).cloned());
                        // Of a two-way pair, the side whose ID sorts first holds the links.
                        let mirror = twin.is_some() && {
                            let me = (d.id.clone(), nid.clone());
                            let other = (
                                target.clone(),
                                percent(
                                    body["dual_property"]["synced_property_id"]
                                        .as_str()
                                        .unwrap_or_default(),
                                ),
                            );
                            me > other
                        };
                        (
                            PropertyKind::Relation {
                                database: ids[&target],
                                twin,
                                mirror,
                                single: false,
                            },
                            Vec::new(),
                        )
                    }
                    "rollup" => {
                        let rel_nid = body["relation_property_id"]
                            .as_str()
                            .map(percent)
                            .unwrap_or_default();
                        let relation = prop_ids.get(&(d.id.clone(), rel_nid.clone())).cloned();
                        let target_db = props
                            .values()
                            .find(|p| p["id"].as_str().map(percent).as_deref() == Some(&rel_nid))
                            .and_then(|p| p["relation"]["database_id"].as_str())
                            .and_then(db_of);
                        let target = target_db.as_ref().and_then(|tdb| {
                            prop_ids
                                .get(&(
                                    tdb.clone(),
                                    body["rollup_property_id"]
                                        .as_str()
                                        .map(percent)
                                        .unwrap_or_default(),
                                ))
                                .cloned()
                        });
                        let function = body["function"]
                            .as_str()
                            .and_then(RollupFunction::from_api_name)
                            .unwrap_or_default();
                        match (relation, target) {
                            (Some(relation), Some(target)) => (
                                PropertyKind::Rollup {
                                    relation,
                                    target,
                                    function,
                                },
                                Vec::new(),
                            ),
                            _ => {
                                report.unmapped.push(format!(
                                    "{} › {name}: the rollup's relation or property isn't shared with Drusite",
                                    d.title
                                ));
                                continue;
                            }
                        }
                    }
                    "formula" => {
                        let expression =
                            formula_text(body["expression"].as_str().unwrap_or_default(), &names);
                        (PropertyKind::Formula { expression }, Vec::new())
                    }
                    other => {
                        report.unmapped.push(format!(
                            "{} › {name}: {other} properties aren't in Drusite",
                            d.title
                        ));
                        continue;
                    }
                };
                if matches!(kind, PropertyKind::Status) && board_by.is_none() {
                    board_by = Some(ours.clone());
                }
                if matches!(kind, PropertyKind::Formula { .. }) {
                    // Formulas go in last, once every property they name is there.
                }
                self.workspace.put_property(
                    db,
                    &PropertyDef {
                        id: ours.clone(),
                        name: name.clone(),
                        kind,
                        options: opts,
                        position,
                        description: p["description"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned),
                    },
                )?;
                position += 1.0;
                shown.push(ViewProperty {
                    id: ours,
                    width: None,
                });
            }
            // Views: Notion's API doesn't give them. A table, and a board by status or select.
            let schema = self.workspace.schema(db)?;
            let board_by = board_by.or_else(|| {
                schema
                    .properties
                    .iter()
                    .find(|p| matches!(p.kind, PropertyKind::Select))
                    .map(|p| p.id.clone())
            });
            let mut table = schema.views[0].clone();
            table.properties = shown.clone();
            self.workspace.put_view(db, &table)?;
            if let Some(by) = board_by {
                let mut board = View::new(short_id(), "Board", Layout::Board);
                board.position = 1.0;
                board.group = Some(Grouping {
                    property: by,
                    hidden: Vec::new(),
                    hide_empty: false,
                    date_unit: None,
                });
                board.properties = shown;
                self.workspace.put_view(db, &board)?;
            }
            // Formulas that don't type-check here (a function Drusite lacks) are reported.
            for p in &schema.properties {
                if let PropertyKind::Formula { expression } = &p.kind
                    && let Err(e) = drusite_formula::parse(expression)
                        .and_then(|e| drusite_formula::check(&e, &super::compute::Types(&schema)))
                {
                    report.unmapped.push(format!(
                        "{} › {}: formula {expression:?}: {}",
                        d.title, p.name, e.message
                    ));
                }
            }
        }
        report.unmapped.push(
            "Notion's API doesn't give views: each database has a table, and a board by its status"
                .into(),
        );

        // Rows' values.
        for d in &f.databases {
            let db = ids[&d.id];
            let schema = self.workspace.schema(db)?;
            for r in &d.rows {
                let row = ids[&r.id];
                self.workspace.set_row_history(
                    row,
                    r.created,
                    r.created_by.as_ref(),
                    r.edited,
                    r.edited_by.as_ref(),
                )?;
                let Some(props) = r.properties.as_ref().and_then(J::as_object) else {
                    continue;
                };
                for (name, p) in props {
                    let nid = p["id"].as_str().unwrap_or_default().to_owned();
                    let Some(ours) = prop_ids
                        .get(&(d.id.clone(), percent(&nid)))
                        .or_else(|| prop_ids.get(&(d.id.clone(), nid.clone())))
                    else {
                        continue;
                    };
                    let Some(def) = schema.property(ours) else {
                        continue;
                    };
                    if !def.kind.is_stored() {
                        continue;
                    }
                    if let Some(v) = api_value(p, def, &ids) {
                        if let Err(e) = self.workspace.set_value(row, ours, Some(&v)) {
                            report
                                .unmapped
                                .push(format!("{} › {} › {name}: {e}", d.title, r.title));
                        }
                    }
                }
            }
        }

        // Content.
        let all_pages = f
            .pages
            .iter()
            .chain(f.databases.iter().flat_map(|d| d.rows.iter()));
        for p in all_pages {
            let page = ids[&p.id];
            let wanted: Vec<Wanted> = p
                .blocks
                .iter()
                .map(|b| {
                    let kind = match &b.kind {
                        FetchedKind::Block(k) => k.clone(),
                        FetchedKind::ChildDatabase(nid) => match ids.get(nid) {
                            Some(database) => BlockKind::Database {
                                database: *database,
                                view: None,
                            },
                            None => BlockKind::Paragraph,
                        },
                    };
                    let spans = b
                        .spans
                        .iter()
                        .map(|s| {
                            let mut s = s.clone();
                            if let Some(l) = &s.marks.link
                                && let Some(nid) = l.strip_prefix("notion:")
                                && let Some(id) = ids.get(nid)
                            {
                                s.marks.link = Some(format!("{PAGE_LINK}{id}"));
                            }
                            s
                        })
                        .collect();
                    Wanted {
                        id: None,
                        kind,
                        indent: b.indent,
                        spans,
                    }
                })
                .collect();
            if !wanted.is_empty() {
                let (mut doc, mut at) = self.store.load_page(page)?;
                merge::apply(&mut doc, &wanted)?;
                self.store.save_page(&doc, &mut at)?;
            }
            self.changed(*page.as_uuid());
        }
        if report.attachments > 0
            && let Some(notify) = &self.on_change
        {
            notify();
        }
        self.save_workspace()?;
        Ok(report)
    }
}

/// Notion's option and property IDs hold characters Drusite's keys don't (`:` in `%3A`).
fn clean_id(id: &str) -> String {
    let c: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if c.is_empty() { short_id() } else { c }
}

fn percent(id: &str) -> String {
    super::notion::percent_decode(id)
}

fn api_date(j: &J) -> Option<DateValue> {
    let start = DateTime::parse(j["start"].as_str()?)?;
    let end = j["end"].as_str().and_then(DateTime::parse);
    Some(DateValue { start, end })
}

/// A row's value for one property, as the API gave it.
fn api_value(p: &J, def: &PropertyDef, ids: &HashMap<String, PageId>) -> Option<Value> {
    let t = p["type"].as_str()?;
    let body = &p[t];
    let opt = |o: &J| -> Option<String> {
        let id = clean_id(o["id"].as_str()?);
        def.options.iter().any(|x| x.id == id).then_some(id)
    };
    Some(match t {
        "rich_text" => Value::Text(plain(body)),
        "number" => Value::Number(body.as_f64()?),
        "select" => Value::Select(opt(body)?),
        "status" => Value::Status(opt(body)?),
        "multi_select" => Value::MultiSelect(body.as_array()?.iter().filter_map(opt).collect()),
        "date" => Value::Date(api_date(body)?),
        "checkbox" => Value::Checkbox(body.as_bool()?),
        "url" => Value::Url(body.as_str()?.to_owned()),
        "email" => Value::Email(body.as_str()?.to_owned()),
        "phone_number" => Value::Phone(body.as_str()?.to_owned()),
        "people" => Value::People(body.as_array()?.iter().filter_map(person).collect()),
        "files" => Value::Files(
            body.as_array()?
                .iter()
                .map(|f| FileRef {
                    name: f["name"].as_str().unwrap_or("file").to_owned(),
                    blob: None,
                    url: f["external"]["url"]
                        .as_str()
                        .or_else(|| f["file"]["url"].as_str())
                        .map(str::to_owned),
                })
                .collect(),
        ),
        "relation" => Value::Relation(
            body.as_array()?
                .iter()
                .filter_map(|r| {
                    let r = if r.is_array() { r.get(0)? } else { r };
                    ids.get(&r["id"].as_str()?.replace('-', "")).copied()
                })
                .collect(),
        ),
        "unique_id" => Value::UniqueId(u32::try_from(body["number"].as_u64()?).ok()?),
        _ => return None,
    })
}

/// Where the Notion token is kept.
pub mod token {
    use crate::api::ApiError;
    use crate::engine::Core;

    #[cfg(target_vendor = "apple")]
    const SERVICE: &str = "dev.drusite.notion";

    impl Core {
        /// Keeps `token` for later imports.
        pub fn save_notion_token(&mut self, token: &str) -> Result<(), ApiError> {
            #[cfg(target_vendor = "apple")]
            {
                use security_framework::passwords::{
                    PasswordOptions, set_generic_password_options,
                };
                let mut o = PasswordOptions::new_generic_password(SERVICE, "token");
                o.set_access_synchronized(Some(false));
                o.set_label("Drusite: Notion integration token");
                set_generic_password_options(token.trim().as_bytes(), o).map_err(|e| {
                    ApiError::new(crate::api::ErrorCode::Storage, format!("Keychain: {e}"))
                })
            }
            #[cfg(not(target_vendor = "apple"))]
            {
                self.store.set_secret("notion", Some(token.trim()))?;
                Ok(())
            }
        }

        pub fn notion_token(&self) -> Result<Option<zeroize::Zeroizing<String>>, ApiError> {
            #[cfg(target_vendor = "apple")]
            {
                use security_framework::passwords::{PasswordOptions, generic_password};
                match generic_password(PasswordOptions::new_generic_password(SERVICE, "token")) {
                    Ok(b) => Ok(String::from_utf8(b).ok().map(zeroize::Zeroizing::new)),
                    Err(e) if e.code() == -25_300 => Ok(None),
                    Err(e) => Err(ApiError::new(
                        crate::api::ErrorCode::Storage,
                        format!("Keychain: {e}"),
                    )),
                }
            }
            #[cfg(not(target_vendor = "apple"))]
            {
                Ok(self.store.secret("notion")?)
            }
        }

        pub fn forget_notion_token(&mut self) -> Result<(), ApiError> {
            #[cfg(target_vendor = "apple")]
            {
                use security_framework::passwords::delete_generic_password;
                match delete_generic_password(SERVICE, "token") {
                    Ok(()) => Ok(()),
                    Err(e) if e.code() == -25_300 => Ok(()),
                    Err(e) => Err(ApiError::new(
                        crate::api::ErrorCode::Storage,
                        format!("Keychain: {e}"),
                    )),
                }
            }
            #[cfg(not(target_vendor = "apple"))]
            {
                self.store.set_secret("notion", None)?;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_references_become_prop_calls() {
        let names = HashMap::from([("ab%3Ac".replace("%3A", ":"), "Points".to_owned())]);
        assert_eq!(
            formula_text(
                "{{notion:block_property:ab%3Ac:00000000-0000-0000-0000-000000000000:8994905a}} * 2",
                &names
            ),
            "prop(\"Points\") * 2"
        );
    }
}

/// Whether `command` is one of the Notion API commands, which [`run`] carries out.
pub fn is_notion_api(command: &crate::api::Command) -> bool {
    use crate::api::Command as C;
    matches!(
        command,
        C::ImportNotionApi { .. } | C::NotionToken | C::ForgetNotionToken
    )
}

/// Runs a Notion API command: the network part off the async threads and without the core's
/// lock, which is taken only to read the token and to make the pages at the end.
pub async fn run(
    core: &std::sync::Arc<std::sync::Mutex<Core>>,
    command: crate::api::Command,
) -> Result<crate::api::Response, ApiError> {
    use crate::api::{Command as C, Event, Response};
    let crashed = || {
        ApiError::new(
            ErrorCode::Internal,
            "the core crashed earlier; restart Drusite",
        )
    };
    match command {
        C::NotionToken => Ok(Response::NotionToken {
            saved: core
                .lock()
                .map_err(|_| crashed())?
                .notion_token()?
                .is_some(),
        }),
        C::ForgetNotionToken => {
            core.lock().map_err(|_| crashed())?.forget_notion_token()?;
            Ok(Response::Done)
        }
        C::ImportNotionApi { token } => {
            let given = token
                .map(|t| zeroize::Zeroizing::new(t.trim().to_owned()))
                .filter(|t| !t.is_empty());
            let token = match &given {
                Some(t) => t.clone(),
                None => core
                    .lock()
                    .map_err(|_| crashed())?
                    .notion_token()?
                    .ok_or_else(|| {
                        ApiError::new(
                            ErrorCode::Invalid,
                            "paste your Notion integration's token first",
                        )
                    })?,
            };
            let for_fetch = std::sync::Arc::clone(core);
            let fetched = tokio::task::spawn_blocking(move || {
                let client = Client::new(API, &token);
                fetch(&client, &|done, total, what| {
                    if let Ok(c) = for_fetch.lock() {
                        c.emit(Event::NotionProgress {
                            done,
                            total,
                            what: what.to_owned(),
                        });
                    }
                })
            })
            .await
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))??;
            let core = std::sync::Arc::clone(core);
            tokio::task::spawn_blocking(move || {
                let mut core = core.lock().map_err(|_| crashed())?;
                // Kept once Notion has accepted it.
                if let Some(t) = &given {
                    core.save_notion_token(t)?;
                }
                Ok(Response::NotionImported {
                    report: core.import_notion_fetched(&fetched)?,
                })
            })
            .await
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
        }
        _ => Err(ApiError::new(ErrorCode::Internal, "not a Notion command")),
    }
}
