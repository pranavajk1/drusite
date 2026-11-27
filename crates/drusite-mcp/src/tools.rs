// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tools: what each one takes, says about itself, and does.

use std::sync::Arc;

use drusite_crdt::merge::{self, Wanted};
use drusite_crdt::{PageNode, Workspace};
use drusite_markdown::encode_block_id;
use drusite_model::{
    BlockKind, Day, PageId, PageKind, Property, PropertyValue, Span, WorkspaceId, journal,
};
use rmcp::model::JsonObject;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::markdown::{self, page_to_markdown};
use crate::{Activity, Agent, Change, Effect, Notes, ToolError, ToolResult, WorkspaceInfo};

/// How a tool presents itself.
#[derive(Debug, Clone, Copy)]
pub struct ToolSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// It never changes anything.
    pub read_only: bool,
    /// It can take something out of a page (undoable, but the agent should know).
    pub destructive: bool,
    pub schema: fn() -> Arc<JsonObject>,
}

fn schema<T: JsonSchema + 'static>() -> Arc<JsonObject> {
    rmcp::handler::server::common::schema_for_type::<T>()
}

/// Every tool, in the order agents see them.
pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "list_workspaces",
        title: "List workspaces",
        description: "Lists the Drusite workspaces you can see, with their IDs and whether you \
                      may change them. Most people have one; the other tools pick it when you \
                      leave `workspace` out.",
        read_only: true,
        destructive: false,
        schema: schema::<NoArgs>,
    },
    ToolSpec {
        name: "list_pages",
        title: "List pages",
        description: "Lists the pages in a workspace as a tree (title and page ID), or only \
                      the pages under `parent`. Journal entries show their date.",
        read_only: true,
        destructive: false,
        schema: schema::<ListPages>,
    },
    ToolSpec {
        name: "read_page",
        title: "Read a page",
        description: "Reads a page as Markdown. The frontmatter holds its title and \
                      properties. Every block ends with its ID as a block reference \
                      (` ^01J…`): cite a block by it, and keep the references when you send the \
                      page back to update_page, so the blocks keep their identity.",
        read_only: true,
        destructive: false,
        schema: schema::<ReadPage>,
    },
    ToolSpec {
        name: "read_journal",
        title: "Read a journal entry",
        description: "Reads the journal entry for a day (YYYY-MM-DD, in the person's own time \
                      zone), as read_page does.",
        read_only: true,
        destructive: false,
        schema: schema::<ReadJournal>,
    },
    ToolSpec {
        name: "search",
        title: "Search",
        description: "Searches pages by their words, best matches first. Each result names \
                      the page and, where it can, the block (as in read_page's block \
                      references).",
        read_only: true,
        destructive: false,
        schema: schema::<Search>,
    },
    ToolSpec {
        name: "create_page",
        title: "Create a page",
        description: "Creates a page with a title and, optionally, Markdown content, at the \
                      top level or under `parent`. Returns its page ID.",
        read_only: false,
        destructive: false,
        schema: schema::<CreatePage>,
    },
    ToolSpec {
        name: "append_to_page",
        title: "Add to a page",
        description: "Adds Markdown to the end of a page. Nothing already there changes. The \
                      safest way to add notes.",
        read_only: false,
        destructive: false,
        schema: schema::<AppendToPage>,
    },
    ToolSpec {
        name: "update_page",
        title: "Edit a page",
        description: "Replaces a page's content with `markdown`, and/or renames it. Read the \
                      page first and edit what read_page gave you: blocks keep their identity \
                      by their ` ^…` references, and a block left out is removed. Frontmatter, \
                      if you send it, sets the page's properties (and title). Anything the \
                      person changed meanwhile is kept. The person can undo any of your edits.",
        read_only: false,
        destructive: true,
        schema: schema::<UpdatePage>,
    },
    ToolSpec {
        name: "publish_page",
        title: "Publish a page to the web",
        description: "Shares a page as a read-only link anyone can open (`publish: true`), \
                      changes how (with its subpages; findable by search engines), or takes it \
                      down (`publish: false`). Returns the link. Only works if the person \
                      allowed you to publish when connecting you; ask them first, since a \
                      published page is public.",
        read_only: false,
        destructive: false,
        schema: schema::<PublishPage>,
    },
];

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct NoArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ListPages {
    /// The workspace's ID; leave it out when there is one.
    pub(crate) workspace: Option<String>,
    /// Only the pages under this one.
    pub(crate) parent: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ReadPage {
    /// The page's ID (or a `drusite:page/…` link to it).
    pub(crate) page: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct PublishPage {
    /// The page's ID (or a `drusite:page/…` link to it).
    pub(crate) page: String,
    /// True to publish it (or keep it published with these options), false to take it down.
    pub(crate) publish: bool,
    /// Share the pages inside it too, as a small site.
    #[serde(default)]
    pub(crate) include_subpages: Option<bool>,
    /// Let search engines index it.
    #[serde(default)]
    pub(crate) allow_search_engines: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ReadJournal {
    /// The day, as YYYY-MM-DD.
    pub(crate) date: String,
    /// The workspace's ID; leave it out when there is one.
    pub(crate) workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct Search {
    /// The words to look for.
    pub(crate) query: String,
    /// The workspace's ID; leave it out to search every one you can see.
    pub(crate) workspace: Option<String>,
    /// At most this many results (10 if left out, at most 50).
    pub(crate) limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct CreatePage {
    pub(crate) title: String,
    /// The content, as Markdown.
    pub(crate) markdown: Option<String>,
    /// The page to put it under; leave it out for the top level.
    pub(crate) parent: Option<String>,
    /// The workspace's ID, for a top-level page; leave it out when there is one.
    pub(crate) workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct AppendToPage {
    /// The page's ID.
    pub(crate) page: String,
    /// What to add, as Markdown.
    pub(crate) markdown: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct UpdatePage {
    /// The page's ID.
    pub(crate) page: String,
    /// The whole new content, as Markdown (start from read_page's).
    pub(crate) markdown: Option<String>,
    /// A new title.
    pub(crate) title: Option<String>,
}

fn args<T: DeserializeOwned>(value: serde_json::Value) -> ToolResult<T> {
    // A call with no arguments at all may arrive as null.
    let value = if value.is_null() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        value
    };
    serde_json::from_value(value).map_err(|e| ToolError::Invalid(format!("bad arguments: {e}")))
}

/// Runs tool `name`. The text that comes back is what the agent reads.
pub async fn call<N: Notes>(
    notes: &N,
    agent: &Agent,
    name: &str,
    arguments: serde_json::Value,
) -> ToolResult<String> {
    let tools = Tools { notes, agent };
    match name {
        "list_workspaces" => tools.list_workspaces().await,
        "list_pages" => tools.list_pages(args(arguments)?).await,
        "read_page" => tools.read_page(args(arguments)?).await,
        "read_journal" => tools.read_journal(args(arguments)?).await,
        "search" => tools.search(args(arguments)?).await,
        "create_page" => tools.create_page(args(arguments)?).await,
        "append_to_page" => tools.append_to_page(args(arguments)?).await,
        "update_page" => tools.update_page(args(arguments)?).await,
        "publish_page" => tools.publish_page(args(arguments)?).await,
        other => Err(ToolError::NotFound(format!(
            "there is no tool called {other}"
        ))),
    }
}

/// Reads a page ID, bare or as a link.
fn page_id(s: &str) -> ToolResult<PageId> {
    let s = s.trim();
    let s = s.strip_prefix("drusite:page/").unwrap_or(s);
    s.parse()
        .map_err(|_| ToolError::Invalid(format!("{s:?} isn't a page ID")))
}

fn workspace_id(s: &str) -> ToolResult<WorkspaceId> {
    s.trim()
        .parse()
        .map_err(|_| ToolError::Invalid(format!("{s:?} isn't a workspace ID")))
}

fn quote(title: &str) -> String {
    if title.is_empty() {
        "an untitled page".to_owned()
    } else {
        format!("“{title}”")
    }
}

/// A page the agent may look at: where it is, and its place in the tree.
struct Located {
    info: WorkspaceInfo,
    ws: Workspace,
    node: PageNode,
}

struct Tools<'a, N> {
    notes: &'a N,
    agent: &'a Agent,
}

impl<N: Notes> Tools<'_, N> {
    async fn visible(&self) -> ToolResult<Vec<WorkspaceInfo>> {
        let mut all = self.notes.workspaces(self.agent).await?;
        all.retain(|w| self.agent.access.sees(w.id));
        Ok(all)
    }

    /// The workspace named, or the only one.
    async fn pick(&self, workspace: Option<&str>) -> ToolResult<WorkspaceInfo> {
        let visible = self.visible().await?;
        match workspace {
            Some(s) => {
                let id = workspace_id(s)?;
                visible
                    .into_iter()
                    .find(|w| w.id == id)
                    .ok_or_else(|| ToolError::NotFound("no such workspace".to_owned()))
            }
            None => match visible.len() {
                0 => Err(ToolError::NotFound(
                    "there are no workspaces you can see".to_owned(),
                )),
                1 => Ok(visible
                    .into_iter()
                    .next()
                    .ok_or_else(|| ToolError::Server("a workspace went missing".to_owned()))?),
                _ => Err(ToolError::Invalid(
                    "there is more than one workspace: say which (list_workspaces)".to_owned(),
                )),
            },
        }
    }

    async fn locate(&self, page: &str) -> ToolResult<Located> {
        let id = page_id(page)?;
        let missing = || ToolError::NotFound("no such page".to_owned());
        let ws_id = self
            .notes
            .find_page(self.agent, id)
            .await?
            .ok_or_else(missing)?;
        let info = self
            .visible()
            .await?
            .into_iter()
            .find(|w| w.id == ws_id)
            .ok_or_else(missing)?;
        let ws = self.notes.workspace(self.agent, ws_id).await?;
        // Pages in the trash are as good as gone to an agent.
        let node = ws
            .pages()?
            .into_iter()
            .find(|n| n.id == id)
            .ok_or_else(missing)?;
        Ok(Located { info, ws, node })
    }

    /// Refuses a change the agent may not make to `node` (or under it, for a new page).
    fn may_write(&self, info: &WorkspaceInfo, node: Option<&PageNode>) -> ToolResult<()> {
        if !self.agent.access.write {
            return Err(ToolError::Forbidden(
                "you were connected to read only; the person can give you write access in \
                 Drusite"
                    .to_owned(),
            ));
        }
        if !info.writable {
            return Err(ToolError::Forbidden(
                "the person can only read this workspace".to_owned(),
            ));
        }
        if node.is_some_and(|n| n.kind == PageKind::Journal) && !self.agent.access.journal {
            return Err(ToolError::Forbidden(
                "journal entries are read-only to agents unless the person allows more when \
                 connecting you"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    async fn publish_page(&self, a: PublishPage) -> ToolResult<String> {
        if !self.agent.access.publish {
            return Err(ToolError::Forbidden(
                "you weren't allowed to publish pages; the person can allow it when connecting \
                 you in Drusite"
                    .to_owned(),
            ));
        }
        let at = self.locate(&a.page).await?;
        self.may_write(&at.info, None)?;
        let request = if a.publish {
            crate::ShareRequest::Publish {
                include_subpages: a.include_subpages,
                allow_search_engines: a.allow_search_engines,
            }
        } else {
            crate::ShareRequest::Unpublish
        };
        self.notes
            .share(self.agent, at.info.id, at.node.id, request)
            .await
    }

    async fn list_workspaces(&self) -> ToolResult<String> {
        let mut out = String::new();
        for info in self.visible().await? {
            let ws = self.notes.workspace(self.agent, info.id).await?;
            let count = ws.pages()?.len();
            let access = if info.writable && self.agent.access.write {
                "read and write"
            } else {
                "read only"
            };
            out.push_str(&format!("- {} ({count} pages, {access})\n", info.id));
        }
        if out.is_empty() {
            out.push_str("There are no workspaces you can see.");
        }
        Ok(out)
    }

    async fn list_pages(&self, a: ListPages) -> ToolResult<String> {
        const MAX: usize = 2000;
        let (ws, under) = match &a.parent {
            Some(p) => {
                let at = self.locate(p).await?;
                (at.ws, Some(at.node.id))
            }
            None => {
                let info = self.pick(a.workspace.as_deref()).await?;
                (self.notes.workspace(self.agent, info.id).await?, None)
            }
        };
        let nodes = ws.pages()?;
        // Parents come before their children, so depth is the parent's plus one.
        let mut depth = std::collections::HashMap::new();
        let mut out = String::new();
        let mut shown = 0;
        for n in &nodes {
            let d = match (n.parent, under) {
                (Some(p), _) if Some(p) == under => 0,
                (Some(p), _) => match depth.get(&p) {
                    Some(d) => d + 1,
                    None => continue,
                },
                (None, None) => 0,
                (None, Some(_)) => continue,
            };
            depth.insert(n.id, d);
            shown += 1;
            if shown > MAX {
                out.push_str("… and more; list under a page to see them.\n");
                break;
            }
            let date = journal_date(n)
                .map(|d| format!(", journal {d}"))
                .unwrap_or_default();
            let title = if n.title.is_empty() {
                "Untitled"
            } else {
                &n.title
            };
            out.push_str(&format!(
                "{}- {title} (page {}{date})\n",
                "  ".repeat(d),
                n.id
            ));
        }
        if out.is_empty() {
            out.push_str("No pages.");
        }
        Ok(out)
    }

    async fn read_page(&self, a: ReadPage) -> ToolResult<String> {
        let at = self.locate(&a.page).await?;
        self.render(at.info.id, &at.node).await
    }

    async fn render(&self, ws: WorkspaceId, node: &PageNode) -> ToolResult<String> {
        let page = self.notes.page(self.agent, ws, node.id).await?;
        Ok(page_to_markdown(
            node.id,
            &node.title,
            node.kind,
            &node.properties,
            &page.blocks()?,
        ))
    }

    async fn read_journal(&self, a: ReadJournal) -> ToolResult<String> {
        let day: Day = a
            .date
            .trim()
            .parse()
            .map_err(|_| ToolError::Invalid("the date must be YYYY-MM-DD".to_owned()))?;
        let info = self.pick(a.workspace.as_deref()).await?;
        let pages = self.notes.workspace(self.agent, info.id).await?.pages()?;
        let Some(node) = pages
            .into_iter()
            .filter(|n| journal_date(n) == Some(day))
            .min_by_key(|n| n.id)
        else {
            return Ok(format!("There is no journal entry for {day}."));
        };
        self.render(info.id, &node).await
    }

    async fn search(&self, a: Search) -> ToolResult<String> {
        let limit = a.limit.unwrap_or(10).clamp(1, 50) as usize;
        let spaces = match &a.workspace {
            Some(w) => vec![self.pick(Some(w)).await?],
            None => self.visible().await?,
        };
        let mut out = String::new();
        let mut found = 0;
        for info in spaces {
            for hit in self
                .notes
                .search(self.agent, info.id, &a.query, limit - found)
                .await?
            {
                let block = hit
                    .block
                    .map(|b| format!(", block ^{}", encode_block_id(b)))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "- {} (page {}{block}): {}\n",
                    quote(&hit.title),
                    hit.page,
                    hit.snippet.replace('\n', " ")
                ));
                found += 1;
            }
            if found >= limit {
                break;
            }
        }
        if out.is_empty() {
            out.push_str("Nothing matches.");
        }
        Ok(out)
    }

    async fn create_page(&self, a: CreatePage) -> ToolResult<String> {
        let (info, mut ws, parent) = match &a.parent {
            Some(p) => {
                let at = self.locate(p).await?;
                self.may_write(&at.info, Some(&at.node))?;
                (at.info, at.ws, Some(at.node.id))
            }
            None => {
                let info = self.pick(a.workspace.as_deref()).await?;
                self.may_write(&info, None)?;
                let ws = self.notes.workspace(self.agent, info.id).await?;
                (info, ws, None)
            }
        };
        let ws_before = ws.version();
        let mut page = ws.create_page(parent, a.title.trim())?;
        let id = page.id();
        if let Some(md) = &a.markdown {
            let read = markdown::read(md, &[]);
            let mut wanted = markdown::wanted(&read);
            for w in &mut wanted {
                w.id = None;
            }
            if !wanted.is_empty() {
                merge::apply(&mut page, &wanted)?;
            }
            for p in &read.front.properties {
                ws.set_property(id, &p.name, Some(&p.value))?;
            }
        }
        let change = Change {
            created: vec![(id, page.export_snapshot()?)],
            pages: Vec::new(),
            workspace: Some(ws.export_updates(&ws_before)?),
        };
        let title = ws.title(id)?;
        let activity = Activity {
            tool: "create_page".to_owned(),
            summary: format!("Created {}", quote(&title)),
            effects: vec![Effect::Created { page: id }],
        };
        self.notes
            .commit(self.agent, info.id, change, activity)
            .await?;
        Ok(format!("Created {} (page {id}).", quote(&title)))
    }

    async fn append_to_page(&self, a: AppendToPage) -> ToolResult<String> {
        let at = self.locate(&a.page).await?;
        self.may_write(&at.info, Some(&at.node))?;
        let mut page = self.notes.page(self.agent, at.info.id, at.node.id).await?;
        let blocks = page.blocks()?;
        let read = markdown::read(&a.markdown, &blocks);
        let added = read.blocks.len();
        if added == 0 {
            return Err(ToolError::Invalid("there is nothing to add".to_owned()));
        }
        let mut wanted: Vec<Wanted> = blocks
            .iter()
            .map(|b| Wanted {
                id: Some(b.id),
                kind: b.kind.clone(),
                indent: b.indent,
                spans: b.spans.clone(),
            })
            .collect();
        // A page that is one empty paragraph is filled, not added to after a blank line.
        if blocks.len() == 1 && is_blank(&blocks[0].kind, &blocks[0].spans) {
            wanted.clear();
        }
        wanted.extend(markdown::wanted(&read).into_iter().map(|mut w| {
            w.id = None;
            w
        }));
        let before = page.version();
        merge::apply(&mut page, &wanted)?;
        let after = page.version();
        let change = Change {
            pages: vec![(at.node.id, page.export_updates(&before)?)],
            ..Change::default()
        };
        let what = if added == 1 {
            "a block".to_owned()
        } else {
            format!("{added} blocks")
        };
        let activity = Activity {
            tool: "append_to_page".to_owned(),
            summary: format!("Added {what} to {}", quote(&at.node.title)),
            effects: vec![Effect::Page {
                page: at.node.id,
                before: before.encode(),
                after: after.encode(),
            }],
        };
        self.notes
            .commit(self.agent, at.info.id, change, activity)
            .await?;
        Ok(format!("Added {what} to {}.", quote(&at.node.title)))
    }

    async fn update_page(&self, a: UpdatePage) -> ToolResult<String> {
        if a.markdown.is_none() && a.title.is_none() {
            return Err(ToolError::Invalid(
                "send markdown, a title, or both".to_owned(),
            ));
        }
        let mut at = self.locate(&a.page).await?;
        self.may_write(&at.info, Some(&at.node))?;
        let id = at.node.id;
        let mut change = Change::default();
        let mut effects = Vec::new();
        let mut title = a.title.as_deref().map(str::trim).map(str::to_owned);
        let mut properties = None;

        if let Some(md) = &a.markdown {
            let mut page = self.notes.page(self.agent, at.info.id, id).await?;
            let read = markdown::read(md, &page.blocks()?);
            let mut wanted = markdown::wanted(&read);
            if wanted.is_empty() {
                wanted.push(Wanted {
                    id: None,
                    kind: BlockKind::Paragraph,
                    indent: 0,
                    spans: Vec::new(),
                });
            }
            let before = page.version();
            merge::apply(&mut page, &wanted)?;
            let after = page.version();
            if after != before {
                change.pages.push((id, page.export_updates(&before)?));
                effects.push(Effect::Page {
                    page: id,
                    before: before.encode(),
                    after: after.encode(),
                });
            }
            if markdown::has_frontmatter(md) {
                properties = Some(read.front.properties.clone());
                if title.is_none() {
                    title = read.front.title.clone().map(|t| t.trim().to_owned());
                }
            }
        }

        let ws = &mut at.ws;
        let ws_before = ws.version();
        if let Some(t) = &title
            && *t != at.node.title
        {
            ws.set_title(id, t)?;
        }
        if let Some(props) = &properties {
            set_properties(ws, id, &at.node.properties, props)?;
        }
        let ws_after = ws.version();
        if ws_after != ws_before {
            change.workspace = Some(ws.export_updates(&ws_before)?);
            effects.push(Effect::Workspace {
                before: ws_before.encode(),
                after: ws_after.encode(),
            });
        }

        if change.is_empty() {
            return Ok("Nothing changed: the page already says that.".to_owned());
        }
        let name = quote(&at.node.title);
        let summary = match (
            &title,
            effects.iter().any(|e| matches!(e, Effect::Page { .. })),
        ) {
            (Some(t), false) if *t != at.node.title => {
                format!("Renamed {name} to {}", quote(t))
            }
            _ => format!("Edited {name}"),
        };
        let activity = Activity {
            tool: "update_page".to_owned(),
            summary: summary.clone(),
            effects,
        };
        self.notes
            .commit(self.agent, at.info.id, change, activity)
            .await?;
        Ok(format!("{summary}."))
    }
}

fn is_blank(kind: &BlockKind, spans: &[Span]) -> bool {
    *kind == BlockKind::Paragraph && spans.iter().all(|s| s.text.is_empty())
}

/// The day a journal entry is for.
fn journal_date(node: &PageNode) -> Option<Day> {
    if node.kind != PageKind::Journal {
        return None;
    }
    node.properties
        .iter()
        .find(|p| p.name == journal::DATE)
        .and_then(|p| p.value.as_day())
}

/// Makes a page's properties `wanted`, changing only what differs.
fn set_properties(
    ws: &mut Workspace,
    page: PageId,
    now: &[Property],
    wanted: &[Property],
) -> ToolResult<()> {
    for p in now {
        if !wanted.iter().any(|w| w.name == p.name) {
            ws.set_property(page, &p.name, None)?;
        }
    }
    for w in wanted {
        let same = now
            .iter()
            .any(|p| p.name == w.name && same_value(&p.value, &w.value));
        if !same {
            ws.set_property(page, &w.name, Some(&w.value))?;
        }
    }
    Ok(())
}

fn same_value(a: &PropertyValue, b: &PropertyValue) -> bool {
    a.compare(b) == std::cmp::Ordering::Equal
        && std::mem::discriminant(a) == std::mem::discriminant(b)
}
