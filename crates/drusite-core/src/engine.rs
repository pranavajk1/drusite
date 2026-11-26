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
        Ok(Self {
            store,
            workspace,
            workspace_at,
            open: HashMap::new(),
            events,
            sync: None,
            on_change: None,
        })
    }

    /// Tells the sync driver, if any, that `doc` changed locally.
    pub(crate) fn changed(&mut self, doc: uuid::Uuid) {
        self.mark_changed(doc);
        if let Some(notify) = &self.on_change {
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
                self.workspace.delete_page(page)?;
                self.open.remove(&page);
                self.save_workspace()?;
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
        }
    }

    pub(crate) fn pages(&self) -> Result<Vec<PageInfo>, ApiError> {
        Ok(self
            .workspace
            .pages()?
            .into_iter()
            .map(|n| PageInfo {
                id: n.id,
                parent: n.parent,
                title: n.title,
                children: n.children,
            })
            .collect())
    }

    pub(crate) fn save_workspace(&mut self) -> Result<(), ApiError> {
        self.store
            .save_workspace(&self.workspace, &mut self.workspace_at)?;
        self.changed(*self.workspace.id().as_uuid());
        (self.events)(Event::PagesChanged {
            pages: self.pages()?,
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
