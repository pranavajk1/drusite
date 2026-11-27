// SPDX-License-Identifier: AGPL-3.0-or-later

//! Importing from Notion's API, against a fake API serving a synthetic workspace in the API's
//! own shapes (version 2022-06-28): a Kanban of tasks, a two-way relation, a rollup, formulas
//! whose property references are Notion's `{{notion:block_property:…}}`, paging, and a wrong
//! token.

#![cfg(feature = "net")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use drusite_core::api::{Command, Response};
use drusite_core::database::notion_api::{Client, fetch};
use drusite_core::database::{Clock, ViewResult};
use drusite_core::{Core, StoreKey};
use drusite_model::database::{DateTime, Layout, PropertyKind};
use serde_json::{Value as J, json};

const TOKEN: &str = "secret_synthetic_test_token_123";
const TOP: &str = "aaaa0000-0000-4000-8000-000000000001";
const PROJECTS: &str = "aaaa0000-0000-4000-8000-000000000002";
const TASKS: &str = "aaaa0000-0000-4000-8000-000000000003";
const GARDEN: &str = "bbbb0000-0000-4000-8000-000000000001";
const T1: &str = "cccc0000-0000-4000-8000-000000000001";
const T2: &str = "cccc0000-0000-4000-8000-000000000002";
const T3: &str = "cccc0000-0000-4000-8000-000000000003";

fn text(s: &str) -> J {
    json!([{"type": "text", "plain_text": s, "text": {"content": s}, "annotations": {"bold": false, "italic": false, "strikethrough": false, "underline": false, "code": false}, "href": null}])
}

fn status_opt(id: &str, name: &str, color: &str) -> J {
    json!({"id": id, "name": name, "color": color})
}

fn tasks_db() -> J {
    json!({
        "object": "database", "id": TASKS, "is_inline": true,
        "title": text("Tasks"),
        "parent": {"type": "page_id", "page_id": TOP},
        "properties": {
            "Task": {"id": "title", "type": "title", "title": {}},
            "Status": {"id": "st%3Ax", "type": "status", "status": {
                "options": [status_opt("o1", "Not started", "default"), status_opt("o2", "In progress", "blue"), status_opt("o3", "Done", "green")],
                "groups": [
                    {"id": "g1", "name": "To-do", "color": "gray", "option_ids": ["o1"]},
                    {"id": "g2", "name": "In progress", "color": "blue", "option_ids": ["o2"]},
                    {"id": "g3", "name": "Complete", "color": "green", "option_ids": ["o3"]}
                ]}},
            "Points": {"id": "pts", "type": "number", "number": {"format": "number"}},
            "Due": {"id": "due", "type": "date", "date": {}},
            "Tags": {"id": "tg", "type": "multi_select", "multi_select": {"options": [{"id": "t1", "name": "web", "color": "blue"}, {"id": "t2", "name": "design", "color": "pink"}]}},
            "Project": {"id": "prj", "type": "relation", "relation": {"database_id": PROJECTS, "type": "dual_property", "dual_property": {"synced_property_name": "Tasks", "synced_property_id": "tsk"}}},
            "Estimate": {"id": "est", "type": "formula", "formula": {"expression": "{{notion:block_property:pts:00000000-0000-0000-0000-000000000000:8994905a-074a-415f-9bcf-d1f8b4fa38e4}} * 2 + \" h\""}},
            "Assignee": {"id": "who", "type": "people", "people": {}},
            "ID": {"id": "uid", "type": "unique_id", "unique_id": {"prefix": "TASK"}},
            "Button": {"id": "btn", "type": "button", "button": {}}
        }
    })
}

fn projects_db() -> J {
    json!({
        "object": "database", "id": PROJECTS, "is_inline": false,
        "title": text("Projects"),
        "parent": {"type": "page_id", "page_id": TOP},
        "properties": {
            "Name": {"id": "title", "type": "title", "title": {}},
            "Tasks": {"id": "tsk", "type": "relation", "relation": {"database_id": TASKS, "type": "dual_property", "dual_property": {"synced_property_name": "Project", "synced_property_id": "prj"}}},
            "Total points": {"id": "tot", "type": "rollup", "rollup": {"relation_property_name": "Tasks", "relation_property_id": "tsk", "rollup_property_name": "Points", "rollup_property_id": "pts", "function": "sum"}},
            "Budget": {"id": "bud", "type": "number", "number": {"format": "dollar"}}
        }
    })
}

