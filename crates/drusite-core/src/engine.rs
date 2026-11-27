// SPDX-License-Identifier: AGPL-3.0-or-later

//! The engine behind the Core API: one workspace, its store, and the pages that are open.
//!
//! Every command that changes something saves it before answering, so a change the UI has
//! seen confirmed is on disk. The open page's Loro document lives here and nowhere else; the UI
//! holds a view of it.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use drusite_crdt::{Page, Workspace};
use drusite_model::{BlockId, PageId};

use crate::api::{
    ApiError, Command, Edit, EditOutcome, ErrorCode, Event, PageInfo, PagePatch, PageView,
    PlacedBlock, Response,
};
use crate::key::StoreKey;
use crate::store::{STORE_FILE, SavePoint, Store};
use crate::{Error, Result};

/// Where the core's events go: the platform adapter forwards them to the UI.
pub type EventSink = Box<dyn Fn(Event) + Send>;

/// The engine. One per workspace.
pub struct Core {
    pub(crate) store: Store,
    pub(crate) workspace: Workspace,
    pub(crate) workspace_at: SavePoint,
    pub(crate) open: HashMap<PageId, OpenPage>,
    pub(crate) events: EventSink,
    /// The connection to the server, while there is one.
    pub(crate) sync: Option<crate::sync::Session>,
    /// Called after every local change, so a sync driver knows to push.
    pub(crate) on_change: Option<Box<dyn Fn() + Send>>,
    pub(crate) search: crate::search::Search,
    /// Attachments the UI asked for that aren't here: the sync driver fetches them.
    pub(crate) wanted_blobs: HashSet<String>,
    /// Pages changed since the Markdown bridge last looked.
    pub(crate) bridge_dirty: HashSet<PageId>,
    /// Called when a page changes, so the bridge's watcher knows to run.
    pub(crate) on_bridge_change: Option<Box<dyn Fn() + Send>>,
    /// How the bridge's last round went.
    pub(crate) bridge_last: Option<crate::api::BridgeRound>,
}

pub(crate) struct OpenPage {
    pub(crate) page: Page,
    pub(crate) at: SavePoint,
    /// How many views have it open. A view that closes after another opened the same page (a
    /// quick switch away and back) mustn't close it under the newer one.
    views: u32,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core")
            .field("workspace", &self.workspace.id())
            .field("open", &self.open.len())
            .finish()
    }
}

