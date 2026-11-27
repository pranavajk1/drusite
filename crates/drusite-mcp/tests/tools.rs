// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tools against an in-memory set of documents: what agents read, what they may change,
//! and that every change can be undone without losing anything the person did meanwhile.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Mutex;

use drusite_crdt::{Page, Workspace};
use drusite_markdown::encode_block_id;
use drusite_mcp::{
    Access, Activity, Agent, Change, Hit, Notes, ToolError, ToolResult, WorkspaceInfo, call_tool,
    undo,
};
use drusite_model::{
    BlockKind, Day, PageId, PageKind, Property, PropertyValue, WorkspaceId, journal,
};
use serde_json::json;

/// One workspace, its pages, and what agents did.
struct Memory {
    ws: Mutex<Workspace>,
    pages: Mutex<HashMap<PageId, Page>>,
    log: Mutex<Vec<Activity>>,
    writable: bool,
}

fn copy_page(p: &Page) -> Page {
    Page::load(&p.export_snapshot().unwrap()).unwrap()
}

impl Memory {
    fn new() -> Self {
        Self {
            ws: Mutex::new(Workspace::create().unwrap()),
            pages: Mutex::new(HashMap::new()),
            log: Mutex::new(Vec::new()),
            writable: true,
        }
    }

    fn id(&self) -> WorkspaceId {
        self.ws.lock().unwrap().id()
    }

    /// A page the person made, with a paragraph per text.
    fn add(&self, parent: Option<PageId>, title: &str, texts: &[&str]) -> PageId {
        let mut page = self.ws.lock().unwrap().create_page(parent, title).unwrap();
        let mut last = page.order()[0];
        page.insert_text(last, 0, texts[0]).unwrap();
        for t in &texts[1..] {
            let (id, _) = page
                .insert_block_after(Some(last), BlockKind::Paragraph)
                .unwrap();
            page.insert_text(id, 0, t).unwrap();
            last = id;
        }
        let id = page.id();
        self.pages.lock().unwrap().insert(id, page);
        id
    }

    fn texts(&self, id: PageId) -> Vec<String> {
        self.pages.lock().unwrap()[&id]
            .blocks()
            .unwrap()
            .iter()
            .map(drusite_model::Block::text)
            .collect()
    }

    /// Undoes the latest activity, as the app would.
    fn undo_last(&self) {
        let activity = self.log.lock().unwrap().pop().unwrap();
        let mut ws = self.ws.lock().unwrap();
        let mut pages = self.pages.lock().unwrap();
        let mut loaded: HashMap<PageId, Page> = activity
            .pages()
            .into_iter()
            .map(|p| (p, copy_page(&pages[&p])))
            .collect();
        let change = undo(&activity, &mut ws, &mut loaded, 1).unwrap();
        for (id, bytes) in change.pages {
            pages.get_mut(&id).unwrap().import(&bytes).unwrap();
        }
    }
}

impl Notes for Memory {
    async fn workspaces(&self, _: &Agent) -> ToolResult<Vec<WorkspaceInfo>> {
        Ok(vec![WorkspaceInfo {
            id: self.id(),
            writable: self.writable,
        }])
    }

    async fn workspace(&self, _: &Agent, ws: WorkspaceId) -> ToolResult<Workspace> {
        assert_eq!(ws, self.id());
        let ws = self.ws.lock().unwrap();
        Ok(Workspace::load(&ws.export_snapshot().unwrap()).unwrap())
    }

    async fn find_page(&self, _: &Agent, page: PageId) -> ToolResult<Option<WorkspaceId>> {
        Ok(self
            .pages
            .lock()
            .unwrap()
            .contains_key(&page)
            .then(|| self.id()))
    }

    async fn page(&self, _: &Agent, _: WorkspaceId, page: PageId) -> ToolResult<Page> {
        Ok(copy_page(&self.pages.lock().unwrap()[&page]))
    }

    async fn search(
        &self,
        _: &Agent,
        _: WorkspaceId,
        query: &str,
        limit: usize,
    ) -> ToolResult<Vec<Hit>> {
        let ws = self.ws.lock().unwrap();
        let mut hits = Vec::new();
        for (id, page) in self.pages.lock().unwrap().iter() {
            for b in page.blocks().unwrap() {
                if b.text().contains(query) {
                    hits.push(Hit {
                        page: *id,
                        block: Some(b.id),
                        title: ws.title(*id).unwrap(),
                        snippet: b.text(),
                    });
                }
            }
        }
        hits.truncate(limit);
        Ok(hits)
    }