fn task(id: &str, title: &str, status: &str, points: f64, project: Option<&str>) -> J {
    json!({
        "object": "page", "id": id,
        "created_time": "2026-11-20T09:00:00.000Z", "last_edited_time": "2026-11-21T10:30:00.000Z",
        "created_by": {"object": "user", "id": "u1", "name": "Ada Lovelace", "person": {"email": "ada@example.com"}},
        "last_edited_by": {"object": "user", "id": "u1", "name": "Ada Lovelace", "person": {"email": "ada@example.com"}},
        "parent": {"type": "database_id", "database_id": TASKS},
        "properties": {
            "Task": {"id": "title", "type": "title", "title": text(title)},
            "Status": {"id": "st%3Ax", "type": "status", "status": {"id": status, "name": "?", "color": "blue"}},
            "Points": {"id": "pts", "type": "number", "number": points},
            "Due": {"id": "due", "type": "date", "date": {"start": "2026-11-28T14:30:00.000-06:00", "end": null}},
            "Tags": {"id": "tg", "type": "multi_select", "multi_select": [{"id": "t1", "name": "web"}]},
            "Project": {"id": "prj", "type": "relation", "relation": project.map(|p| vec![json!({"id": p})]).unwrap_or_default(), "has_more": false},
            "Estimate": {"id": "est", "type": "formula", "formula": {"type": "string", "string": "x"}},
            "Assignee": {"id": "who", "type": "people", "people": [{"object": "user", "id": "u1", "name": "Ada Lovelace", "person": {"email": "ada@example.com"}}]},
            "ID": {"id": "uid", "type": "unique_id", "unique_id": {"prefix": "TASK", "number": 7}}
        }
    })
}

fn route(method: &str, path: &str, auth: Option<&str>) -> (u16, J) {
    if auth != Some(&format!("Bearer {TOKEN}")) {
        return (
            401,
            json!({"object": "error", "status": 401, "code": "unauthorized", "message": "API token is invalid."}),
        );
    }
    let list = |items: Vec<J>| json!({"object": "list", "results": items, "has_more": false, "next_cursor": null});
    match (method, path) {
        // Search answers in two pages, to check paging.
        ("POST", "/v1/search") => (
            200,
            json!({"object": "list", "results": [
            {"object": "page", "id": TOP, "parent": {"type": "workspace", "workspace": true}, "created_time": "2026-11-20T09:00:00.000Z", "last_edited_time": "2026-11-20T09:00:00.000Z",
             "properties": {"title": {"id": "title", "type": "title", "title": text("Team space")}}},
            {"object": "database", "id": TASKS}
        ], "has_more": true, "next_cursor": "two"}),
        ),
        ("POST", "/v1/search?2") => (
            200,
            list(vec![json!({"object": "database", "id": PROJECTS})]),
        ),
        ("GET", p) if p == format!("/v1/databases/{}", TASKS.replace('-', "")) => (200, tasks_db()),
        ("GET", p) if p == format!("/v1/databases/{}", PROJECTS.replace('-', "")) => {
            (200, projects_db())
        }
        ("POST", p) if p == format!("/v1/databases/{}/query", TASKS.replace('-', "")) => (
            200,
            list(vec![
                task(T1, "Water the seedlings", "o3", 3.0, Some(GARDEN)),
                task(T2, "Design the logo", "o2", 5.0, Some(GARDEN)),
                task(T3, "Inbox zero", "o1", 1.0, None),
            ]),
        ),
        ("POST", p) if p == format!("/v1/databases/{}/query", PROJECTS.replace('-', "")) => (
            200,
            list(vec![json!({
                "object": "page", "id": GARDEN, "created_time": "2026-11-20T09:00:00.000Z", "last_edited_time": "2026-11-20T09:00:00.000Z",
                "parent": {"type": "database_id", "database_id": PROJECTS},
                "properties": {
                    "Name": {"id": "title", "type": "title", "title": text("Garden app")},
                    "Tasks": {"id": "tsk", "type": "relation", "relation": [{"id": T1}, {"id": T2}], "has_more": false},
                    "Total points": {"id": "tot", "type": "rollup", "rollup": {"type": "number", "number": 8}},
                    "Budget": {"id": "bud", "type": "number", "number": 1250}
                }
            })]),
        ),
        ("GET", p) if p.starts_with(&format!("/v1/blocks/{}/children", TOP.replace('-', ""))) => (
            200,
            list(vec![
                json!({"object": "block", "id": "d1", "type": "heading_2", "has_children": false, "heading_2": {"rich_text": text("This week")}}),
                json!({"object": "block", "id": "d2", "type": "to_do", "has_children": false, "to_do": {"rich_text": text("Plant the tomatoes"), "checked": false}}),
                json!({"object": "block", "id": "d3", "type": "paragraph", "has_children": false, "paragraph": {"rich_text": [
                    {"type": "text", "plain_text": "See ", "annotations": {}, "href": null},
                    {"type": "mention", "plain_text": "Design the logo", "annotations": {"bold": true}, "mention": {"type": "page", "page": {"id": T2}}, "href": null}
                ]}}),
                json!({"object": "block", "id": TASKS, "type": "child_database", "has_children": false, "child_database": {"title": "Tasks"}}),
                json!({"object": "block", "id": "d4", "type": "callout", "has_children": false, "callout": {"rich_text": text("Tip"), "icon": {"type": "emoji", "emoji": "💡"}}}),
            ]),
        ),
        ("GET", p) if p.starts_with(&format!("/v1/blocks/{}/children", T1.replace('-', ""))) => (
            200,
            list(vec![
                json!({"object": "block", "id": "e1", "type": "paragraph", "has_children": false, "paragraph": {"rich_text": text("Twice a day.")}}),
            ]),
        ),
        ("GET", p) if p.starts_with("/v1/blocks/") => (200, list(Vec::new())),
        _ => (
            404,
            json!({"object": "error", "status": 404, "code": "object_not_found", "message": format!("no {path}")}),
        ),
    }
}

