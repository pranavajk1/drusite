// SPDX-License-Identifier: AGPL-3.0-or-later

use drusite_model::{BlockId, PageId};

/// Everything that can go wrong editing a document. Nothing here leaves a document half-written:
/// positions and IDs are checked before anything is written to Loro.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no block {0} on this page")]
    UnknownBlock(BlockId),
    #[error("no page {0} in this workspace")]
    UnknownPage(PageId),
    #[error("position {pos} is past the end of the block ({len} UTF-16 units)")]
    OutOfRange { pos: usize, len: usize },
    #[error("position {0} falls inside a character (between the halves of a surrogate pair)")]
    InsideCharacter(usize),
    #[error("a page can't be moved under itself or one of its own subpages")]
    Cycle,
    #[error("not a Drusite {expected} document")]
    NotADocument { expected: &'static str },
    #[error("document schema {found} is newer than this build understands ({supported})")]
    NewerSchema { found: u32, supported: u32 },
    /// The changes belong to another document. The page has been poisoned: reload it.
    #[error("those changes belong to a different document")]
    WrongDocument,
    /// Loro panicked. The document can't be trusted any more and must be reloaded from its last
    /// saved state; every later call returns this error until then.
    #[error("the document is unusable after an internal error and must be reloaded")]
    Poisoned,
    #[error("Loro: {0}")]
    Loro(String),
}

impl From<loro::LoroError> for Error {
    fn from(err: loro::LoroError) -> Self {
        Self::Loro(err.to_string())
    }
}

impl From<loro::LoroEncodeError> for Error {
    fn from(err: loro::LoroEncodeError) -> Self {
        Self::Loro(err.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
