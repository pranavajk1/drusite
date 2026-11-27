// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pages as Markdown, and Markdown as pages: the dialect the Markdown bridge writes and reads,
//! and what importing a folder of notes reads.
//!
//! A page written here and read back is the same page: the same blocks, with the same IDs,
//! kinds, nesting and formatting. That rests on a few conventions, each one there because a
//! property test found the case:
//!
//! - **Every block ends with its ID** as an Obsidian block reference, ` ^id`, or on a line of
//!   its own after a block that can't hold one (code, a divider, a filter). Obsidian links to
//!   blocks the same way, and other tools show it as plain text.
//! - **Nesting** follows Markdown where it can: under a list item by indenting, under a quote,
//!   callout or toggle inside the `>`. A block nested under one that Markdown can't nest under
//!   (a paragraph, say) is written at the top level after `<!-- indent N -->`.
//! - **Escapes** for text Markdown would otherwise read as syntax, `<br>` for a line break inside
//!   a block, `&#32;` for spaces at a block's edges, and `<b>`/`<i>` for emphasis that
//!   CommonMark's rules won't close. Ordinary prose needs none of them.
//! - **Callouts and toggles** are Obsidian callouts: `> [!tip] text`, `> [!toggle]- text`.
//!   **Filter blocks** are fenced as `drusite-query`. **Attachments** are images, `![caption](file)`.
//! - **Page facts** go in YAML frontmatter: the page's properties by name, and `drusite-id`,
//!   `drusite-kind` and `drusite-title` (when the file name can't carry the title).
//!
//! Hand-written Markdown is a bigger language than a page. Reading it keeps everything as text
//! somewhere; what has no block of its own (tables, footnotes, headings below level 3) is
//! flattened, as listed on [`read`].

mod front;
mod ids;
mod read;
mod write;

use drusite_model::{BlockId, BlockKind, Span};

pub use front::{Frontmatter, split_frontmatter};
pub use ids::{decode_block_id, encode_block_id};
pub use read::{normalise_spans, read};
pub use write::write;

/// A block as a file holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct MdBlock {
    /// The block reference at its end, if it had one.
    pub id: Option<BlockRef>,
    pub kind: BlockKind,
    pub indent: u8,
    pub spans: Vec<Span>,
}

impl MdBlock {
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// A block reference (`^ref`) as written.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BlockRef {
    /// One of Drusite's own block IDs.
    Block(BlockId),
    /// Any other, such as one Obsidian made (`^37066d`): kept so links to it can be resolved.
    Other(String),
}

/// A page read from a file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MdPage {
    pub front: Frontmatter,
    pub blocks: Vec<MdBlock>,
}

/// What links and attachments become in a file, and what they mean when read back. The
/// default keeps link targets as they are and has no attachments.
pub trait Resolver {
    /// Where a link mark's `href` points, as written in the file.
    fn link_out(&self, href: &str) -> String {
        href.to_owned()
    }

    /// The `href` for a link in the file: an ordinary one (`[text](dest)`) or a wikilink
    /// (`[[dest]]`, `dest` being what's inside, before any `|`).
    fn link_in(&self, dest: &str, wiki: bool) -> String {
        let _ = wiki;
        dest.to_owned()
    }

    /// Where an attachment is, as written in the file.
    fn attachment_out(&self, blob: &str, name: &str) -> String {
        let _ = blob;
        name.to_owned()
    }

    /// The attachment an embedded file is (`![..](dest)` or `![[dest]]`), if it is one; an
    /// embed that isn't stays a link in the text.
    fn attachment_in(&self, dest: &str, wiki: bool) -> Option<BlockKind> {
        let _ = (dest, wiki);
        None
    }
}

/// The resolver that changes nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct Plain;

impl Resolver for Plain {}