impl Core {
    /// Opens the workspace stored in `data_dir`, creating the store and an empty workspace the
    /// first time.
    pub fn open(data_dir: &Path, key: &StoreKey, events: EventSink) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| Error::Corrupt(format!("can't create {}: {e}", data_dir.display())))?;
        let mut store = Store::open(&data_dir.join(STORE_FILE), key)?;
        let (workspace, workspace_at) = match store.workspace_id()? {
            Some(id) => store.load_workspace(id)?,
            None => {
                let ws = Workspace::create()?;
                let at = store.create_workspace(&ws)?;
                (ws, at)
            }
        };
        let mut core = Self {
            store,
            workspace,
            workspace_at,
            open: HashMap::new(),
            events,
            sync: None,
            on_change: None,
            search: crate::search::Search::default(),
            wanted_blobs: HashSet::new(),
            bridge_dirty: HashSet::new(),
            on_bridge_change: None,
            bridge_last: None,
        };
        core.purge_trash(crate::trash::now_ms())
            .map_err(|e| Error::Corrupt(format!("emptying old trash: {}", e.message)))?;
        Ok(core)
    }

    /// Tells the sync driver, if any, that `doc` changed locally.
    pub(crate) fn changed(&mut self, doc: uuid::Uuid) {
        self.search_changed(doc);
        self.mark_changed(doc);
        if let Some(notify) = &self.on_change {
            notify();
        }
    }

    /// Tells the search index that `doc` changed.
    pub(crate) fn search_changed(&mut self, doc: uuid::Uuid) {
        if doc == *self.workspace.id().as_uuid() {
            self.search.workspace_changed();
        } else {
            self.search.page_changed(PageId::from_uuid(doc));
            self.bridge_dirty.insert(PageId::from_uuid(doc));
        }
        if let Some(notify) = &self.on_bridge_change {
            notify();
        }
    }

    /// Runs one command.
    pub fn handle(&mut self, command: Command) -> Result<Response, ApiError> {
        match command {
            Command::ListPages => Ok(Response::Pages {
                pages: self.pages()?,
            }),
            Command::CreatePage { parent, title } => {
                let page = self.workspace.create_page(parent, &title)?;
                self.store.create_page(&page)?;
                self.save_workspace()?;
                let info = self
                    .pages()?
                    .into_iter()
                    .find(|p| p.id == page.id())
                    .ok_or_else(internal("page vanished"))?;
                Ok(Response::PageCreated { page: info })
            }
            Command::RenamePage { page, title } => {
                self.workspace.set_title(page, &title)?;
                self.save_workspace()?;
                self.row_touched(page);
                Ok(Response::Done)
            }
            Command::MovePage {
                page,
                parent,
                index,
            } => {
                self.workspace.move_page(page, parent, index as usize)?;
                self.save_workspace()?;
                Ok(Response::Done)
            }
            Command::DeletePage { page } => {
                self.workspace.trash_page(page, crate::trash::now_ms())?;
                self.save_workspace()?;
                Ok(Response::Done)
            }
            Command::Trash => Ok(Response::Trash {
                pages: self.trash()?,
            }),
            Command::RestorePage { page } => {
                self.workspace.restore_page(page)?;
                self.save_workspace()?;
                Ok(Response::Done)
            }
            Command::DeleteForever { page } => {
                if !self.workspace.trash()?.iter().any(|t| t.id == page) {
                    return Err(ApiError::new(
                        ErrorCode::Invalid,
                        "only a page in the trash can be deleted for good",
                    ));
                }
                self.delete_forever(&[page])?;
                Ok(Response::Done)
            }
            Command::AddAttachment { name, mime, data } => Ok(Response::Attachment {
                kind: self.add_attachment(&name, &mime, &data)?,
            }),
            Command::ReadAttachment { blob } => Ok(Response::AttachmentData {
                data: self.read_attachment(&blob)?,
            }),
            Command::BridgeStatus => Ok(Response::Bridge {
                status: self.bridge_status()?,
            }),
            Command::SetBridgeFolder { folder } => {
                let result = self.set_bridge_folder(folder.as_deref().map(Path::new));
                self.bridge_record(result.clone());
                result?;
                Ok(Response::Bridge {
                    status: self.bridge_status()?,
                })
            }
            Command::ImportFolder { folder } => Ok(Response::Imported {
                report: self.import_folder(Path::new(&folder))?,
            }),
            Command::EmptyTrash => {
                let all: Vec<PageId> = self.workspace.trash()?.iter().map(|t| t.id).collect();
                self.delete_forever(&all)?;
                Ok(Response::Done)
            }
            Command::OpenPage { page } => {
                let view = self.open_page(page)?;
                if let Some(open) = self.open.get_mut(&page) {
                    open.views += 1;
                }
                Ok(Response::Opened { view })
            }
            Command::ClosePage { page } => {
                if let Some(open) = self.open.get_mut(&page) {
                    open.views = open.views.saturating_sub(1);
                    if open.views == 0 {
                        self.open.remove(&page);
                    }
                }
                Ok(Response::Done)
            }
            Command::Edit { page, edits } => Ok(Response::Edited {
                outcome: self.edit(page, edits)?,
            }),
            Command::Ready => Ok(Response::Done),
            // The app handles these, since they need the browser and the device's key store.
            Command::AccountStatus => Ok(Response::Account {
                status: crate::api::AccountStatus::signed_out(None),
            }),
            Command::SignIn { .. } | Command::CancelSignIn | Command::SignOut => Err(
                ApiError::new(ErrorCode::Unsupported, "signing in needs the Drusite app"),
            ),
            Command::SetProperty { page, name, value } => {
                self.workspace.set_property(page, &name, value.as_ref())?;
                self.save_workspace()?;
                Ok(Response::Done)
            }
            Command::OpenJournalDay { date } => {
                let (page, created) = self.journal_day(date)?;
                Ok(Response::JournalDay { page, created })
            }
            Command::QueryPages { query } => Ok(Response::Pages {
                pages: self.query_pages(&query)?,
            }),
            Command::Settings => Ok(Response::Settings {
                settings: self.settings()?,
            }),
            Command::Search { query, limit } => Ok(Response::SearchResults {
                hits: self.search(&query, limit.min(100))?,
            }),
            #[cfg(feature = "mcp")]
            Command::Agents => self.agents(None),
            #[cfg(feature = "mcp")]
            Command::SetLocalAgents { enabled, port } => {
                self.set_local_agents(enabled, port)?;
                Ok(Response::Done)
            }
            #[cfg(feature = "mcp")]
            Command::CreateAgentToken { name, access } => self.create_agent_token(&name, access),
            #[cfg(feature = "mcp")]
            Command::RevokeAgentToken { id } => {
                self.revoke_agent_token(&id)?;
                Ok(Response::Done)
            }
            #[cfg(feature = "mcp")]
            Command::UndoAgentCall {
                id,
                source: crate::api::AgentSource::Device,
            } => {
                self.undo_agent_call(&id)?;
                Ok(Response::Done)
            }
            #[cfg(not(feature = "mcp"))]
            Command::Agents => Ok(Response::Agents {
                local: crate::api::LocalAgents {
                    available: false,
                    enabled: false,
                    port: crate::api::LOCAL_AGENTS_PORT,
                    url: String::new(),
                    problem: None,
                },
                tokens: Vec::new(),
                activity: Vec::new(),
                server_problem: None,
            }),
            #[cfg(not(feature = "mcp"))]
            Command::SetLocalAgents { .. }
            | Command::CreateAgentToken { .. }
            | Command::RevokeAgentToken { .. }
            | Command::UndoAgentCall { .. } => Err(ApiError::new(
                ErrorCode::Unsupported,
                "agents on this computer need the desktop app",
            )),
            #[cfg(feature = "mcp")]
            Command::UndoAgentCall { .. } => Err(ApiError::new(
                ErrorCode::Unsupported,
                "the server's agent activity is undone through the app",
            )),
            #[cfg(feature = "ai")]
            Command::Ai => self.ai(),
            #[cfg(feature = "ai")]
            Command::SaveAiProvider {
                id,
                kind,
                name,
                base_url,
                api_key,
            } => self.save_ai_provider(id, kind, &name, base_url, api_key),
            #[cfg(feature = "ai")]
            Command::DeleteAiProvider { id } => {
                self.delete_ai_provider(&id)?;
                Ok(Response::Done)
            }
            #[cfg(feature = "ai")]
            Command::SetAiFeature { feature, choice } => {
                self.set_ai_feature(feature, choice)?;
                Ok(Response::Done)
            }
            #[cfg(feature = "ai")]
            Command::AiModels { .. } | Command::TestAiProvider { .. } => Err(ApiError::new(
                ErrorCode::Unsupported,
                "reaching a provider goes through drusite_core::ai::run",
            )),
            #[cfg(not(feature = "ai"))]
            Command::Ai => Ok(Response::Ai {
                available: false,
                kinds: Vec::new(),
                providers: Vec::new(),
                features: Vec::new(),
            }),
            #[cfg(not(feature = "ai"))]
            Command::SaveAiProvider { .. }
            | Command::DeleteAiProvider { .. }
            | Command::SetAiFeature { .. }
            | Command::AiModels { .. }
            | Command::TestAiProvider { .. } => Err(ApiError::new(
                ErrorCode::Unsupported,
                "AI providers need the app",
            )),
            // Ask your notes goes to the server, through the app's connection.
            Command::AskStatus => Ok(Response::AskStatus {
                status: crate::api::AskStatus {
                    signed_in: false,
                    available: false,
                    message: Some("Sign in to a Drusite server to ask your notes.".to_owned()),
                    reason: Some("not_signed_in".to_owned()),
                    model: None,
                    semantic: false,
                    embedding_model: None,
                    indexing: 0,
                },
            }),
            Command::Conversations => Ok(Response::Conversations {
                conversations: Vec::new(),
            }),
            Command::Conversation { .. }
            | Command::RenameConversation { .. }
            | Command::DeleteConversation { .. }
            | Command::Ask { .. }
            | Command::CancelAsk { .. } => Err(ApiError::new(
                ErrorCode::Unsupported,
                "Ask your notes needs a Drusite server, through the app",
            )),
            Command::LocalShare { page } => Ok(Response::Share {
                share: self.local_share(page)?,
            }),
            // Without the app's connection there's no server to ask: what the workspace heard.
            Command::Share { page } => {
                let mut share = self.local_share(page)?;
                share.message = Some("Sign in to a Drusite server to publish pages.".to_owned());
                Ok(Response::Share { share })
            }
            Command::Publish { .. }
            | Command::Unpublish { .. }
            | Command::RegenerateShareLink { .. } => Err(ApiError::new(
                ErrorCode::Unsupported,
                "publishing to the web needs a Drusite server, through the app",
            )),
            Command::CreateDatabase {
                parent,
                title,
                inline,
                template,
            } => {
                let (page, schema) = self.create_database(parent, &title, inline, template)?;
                Ok(Response::DatabaseCreated { page, schema })
            }
            Command::Database { database } => Ok(Response::Database {
                schema: self.workspace.schema(database)?,
            }),
            Command::QueryView {
                database,
                view,
                clock,
            } => Ok(Response::View {
                result: self.query_view(database, view.as_deref(), clock)?,
            }),
            Command::RowCells { row, clock } => {
                let (schema, row) = match self.row_cells(row, clock)? {
                    Some((s, r)) => (Some(s), Some(r)),
                    None => (None, None),
                };
                Ok(Response::RowCells { schema, row })
            }
            Command::PutProperty {
                database,
                property,
                twin_name,
            } => Ok(Response::Database {
                schema: self.put_property(database, property, twin_name)?,
            }),
            Command::DeleteProperty { database, property } => Ok(Response::Database {
                schema: self.delete_property(database, &property)?,
            }),
            Command::PutView { database, view } => Ok(Response::Database {
                schema: self.put_view(database, &view)?,
            }),
            Command::DeleteView { database, view } => Ok(Response::Database {
                schema: self.delete_view(database, &view)?,
            }),
            Command::SetCell {
                row,
                property,
                value,
            } => {
                self.set_cell(row, &property, value)?;
                Ok(Response::Done)
            }
            Command::CreateRow {
                database,
                title,
                values,
                after,
            } => Ok(Response::PageCreated {
                page: self.create_row(
                    database,
                    &title,
                    values.into_iter().map(|c| (c.property, c.value)).collect(),
                    after,
                )?,
            }),
            Command::CheckFormula {
                database,
                expression,
            } => Ok(Response::FormulaCheck {
                check: self.check_formula(database, &expression)?,
            }),
            Command::ImportNotion { path } => Ok(Response::NotionImported {
                report: self.import_notion_path(Path::new(&path))?,
            }),
            Command::ImportNotionData { name, data } => Ok(Response::NotionImported {
                report: self.import_notion_data(&name, &data)?,
            }),
            // These need the network and the device's key store: the app runs them.
            Command::ImportNotionApi { .. } | Command::NotionToken | Command::ForgetNotionToken => {
                Err(ApiError::new(
                    ErrorCode::Unsupported,
                    "importing from Notion's API needs the Drusite app",
                ))
            }
            Command::SetSetting { setting } => {
                self.set_setting(setting)?;
                Ok(Response::Settings {
                    settings: self.settings()?,
                })
            }
        }
    }

    /// Sends an event to the UI.
    pub fn emit(&self, event: Event) {
        (self.events)(event);
    }

    pub(crate) fn pages(&self) -> Result<Vec<PageInfo>, ApiError> {
        let shared: std::collections::HashSet<PageId> =
            self.workspace.shared_pages().into_iter().collect();
        Ok(self
            .workspace
            .pages()?
            .into_iter()
            .map(|n| PageInfo {
                published: shared.contains(&n.id),
                id: n.id,
                parent: n.parent,
                title: n.title,
                children: n.children,
                kind: n.kind,
                properties: n.properties,
            })
            .collect())
    }

    /// How a page is shared, as the workspace document last heard from the server.
    pub fn local_share(&self, page: PageId) -> Result<crate::api::ShareInfo, ApiError> {
        let mirror = self.workspace.share(page)?;
        let now = crate::trash::now_ms();
        Ok(match mirror {
            Some(m) => crate::api::ShareInfo {
                published: true,
                url: Some(m.url),
                include_subpages: m.include_subpages,
                allow_indexing: m.allow_indexing,
                expired: m.expires_at.is_some_and(|e| e <= now),
                expires_at: m.expires_at,
                has_password: m.has_password,
                can_publish: false,
                available: false,
                message: None,
            },
            None => crate::api::ShareInfo {
                published: false,
                url: None,
                include_subpages: false,
                allow_indexing: false,
                expires_at: None,
                expired: false,
                has_password: false,
                can_publish: false,
                available: false,
                message: None,
            },
        })
    }

    pub(crate) fn save_workspace(&mut self) -> Result<(), ApiError> {
        self.store
            .save_workspace(&self.workspace, &mut self.workspace_at)?;
        self.changed(*self.workspace.id().as_uuid());
        self.announce_workspace()
    }

    /// Tells the UI what the workspace document now holds: the page tree and the settings.
    pub(crate) fn announce_workspace(&self) -> Result<(), ApiError> {
        (self.events)(Event::PagesChanged {
            pages: self.pages()?,
        });
        (self.events)(Event::SettingsChanged {
            settings: self.settings()?,
        });
        (self.events)(Event::TrashChanged {
            pages: self.trash()?,
        });
        Ok(())
    }

    fn open_page(&mut self, id: PageId) -> Result<PageView, ApiError> {
        if !self.open.contains_key(&id) {
            // Only pages in the tree can be opened.
            self.workspace.title(id)?;
            let (page, at) = self.store.load_page(id)?;
            self.open.insert(id, OpenPage { page, at, views: 0 });
        }
        self.view(id)
    }

    fn view(&self, id: PageId) -> Result<PageView, ApiError> {
        let open = self.open.get(&id).ok_or_else(not_open(id))?;
        Ok(PageView {
            id,
            title: self.workspace.title(id)?,
            blocks: open.page.blocks()?,
        })
    }

    fn edit(&mut self, id: PageId, edits: Vec<Edit>) -> Result<EditOutcome, ApiError> {
        let open = self.open.get_mut(&id).ok_or_else(not_open(id))?;
        let mut touched: Vec<BlockId> = Vec::new();
        let mut removed: Vec<BlockId> = Vec::new();
        let mut created = Vec::new();
        let mut failure = None;
        for edit in edits {
            match apply(&mut open.page, edit) {
                Ok((patch, new)) => {
                    touched.extend(patch.updated);
                    removed.extend(patch.removed);
                    created.extend(new);
                }
                Err(err) => {
                    failure = Some(err);
                    break;
                }
            }
        }

        if matches!(failure, Some(drusite_crdt::Error::Poisoned)) {
            // Loro failed inside this page: drop it and reload what was last saved.
            let reason = failure.map(|e| e.to_string()).unwrap_or_default();
            let views = self.open.remove(&id).map_or(0, |o| o.views);
            let view = self.open_page(id)?;
            if let Some(open) = self.open.get_mut(&id) {
                open.views = views;
            }
            (self.events)(Event::PageReloaded {
                view,
                reason: reason.clone(),
            });
            return Err(ApiError::new(ErrorCode::Reloaded, reason));
        }

        // Save whatever was applied, even if a later edit in the batch failed.
        let open = self.open.get_mut(&id).ok_or_else(not_open(id))?;
        self.store.save_page(&open.page, &mut open.at)?;
        let patch = patch_for(&open.page, &touched, &removed)?;
        self.changed(*id.as_uuid());
        self.row_touched(id);
        match failure {
            None => Ok(EditOutcome { created, patch }),
            Some(err) => {
                // The UI applied the whole batch; tell it what the page really holds.
                if !patch.is_empty() {
                    (self.events)(Event::PageChanged { page: id, patch });
                }
                Err(err.into())
            }
        }
    }
}