    async fn commit(
        &self,
        _: &Agent,
        _: WorkspaceId,
        change: Change,
        activity: Activity,
    ) -> ToolResult<()> {
        let mut pages = self.pages.lock().unwrap();
        for (id, snapshot) in change.created {
            pages.insert(id, Page::load(&snapshot).unwrap());
        }
        for (id, bytes) in change.pages {
            pages.get_mut(&id).unwrap().import(&bytes).unwrap();
        }
        if let Some(bytes) = change.workspace {
            self.ws.lock().unwrap().import(&bytes).unwrap();
        }
        self.log.lock().unwrap().push(activity);
        Ok(())
    }
}

fn agent(write: bool, journal: bool) -> Agent {
    Agent {
        name: "Test agent".into(),
        access: Access {
            write,
            journal,
            workspaces: None,
        },
        user: None,
        grant: uuid::Uuid::nil(),
    }
}

async fn run(
    notes: &Memory,
    agent: &Agent,
    tool: &str,
    args: serde_json::Value,
) -> ToolResult<String> {
    call_tool(notes, agent, tool, args).await
}

#[tokio::test]
async fn agents_read_pages_as_markdown_with_block_ids() {
    let notes = Memory::new();
    let plans = notes.add(None, "Plans", &["Buy a kettle", "Call the plumber"]);
    let _sub = notes.add(Some(plans), "Kitchen", &["Tiles"]);
    let a = agent(false, false);

    let tree = run(&notes, &a, "list_pages", json!({})).await.unwrap();
    assert!(tree.contains(&format!("- Plans (page {plans})")), "{tree}");
    assert!(tree.contains("  - Kitchen"), "{tree}");

    let md = run(
        &notes,
        &a,
        "read_page",
        json!({ "page": plans.to_string() }),
    )
    .await
    .unwrap();
    assert!(md.contains("drusite-title: Plans"), "{md}");
    let first = notes.pages.lock().unwrap()[&plans].order()[0];
    assert!(
        md.contains(&format!("Buy a kettle ^{}", encode_block_id(first))),
        "{md}"
    );

    let found = run(&notes, &a, "search", json!({ "query": "plumber" }))
        .await
        .unwrap();
    assert!(found.contains("“Plans”"), "{found}");

    // Links work as IDs too.
    let again = run(
        &notes,
        &a,
        "read_page",
        json!({ "page": format!("drusite:page/{plans}") }),
    )
    .await
    .unwrap();
    assert_eq!(again, md);
}

#[tokio::test]
async fn an_appended_note_is_undone_and_the_persons_typing_stays() {
    let notes = Memory::new();
    let page = notes.add(None, "Inbox", &["first"]);
    let a = agent(true, false);
    let said = run(
        &notes,
        &a,
        "append_to_page",
        json!({ "page": page.to_string(), "markdown": "- milk\n- eggs\n" }),
    )
    .await
    .unwrap();
    assert_eq!(said, "Added 2 blocks to “Inbox”.");
    assert_eq!(notes.texts(page), ["first", "milk", "eggs"]);
    assert_eq!(
        notes.log.lock().unwrap()[0].summary,
        "Added 2 blocks to “Inbox”"
    );

    // The person types after the agent did.
    {
        let mut pages = notes.pages.lock().unwrap();
        let p = pages.get_mut(&page).unwrap();
        let first = p.order()[0];
        p.insert_text(first, 5, " thing").unwrap();
    }
    notes.undo_last();
    assert_eq!(notes.texts(page), ["first thing"]);
}

