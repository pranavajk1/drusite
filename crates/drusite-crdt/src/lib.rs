// SPDX-License-Identifier: AGPL-3.0-or-later

//! Drusite's block model on Loro: one [`Page`] document per page, plus one [`Workspace`]
//! document for the page tree.
//!
//! # How a page is stored
//!
//! Phase 0 showed that a page built as "one Loro map per block, each with its own text" can't
//! split a block without copying the tail of its text into a new block. Copies lose their
//! identity, so two devices splitting the same paragraph duplicate text, and text typed after the
//! split point on another device lands in the wrong block. A page is therefore stored in four
//! root containers:
//!
//! - `body` (text): the text of every block, in one `LoroText`. Each block starts with a marker
//!   character ([`MARKER`]) that carries the block's ID as a mark. Splitting a block inserts a
//!   marker; joining two deletes one. Characters never move between containers, so concurrent
//!   splits, joins and typing all land where the person meant them.
//! - `blocks` (map): each block's attributes, keyed by its ID (type, heading level, checked,
//!   language, icon, indent). Changing a paragraph into a heading is an attribute change.
//! - `order` (movable list): the display order of the blocks that have been moved. Moving a block
//!   is a Loro move, so concurrent moves never conflict and text typed into a moved block on
//!   another device follows it.
//! - `meta` (map): the schema version, the page's ID and its first block.
//!
//! Display order, in [`layout`]: the blocks listed in `order` ("anchors") appear in that order;
//! every other block appears right after the block before it in `body`. A block that has never
//! been moved therefore sits where its text sits, which is what makes a split land right after
//! the block it came from even when two devices split at once.
//!
//! # Rules from Phase 0
//!
//! - **A page is created once**, by [`Page::create`], which writes its first block. Every other
//!   copy comes from [`Page::load`]. There is no way to set up the same page twice.
//! - **Positions are checked before writing**: [`Error::OutOfRange`], [`Error::InsideCharacter`].
//! - **A Loro panic doesn't take the app down**: it is caught, the page reports
//!   [`Error::Poisoned`], and its owner reloads it from the last saved state.
//! - **Nothing typed is ever lost** to a merge. Text can only disappear when someone deletes it.

mod attrs;
mod error;
mod guard;
mod layout;
pub mod merge;
mod page;
mod workspace;

pub use error::{Error, Result};
pub use page::{JoinOutcome, Page, Patch};
pub use workspace::{PageNode, SettingValue, ShareMirror, TrashedPage, Workspace};

/// The character that starts every block in a page's `body` text. From Unicode's private use
/// area; text written to a page has any copies of it replaced with U+FFFD first.
pub const MARKER: char = '\u{E000}';

/// Loro's version vector, re-exported for saving and syncing changes since a known state.
pub use loro::VersionVector;

/// The versions a Loro update or snapshot reaches, without importing it: what a device can mark
/// as known to whoever sent it.
pub fn blob_version(bytes: &[u8]) -> Result<VersionVector> {
    Ok(loro::LoroDoc::decode_import_blob_meta(bytes, true)?.partial_end_vv)
}
