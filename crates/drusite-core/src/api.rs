// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Core API: the commands the UI sends, the responses it gets back, and the events the core
//! pushes to it.
//!
//! This is the UI's only way into the core. Each platform carries it its own way (Tauri's IPC
//! in `drusite-app`, direct calls in `drusite-wasm`), and the UI never knows which. The
//! TypeScript types are generated from these definitions (`cargo xtask bindings`), so the two
//! sides can't drift.
//!
//! Positions are UTF-16 code units within one block's text, as in the browser.

use drusite_model::{Block, BlockId, BlockKind, Mark, PageId};
use serde::{Deserialize, Serialize};
use specta::Type;

/// Something the UI asks the core to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Command {
    /// The page tree. Answered with [`Response::Pages`].
    ListPages,
    /// Answered with [`Response::PageCreated`].
    CreatePage {
        parent: Option<PageId>,
        title: String,
    },
    RenamePage {
        page: PageId,
        title: String,
    },
    /// Moves a page to position `index` among `parent`'s children (or the top-level pages).
    MovePage {
        page: PageId,
        parent: Option<PageId>,
        index: u32,
    },
    DeletePage {
        page: PageId,
    },
    /// Loads a page for editing. Answered with [`Response::Opened`]. Each open is matched by a
    /// [`Command::ClosePage`]; the page stays open until every view that opened it has closed it.
    OpenPage {
        page: PageId,
    },
    ClosePage {
        page: PageId,
    },
    /// Applies edits to an open page, in order, and saves them. Answered with
    /// [`Response::Edited`].
    Edit {
        page: PageId,
        edits: Vec<Edit>,
    },
    /// The UI has loaded and is showing what it opened. Benchmarks wait for this.
    Ready,
}

/// One change to a page, as the editor makes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Edit {
    InsertText {
        block: BlockId,
        pos: u32,
        text: String,
    },
    DeleteText {
        block: BlockId,
        pos: u32,
        len: u32,
    },
    Format {
        block: BlockId,
        start: u32,
        end: u32,
        mark: Mark,
        on: bool,
    },
    /// Enter: the text after `pos` moves to a new block of kind `kind`.
    Split {
        block: BlockId,
        pos: u32,
        kind: BlockKind,
    },
    /// A new empty block after `after`, or at the top.
    InsertBlock {
        after: Option<BlockId>,
        kind: BlockKind,
    },
    /// Backspace at the start of a block: join it onto the block before it.
    Join {
        block: BlockId,
    },
    DeleteBlock {
        block: BlockId,
    },
    SetKind {
        block: BlockId,
        kind: BlockKind,
    },
    SetIndent {
        block: BlockId,
        indent: u8,
    },
    /// Moves a block to just after `after`, or to the top.
    MoveBlock {
        block: BlockId,
        after: Option<BlockId>,
    },
}

/// The core's answer to a [`Command`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Response {
    Pages { pages: Vec<PageInfo> },
    PageCreated { page: PageInfo },
    Opened { view: PageView },
    Edited { outcome: EditOutcome },
    Done,
}

/// Something that happened in the core that the UI didn't ask for, or that other views of the
/// same data need to hear about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    /// The page tree or a title changed.
    PagesChanged { pages: Vec<PageInfo> },
    /// An open page changed other than through the edits just answered (another device, or the
    /// part of a batch that was applied before one edit failed).
    PageChanged { page: PageId, patch: PagePatch },
    /// An open page had to be reloaded from its last saved state (after an internal error).
    /// The editor should replace what it shows with `view`.
    PageReloaded { view: PageView, reason: String },
}

/// A page's place in the tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub id: PageId,
    pub parent: Option<PageId>,
    pub title: String,
    pub children: Vec<PageId>,
}

/// An open page: everything the editor needs to show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PageView {
    pub id: PageId,
    pub title: String,
    /// In display order.
    pub blocks: Vec<Block>,
}

/// What a batch of edits did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditOutcome {
    /// The blocks the edits created (splits and inserted blocks), in the order they were made.
    pub created: Vec<BlockId>,
    pub patch: PagePatch,
}

/// The blocks that changed, and only those. Apply `removed`, then `upserted` in order: each
/// block goes right after its `after` block (or first, for `None`). When `order` is set, the
/// change can't be described block by block: show the blocks in that order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PagePatch {
    pub upserted: Vec<PlacedBlock>,
    pub removed: Vec<BlockId>,
    pub order: Option<Vec<BlockId>>,
}

impl PagePatch {
    pub fn is_empty(&self) -> bool {
        self.upserted.is_empty() && self.removed.is_empty() && self.order.is_none()
    }
}

/// A block and the block shown before it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlacedBlock {
    pub block: Block,
    pub after: Option<BlockId>,
}

/// Why a command failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type, thiserror::Error)]
#[serde(rename_all = "camelCase")]
#[error("{message}")]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    UnknownPage,
    UnknownBlock,
    /// A position past the end of a block, or inside a character.
    InvalidPosition,
    /// The command needs the page open first.
    PageNotOpen,
    /// A page can't move under itself.
    Cycle,
    /// The page was reloaded from its last saved state; the edit was not applied.
    Reloaded,
    /// Reading or writing the local store failed.
    Storage,
    Internal,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<drusite_crdt::Error> for ApiError {
    fn from(err: drusite_crdt::Error) -> Self {
        use drusite_crdt::Error as E;
        let code = match &err {
            E::UnknownBlock(_) => ErrorCode::UnknownBlock,
            E::UnknownPage(_) => ErrorCode::UnknownPage,
            E::OutOfRange { .. } | E::InsideCharacter(_) => ErrorCode::InvalidPosition,
            E::Cycle => ErrorCode::Cycle,
            E::Poisoned => ErrorCode::Reloaded,
            _ => ErrorCode::Internal,
        };
        Self::new(code, err.to_string())
    }
}

impl From<crate::Error> for ApiError {
    fn from(err: crate::Error) -> Self {
        match err {
            crate::Error::Document(e) => e.into(),
            crate::Error::UnknownDoc(_) => Self::new(ErrorCode::UnknownPage, err.to_string()),
            other => Self::new(ErrorCode::Storage, other.to_string()),
        }
    }
}
