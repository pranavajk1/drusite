// SPDX-License-Identifier: AGPL-3.0-or-later

//! The client engine: the encrypted local store, search, the sync client, and the Core API the
//! UI talks to. The Markdown bridge and the local MCP server join it later.

#[cfg(feature = "net")]
pub mod account;
pub mod api;
#[cfg(feature = "sqlite")]
pub mod attachments;
pub mod auth;
#[cfg(feature = "sqlite")]
pub mod bridge;
#[cfg(feature = "sqlite")]
mod engine;
#[cfg(feature = "sqlite")]
pub mod journal;
pub mod key;
#[cfg(feature = "sqlite")]
pub mod search;
#[cfg(feature = "sqlite")]
pub mod store;
#[cfg(feature = "sqlite")]
pub mod sync;
#[cfg(feature = "sqlite")]
mod trash;

#[cfg(feature = "sqlite")]
pub use engine::{Core, EventSink};
pub use key::StoreKey;

/// Everything that can go wrong in the core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the store key doesn't open this store (or the file isn't a Drusite store)")]
    WrongKey,
    #[error("store key: {0}")]
    Key(String),
    #[error(
        "the store was written by a newer build (format {found}, this build reads up to {supported})"
    )]
    NewerStore { found: i32, supported: i32 },
    #[error("no document {0} in the store")]
    UnknownDoc(String),
    #[error("the store is damaged: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Document(#[from] drusite_crdt::Error),
    #[cfg(feature = "sqlite")]
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
