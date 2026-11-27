// SPDX-License-Identifier: AGPL-3.0-or-later

//! Drusite's tools for agents, over the Model Context Protocol.
//!
//! The same tools run in two places: the server's `/mcp` endpoint, for agents anywhere (Claude
//! Code on a laptop, an agent on a cluster), and the desktop app's local endpoint, for agents
//! on the same computer. Each place supplies [`Notes`]: how to find, load and save documents
//! there, as the agent's user. Everything else is here, so both places follow one policy:
//!
//! - An agent sees only the workspaces its sign-in names ([`Access`]), and writes only with
//!   write access.
//! - Journal entries are read-only unless the person allowed more when they connected it.
//! - There are no tools that delete. The most an edit can do is take blocks out of a page, and
//!   that can be undone.
//! - Every call that changes something is recorded with what it changed ([`Activity`]), so the
//!   person can see it and undo it ([`undo`]). Undoing is an edit of its own: whatever changed
//!   since stays.
//!
//! Pages go to agents as Markdown, in the same dialect as the Markdown bridge: each block ends
//! with its ID (`^…`), so an agent can cite a block, and an edited page keeps its blocks.

mod handler;
mod markdown;
mod tools;
mod undo;

use std::future::Future;

use drusite_crdt::{Page, Workspace};
use drusite_model::{BlockId, PageId, WorkspaceId};
use serde::{Deserialize, Serialize};

pub use handler::McpServer;
pub use tools::{TOOLS, ToolSpec, call as call_tool};
pub use undo::undo;

/// What a connected agent may do, as the person granted it when they connected it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Access {
    /// It may change pages, not only read them.
    pub write: bool,
    /// It may change journal entries too. Off unless the person turned it on.
    pub journal: bool,
    /// It may publish pages to the web (and change or take down what's published). Off unless
    /// the person turned it on; the server's policy checks it too.
    #[serde(default)]
    pub publish: bool,
    /// The workspaces it may see; `None` is all of the person's.
    pub workspaces: Option<Vec<WorkspaceId>>,
}

impl Access {
    pub fn read_only() -> Self {
        Self {
            write: false,
            journal: false,
            publish: false,
            workspaces: None,
        }
    }

    pub fn sees(&self, ws: WorkspaceId) -> bool {
        self.workspaces.as_ref().is_none_or(|w| w.contains(&ws))
    }
}

/// Who is calling: the agent and what it may do. The place running the tools adds whatever
/// else it needs to know (the server: whose sign-in it is).
#[derive(Debug, Clone)]
pub struct Agent {
    /// What the agent calls itself, or what the person named it ("Claude Code").
    pub name: String,
    pub access: Access,
    /// The person the agent acts for, where there is more than one (on the server).
    pub user: Option<uuid::Uuid>,
    /// The sign-in or token it uses, to attribute its activity.
    pub grant: uuid::Uuid,
}

/// A workspace an agent can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceInfo {
    pub id: WorkspaceId,
    /// The person's role allows changing it (an editor or owner).
    pub writable: bool,
}

/// What an agent asks of a page's share to the web.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareRequest {
    /// Publish, or change how; `None` keeps what's there (off, for a new link).
    Publish {
        include_subpages: Option<bool>,
        allow_search_engines: Option<bool>,
    },
    Unpublish,
}

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub page: PageId,
    pub block: Option<BlockId>,
    pub title: String,
    pub snippet: String,
}

/// What one tool call changed, as it is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Effect {
    /// A page's content changed between two versions (Loro version vectors, encoded).
    Page {
        page: PageId,
        #[serde(with = "b64")]
        before: Vec<u8>,
        #[serde(with = "b64")]
        after: Vec<u8>,
    },
    /// The workspace document changed: titles or properties.
    Workspace {
        #[serde(with = "b64")]
        before: Vec<u8>,
        #[serde(with = "b64")]
        after: Vec<u8>,
    },
    /// A page was made. Undoing it puts it in the trash.
    Created { page: PageId },
}

/// A tool call that changed something: what it was, in a sentence, and what it changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub tool: String,
    pub summary: String,
    pub effects: Vec<Effect>,
}

impl Activity {
    /// The pages it touched, for loading before an undo.
    pub fn pages(&self) -> Vec<PageId> {
        self.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Page { page, .. } => Some(*page),
                _ => None,
            })
            .collect()
    }
}

/// Everything one call (or an undo) changes in a workspace, saved together.
#[derive(Debug, Default)]
pub struct Change {
    /// New pages, as snapshots; save them before the workspace update that lists them.
    pub created: Vec<(PageId, Vec<u8>)>,
    /// Updates to existing pages.
    pub pages: Vec<(PageId, Vec<u8>)>,
    /// An update to the workspace document.
    pub workspace: Option<Vec<u8>>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.pages.is_empty() && self.workspace.is_none()
    }
}

/// Why a tool call failed. The agent sees the message, so it says what to do differently.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Invalid(String),
    /// Something broke on our side. The message is logged, not shown.
    #[error("something went wrong in Drusite; try again later")]
    Server(String),
}

impl From<drusite_crdt::Error> for ToolError {
    fn from(e: drusite_crdt::Error) -> Self {
        match e {
            drusite_crdt::Error::Invalid(m) => Self::Invalid(m),
            other => Self::Server(other.to_string()),
        }
    }
}

pub type ToolResult<T> = Result<T, ToolError>;

/// The documents the tools work on, where they run. Every method acts as `agent`'s user, so
/// the place's own checks (the server's policy and row-level security) apply as well as the
/// tools' own.
pub trait Notes: Send + Sync + 'static {
    /// The workspaces the agent's user belongs to. The tools filter them by [`Access`].
    fn workspaces(
        &self,
        agent: &Agent,
    ) -> impl Future<Output = ToolResult<Vec<WorkspaceInfo>>> + Send;

    /// A workspace's document.
    fn workspace(
        &self,
        agent: &Agent,
        ws: WorkspaceId,
    ) -> impl Future<Output = ToolResult<Workspace>> + Send;

    /// The workspace a page is in, if it's anywhere the user can see.
    fn find_page(
        &self,
        agent: &Agent,
        page: PageId,
    ) -> impl Future<Output = ToolResult<Option<WorkspaceId>>> + Send;

    /// A page's document.
    fn page(
        &self,
        agent: &Agent,
        ws: WorkspaceId,
        page: PageId,
    ) -> impl Future<Output = ToolResult<Page>> + Send;

    fn search(
        &self,
        agent: &Agent,
        ws: WorkspaceId,
        query: &str,
        limit: usize,
    ) -> impl Future<Output = ToolResult<Vec<Hit>>> + Send;

    /// Shares a page to the web, changes how, or takes it down, as the agent's user (with the
    /// agent's own permission checked by the place's policy). Returns the page's share state in
    /// a sentence or two for the agent.
    fn share(
        &self,
        agent: &Agent,
        ws: WorkspaceId,
        page: PageId,
        request: ShareRequest,
    ) -> impl Future<Output = ToolResult<String>> + Send;

    /// Saves a call's changes and records what it did, together.
    fn commit(
        &self,
        agent: &Agent,
        ws: WorkspaceId,
        change: Change,
        activity: Activity,
    ) -> impl Future<Output = ToolResult<()>> + Send;
}

mod b64 {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}