/// A fake Notion API on localhost; it remembers the paths asked for.
fn serve() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or_default().to_owned();
            let path = parts.next().unwrap_or_default().to_owned();
            let mut auth = None;
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                let (k, v) = h.split_once(": ").unwrap_or((h, ""));
                match k.to_lowercase().as_str() {
                    "authorization" => auth = Some(v.to_owned()),
                    "content-length" => len = v.parse().unwrap_or(0),
                    _ => {}
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            let body: J = serde_json::from_slice(&body).unwrap_or(J::Null);
            let key = if body["start_cursor"] == "two" {
                format!("{path}?2")
            } else {
                path.clone()
            };
            log.lock().unwrap().push(format!("{method} {key}"));
            let (status, json) = route(&method, &key, auth.as_deref());
            let text = json.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            );
        }
    });
    (base, seen)
}

fn view(core: &mut Core, db: drusite_model::PageId, view: Option<String>) -> ViewResult {
    match core
        .handle(Command::QueryView {
            database: db,
            view,
            clock: Clock {
                now: DateTime::parse("2026-11-27T12:00").unwrap(),
                utc_offset: 0,
            },
        })
        .unwrap()
    {
        Response::View { result } => result,
        other => panic!("{other:?}"),
    }
}

fn text_of(v: &ViewResult, title: &str, prop: &str) -> String {
    let p = v.schema.property_named(prop).unwrap().id.clone();
    let row = v
        .groups
        .iter()
        .flat_map(|g| &g.rows)
        .find(|r| r.title == title)
        .unwrap();
    row.cells
        .iter()
        .find(|c| c.property == p)
        .map(|c| c.error.clone().unwrap_or_else(|| c.text.clone()))
        .unwrap()
}

