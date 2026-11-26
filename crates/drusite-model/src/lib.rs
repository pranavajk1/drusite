// SPDX-License-Identifier: AGPL-3.0-or-later

//! Drusite's data model, independent of how it is stored: IDs, the block kinds a page is made
//! of, inline formatting, and the schema version.
//!
//! `drusite-crdt` stores this model in Loro documents; the Core API, the editor and the Markdown
//! bridge all speak it.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

/// The version of the page and workspace document layout. Stored in every document, so a newer
/// build can migrate an older document and an older build can refuse a newer one.
pub const SCHEMA_VERSION: u32 = 1;

/// How deep blocks can nest (list items, toggle children). Deeper indents are clamped.
pub const MAX_INDENT: u8 = 8;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        ///
        /// Serialized as the UUID's hyphenated string.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Type)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// A new ID: a UUIDv7, so IDs sort by creation time.
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0.hyphenated())
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, IdError> {
                Uuid::parse_str(s).map(Self).map_err(|_| IdError(s.to_owned()))
            }
        }
    };
}

id_type!(
    /// A block, unique across every page and device.
    BlockId
);
id_type!(
    /// A page.
    PageId
);
id_type!(
    /// A workspace: one page tree and the pages in it.
    WorkspaceId
);

/// A string that isn't an ID.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not an ID: {0:?}")]
pub struct IdError(pub String);

/// Heading levels 1 to 3, as in Notion. Serialized as `"h1"`, `"h2"`, `"h3"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum HeadingLevel {
    H1,
    H2,
    H3,
}

impl HeadingLevel {
    pub const fn number(self) -> u8 {
        match self {
            Self::H1 => 1,
            Self::H2 => 2,
            Self::H3 => 3,
        }
    }

    /// The level for a number, clamped into 1–3.
    pub const fn clamped(n: i64) -> Self {
        match n {
            ..=1 => Self::H1,
            2 => Self::H2,
            _ => Self::H3,
        }
    }
}

/// What a block is. Changing it is an attribute change on the same block, never a new block, so
/// a concurrent edit to the block's text survives.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BlockKind {
    Paragraph,
    Heading {
        level: HeadingLevel,
    },
    Bulleted,
    Numbered,
    Todo {
        checked: bool,
    },
    Quote,
    /// The block's text is the code; inline formatting is ignored.
    Code {
        language: String,
    },
    /// Has no text of its own; any text it gets (from a concurrent edit) is kept, not shown.
    Divider,
    Callout {
        icon: String,
    },
    /// Its children are the blocks after it with a deeper indent.
    Toggle,
}

impl BlockKind {
    /// The name stored in documents and used by the Core API.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::Heading { .. } => "heading",
            Self::Bulleted => "bulleted",
            Self::Numbered => "numbered",
            Self::Todo { .. } => "todo",
            Self::Quote => "quote",
            Self::Code { .. } => "code",
            Self::Divider => "divider",
            Self::Callout { .. } => "callout",
            Self::Toggle => "toggle",
        }
    }
}

/// Inline formatting on a run of text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub link: Option<String>,
}

impl Marks {
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

/// One mark, for adding or removing formatting on a range.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Mark {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    Code,
    Link { href: String },
}

impl Mark {
    /// The key a mark is stored under.
    pub const fn key(&self) -> &'static str {
        match self {
            Self::Bold => "bold",
            Self::Italic => "italic",
            Self::Underline => "underline",
            Self::Strikethrough => "strikethrough",
            Self::Code => "code",
            Self::Link { .. } => "link",
        }
    }
}

/// A run of text with the same formatting.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct Span {
    pub text: String,
    pub marks: Marks,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            marks: Marks::default(),
        }
    }
}

/// A block as the editor sees it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct Block {
    pub id: BlockId,
    pub kind: BlockKind,
    /// Nesting depth. Never more than one deeper than the block before it.
    pub indent: u8,
    pub spans: Vec<Span>,
}

impl Block {
    /// The block's text without formatting.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_strings() {
        let id = BlockId::new();
        assert_eq!(id.to_string().parse::<BlockId>().unwrap(), id);
        assert!("not-an-id".parse::<BlockId>().is_err());
    }

    #[test]
    fn ids_are_v7_and_sort_by_creation() {
        let a = PageId::new();
        let b = PageId::new();
        assert_eq!(a.as_uuid().get_version_num(), 7);
        assert!(a < b);
    }

    #[test]
    fn heading_levels_clamp() {
        assert_eq!(HeadingLevel::clamped(0), HeadingLevel::H1);
        assert_eq!(HeadingLevel::clamped(2), HeadingLevel::H2);
        assert_eq!(HeadingLevel::clamped(9), HeadingLevel::H3);
    }
}