/// Runs one edit, returning what it changed and any block it created.
fn apply(
    page: &mut Page,
    edit: Edit,
) -> drusite_crdt::Result<(drusite_crdt::Patch, Option<BlockId>)> {
    let pos = |p: u32| p as usize;
    Ok(match edit {
        Edit::InsertText {
            block,
            pos: p,
            text,
        } => (page.insert_text(block, pos(p), &text)?, None),
        Edit::DeleteText { block, pos: p, len } => {
            (page.delete_text(block, pos(p), pos(len))?, None)
        }
        Edit::Format {
            block,
            start,
            end,
            mark,
            on,
        } => (page.format(block, pos(start), pos(end), &mark, on)?, None),
        Edit::Split {
            block,
            pos: p,
            kind,
        } => {
            let (new, patch) = page.split(block, pos(p), kind)?;
            (patch, Some(new))
        }
        Edit::InsertBlock { after, kind } => {
            let (new, patch) = page.insert_block_after(after, kind)?;
            (patch, Some(new))
        }
        Edit::Join { block } => (page.join(block)?.1, None),
        Edit::DeleteBlock { block } => (page.delete_block(block)?, None),
        Edit::SetKind { block, kind } => (page.set_kind(block, &kind)?, None),
        Edit::SetIndent { block, indent } => (page.set_indent(block, indent)?, None),
        Edit::MoveBlock { block, after } => (page.move_block(block, after)?, None),
    })
}

/// The final state of every block a batch touched, in display order, and the ones it removed.
pub(crate) fn patch_for(
    page: &Page,
    touched: &[BlockId],
    removed: &[BlockId],
) -> Result<PagePatch, ApiError> {
    let mut seen = HashSet::new();
    let mut shown: Vec<(usize, BlockId)> = touched
        .iter()
        .filter(|id| seen.insert(**id))
        .filter_map(|&id| page.position(id).map(|p| (p, id)))
        .collect();
    shown.sort_unstable();
    let upserted = shown
        .into_iter()
        .map(|(_, id)| {
            Ok(PlacedBlock {
                block: page.block(id)?,
                after: page.block_before(id),
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let mut gone = HashSet::new();
    let removed = removed
        .iter()
        .copied()
        .filter(|&id| page.position(id).is_none() && gone.insert(id))
        .collect();
    Ok(PagePatch {
        upserted,
        removed,
        order: None,
    })
}

fn not_open(id: PageId) -> impl FnOnce() -> ApiError {
    move || ApiError::new(ErrorCode::PageNotOpen, format!("page {id} isn't open"))
}

fn internal(what: &'static str) -> impl FnOnce() -> ApiError {
    move || ApiError::new(ErrorCode::Internal, what)
}