#[tokio::test]
async fn an_edit_keeps_blocks_by_reference_and_can_set_properties() {
    let notes = Memory::new();
    let page = notes.add(None, "Draft", &["keep me", "drop me", "change me"]);
    let a = agent(true, false);
    let md = run(&notes, &a, "read_page", json!({ "page": page.to_string() }))
        .await
        .unwrap();
    let ids_before = notes.pages.lock().unwrap()[&page].order();
    let edited = md
        .replace("drop me", "DELETE")
        .lines()
        .filter(|l| !l.starts_with("DELETE"))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("change me", "changed")
        .replace("drusite-title: Draft", "drusite-title: Final\nStatus: done");
    let said = run(
        &notes,
        &a,
        "update_page",
        json!({ "page": page.to_string(), "markdown": edited }),
    )
    .await
    .unwrap();
    assert_eq!(said, "Edited “Draft”.");
    assert_eq!(notes.texts(page), ["keep me", "changed"]);
    let ids_after = notes.pages.lock().unwrap()[&page].order();
    assert_eq!(
        ids_after,
        [ids_before[0], ids_before[2]],
        "blocks keep their IDs"
    );
    let ws = notes.ws.lock().unwrap();
    assert_eq!(ws.title(page).unwrap(), "Final");
    assert_eq!(
        ws.properties(page).unwrap(),
        [Property {
            name: "Status".into(),
            value: PropertyValue::Text("done".into())
        }]
    );
    drop(ws);

    notes.undo_last();
    assert_eq!(notes.texts(page), ["keep me", "drop me", "change me"]);
    let ws = notes.ws.lock().unwrap();
    assert_eq!(ws.title(page).unwrap(), "Draft");
    assert!(ws.properties(page).unwrap().is_empty());
}

#[tokio::test]
async fn a_created_page_is_trashed_on_undo() {
    let notes = Memory::new();
    let parent = notes.add(None, "Projects", &["…"]);
    let a = agent(true, false);
    let said = run(
        &notes,
        &a,
        "create_page",
        json!({ "title": "Garden", "parent": parent.to_string(), "markdown": "# Plan\n\nDig." }),
    )
    .await
    .unwrap();
    let id: PageId = said
        .rsplit("page ")
        .next()
        .unwrap()
        .trim_end_matches(").")
        .parse()
        .unwrap();
    assert_eq!(notes.texts(id), ["Plan", "Dig."]);
    {
        let ws = notes.ws.lock().unwrap();
        let node = ws
            .pages()
            .unwrap()
            .into_iter()
            .find(|n| n.id == id)
            .unwrap();
        assert_eq!(node.parent, Some(parent));
    }
    notes.undo_last();
    assert!(notes.ws.lock().unwrap().is_trashed(id).unwrap());
}

#[tokio::test]
async fn read_only_agents_and_the_journal_are_refused() {
    let notes = Memory::new();
    let page = notes.add(None, "Notes", &["x"]);
    let day: Day = "2026-11-26".parse().unwrap();
    let entry = {
        let mut ws = notes.ws.lock().unwrap();
        let p = ws
            .create_page_of(
                None,
                &day.title(),
                PageKind::Journal,
                &[Property {
                    name: journal::DATE.into(),
                    value: PropertyValue::Date(day),
                }],
            )
            .unwrap();
        let id = p.id();
        notes.pages.lock().unwrap().insert(id, p);
        id
    };

    let reader = agent(false, false);
    let err = run(
        &notes,
        &reader,
        "append_to_page",
        json!({ "page": page.to_string(), "markdown": "y" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::Forbidden(_)), "{err}");

    let writer = agent(true, false);
    let journal = run(
        &notes,
        &writer,
        "read_journal",
        json!({ "date": "2026-11-26" }),
    )
    .await
    .unwrap();
    assert!(journal.contains("drusite-kind: journal"), "{journal}");
    let err = run(
        &notes,
        &writer,
        "append_to_page",
        json!({ "page": entry.to_string(), "markdown": "y" }),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("journal entries are read-only"),
        "{err}"
    );

    let trusted = agent(true, true);
    run(
        &notes,
        &trusted,
        "append_to_page",
        json!({ "page": entry.to_string(), "markdown": "y" }),
    )
    .await
    .unwrap();

    // A workspace the sign-in doesn't name is invisible.
    let mut elsewhere = agent(true, true);
    elsewhere.access.workspaces = Some(vec![WorkspaceId::new()]);
    let err = run(
        &notes,
        &elsewhere,
        "read_page",
        json!({ "page": page.to_string() }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::NotFound(_)), "{err}");

    // There is no tool that deletes.
    assert!(
        drusite_mcp::TOOLS
            .iter()
            .all(|t| !t.name.contains("delete"))
    );
}

#[tokio::test]
async fn trashed_pages_are_out_of_sight() {
    let notes = Memory::new();
    let page = notes.add(None, "Old", &["x"]);
    notes.ws.lock().unwrap().trash_page(page, 1).unwrap();
    let a = agent(true, false);
    let err = run(&notes, &a, "read_page", json!({ "page": page.to_string() }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::NotFound(_)));
    let tree = run(&notes, &a, "list_pages", json!({})).await.unwrap();
    assert_eq!(tree, "No pages.");
}