#[test]
fn a_workspace_from_the_api_keeps_its_types_formulas_rollups_and_relations() {
    let (base, seen) = serve();
    let fetched = fetch(&Client::new(&base, TOKEN), &|_, _, _| {}).unwrap();
    assert_eq!(fetched.databases.len(), 2);
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|s| s == "POST /v1/search?2"),
        "paged"
    );

    let dir = tempfile::tempdir().unwrap();
    let mut core =
        Core::open(dir.path(), &StoreKey::from_bytes([3; 32]), Box::new(|_| {})).unwrap();
    let report = core.import_notion_fetched(&fetched).unwrap();
    assert_eq!((report.databases, report.rows), (2, 4), "{report:?}");
    assert!(
        report.unmapped.iter().any(|u| u.contains("button")),
        "{report:?}"
    );

    let pages = match core.handle(Command::ListPages).unwrap() {
        Response::Pages { pages } => pages,
        other => panic!("{other:?}"),
    };
    let id = |t: &str| pages.iter().find(|p| p.title == t).unwrap().id;
    let tasks = id("Tasks");
    let projects = id("Projects");

    let t = view(&mut core, tasks, None);
    assert_eq!(
        text_of(&t, "Design the logo", "Estimate"),
        "10 h",
        "the formula came with its expression"
    );
    assert_eq!(text_of(&t, "Design the logo", "Project"), "Garden app");
    assert_eq!(text_of(&t, "Design the logo", "Assignee"), "Ada Lovelace");
    assert_eq!(text_of(&t, "Design the logo", "ID"), "TASK-7");
    assert_eq!(
        text_of(&t, "Design the logo", "Due"),
        "November 28, 2026 2:30 PM"
    );
    assert!(matches!(
        t.schema.property_named("Status").unwrap().kind,
        PropertyKind::Status
    ));
    let board = t
        .schema
        .views
        .iter()
        .find(|v| v.layout == Layout::Board)
        .unwrap()
        .id
        .clone();
    let b = view(&mut core, tasks, Some(board));
    let col = |label: &str| {
        b.groups
            .iter()
            .find(|g| g.label == label)
            .unwrap()
            .rows
            .len()
    };
    assert_eq!(
        (col("Not started"), col("In progress"), col("Done")),
        (1, 1, 1)
    );

    let p = view(&mut core, projects, None);
    assert_eq!(text_of(&p, "Garden app", "Total points"), "8");
    assert_eq!(text_of(&p, "Garden app", "Budget"), "$1,250.00");
    assert_eq!(
        text_of(&p, "Garden app", "Tasks"),
        "Water the seedlings, Design the logo"
    );

    // The top page: its blocks, the inline database, and the mention as a link to the page.
    let top = match core
        .handle(Command::OpenPage {
            page: id("Team space"),
        })
        .unwrap()
    {
        Response::Opened { view } => view,
        other => panic!("{other:?}"),
    };
    assert!(top.blocks.iter().any(|b| b.kind
        == drusite_model::BlockKind::Database {
            database: tasks,
            view: None
        }));
    let design = id("Design the logo");
    assert!(
        top.blocks
            .iter()
            .flat_map(|b| &b.spans)
            .any(|s| s.marks.link.as_deref() == Some(&format!("drusite:page/{design}")))
    );
}

#[test]
fn a_wrong_token_is_refused_without_repeating_it() {
    let (base, _) = serve();
    let err = fetch(&Client::new(&base, "secret_wrong_token_456"), &|_, _, _| {}).unwrap_err();
    assert!(
        err.message.contains("didn't accept the token"),
        "{}",
        err.message
    );
    assert!(!err.message.contains("secret_wrong"));
}

#[test]
fn the_token_is_kept_and_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let mut core =
        Core::open(dir.path(), &StoreKey::from_bytes([3; 32]), Box::new(|_| {})).unwrap();
    // On Apple devices this writes the login Keychain, so it runs only when asked, as the store
    // key's Keychain test does: `DRUSITE_TEST_KEYCHAIN=1`.
    if cfg!(target_vendor = "apple") && std::env::var_os("DRUSITE_TEST_KEYCHAIN").is_none() {
        return;
    }
    assert!(
        core.notion_token().unwrap().is_none(),
        "a token is already kept here"
    );
    core.save_notion_token(TOKEN).unwrap();
    assert_eq!(
        core.notion_token().unwrap().as_deref().map(String::as_str),
        Some(TOKEN)
    );
    core.forget_notion_token().unwrap();
    assert!(core.notion_token().unwrap().is_none());
}
