// SPDX-License-Identifier: AGPL-3.0-or-later

//! Agents on this computer: the local MCP endpoint, its tokens, and what agents did.
//!
//! An agent here (Claude Code, say) connects to `http://127.0.0.1:<port>/mcp` with a token the
//! person made in the app, sent as `Authorization: Bearer <token>`. It gets the same tools as on
//! the server ([`drusite_mcp`]), with the same rules: what the token allows, journal entries
//! read-only unless allowed, nothing that deletes, and every change recorded so it can be
//! undone from the app.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use drusite_crdt::{Page, Workspace};
use drusite_mcp::{Access, Activity, Agent, Change, Effect};
use drusite_model::PageId;

use crate::api::{
    AgentAccess, AgentCall, AgentSource, AgentToken, ApiError, ErrorCode, Event, LOCAL_AGENTS_PORT,
    LocalAgents, Response,
};
use crate::engine::{Core, patch_for};
use crate::store::{AgentActivityRow, AgentTokenRow};

pub mod local;

fn now_ms() -> i64 {
    crate::trash::now_ms()
}

fn bad(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::Invalid, message.into())
}

fn token_hash(token: &str) -> Vec<u8> {
    blake3::hash(token.as_bytes()).as_bytes().to_vec()
}

fn access_of(row: &AgentTokenRow) -> Access {
    serde_json::from_str(&row.access).unwrap_or_else(|_| Access::read_only())
}

/// The pages a call touched, for the activity list.
fn pages_of(effects: &[Effect]) -> Vec<PageId> {
    let mut pages = Vec::new();
    for e in effects {
        let page = match e {
            Effect::Page { page, .. } | Effect::Created { page } => *page,
            Effect::Workspace { .. } => continue,
        };
        if !pages.contains(&page) {
            pages.push(page);
        }
    }
    pages
}

/// The local endpoint's address for `port`.
pub fn local_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

impl Core {
    /// Whether the local endpoint should run, and on which port.
    pub fn local_agents(&self) -> Result<(bool, u16), ApiError> {
        Ok(self
            .store
            .local_agents()?
            .unwrap_or((false, LOCAL_AGENTS_PORT)))
    }

    pub(crate) fn set_local_agents(&mut self, enabled: bool, port: u16) -> Result<(), ApiError> {
        if port < 1024 {
            return Err(bad("pick a port from 1024 up"));
        }
        self.store.set_local_agents(enabled, port)?;
        (self.events)(Event::AgentsChanged);
        Ok(())
    }

    /// The agents' list: the local endpoint, its tokens, and this computer's activity.
    /// `problem` is why the endpoint isn't running, if the app knows.
    pub fn agents(&self, problem: Option<String>) -> Result<Response, ApiError> {
        let (enabled, port) = self.local_agents()?;
        let tokens = self
            .store
            .agent_tokens()?
            .into_iter()
            .map(|row| {
                let access = access_of(&row);
                AgentToken {
                    id: row.id,
                    name: row.name,
                    access: AgentAccess {
                        write: access.write,
                        journal: access.journal,
                    },
                    created_at: row.created_at,
                    last_used_at: row.last_used_at,
                }
            })
            .collect();
        let activity = self
            .store
            .agent_activity(100)?
            .into_iter()
            .map(|row| {
                let effects: Vec<Effect> = serde_json::from_str(&row.effects).unwrap_or_default();
                AgentCall {
                    id: row.id,
                    source: AgentSource::Device,
                    agent: row.agent,
                    tool: row.tool,
                    summary: row.summary,
                    at: row.at,
                    undone_at: row.undone_at,
                    pages: pages_of(&effects),
                }
            })
            .collect();
        Ok(Response::Agents {
            local: LocalAgents {
                available: true,
                enabled,
                port,
                url: local_url(port),
                problem,
            },
            tokens,
            activity,
            server_problem: None,
        })
    }

    /// Makes a token for an agent on this computer. Only its hash is kept.
    pub(crate) fn create_agent_token(
        &mut self,
        name: &str,
        access: AgentAccess,
    ) -> Result<Response, ApiError> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 100 {
            return Err(bad("give the agent a name of up to 100 characters"));
        }
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret)
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        let token = format!("drusite_local_{}", URL_SAFE_NO_PAD.encode(secret));
        let access = Access {
            write: access.write,
            journal: access.write && access.journal,
            workspaces: None,
        };
        let row = AgentTokenRow {
            id: uuid::Uuid::now_v7().to_string(),
            name: name.to_owned(),
            access: serde_json::to_string(&access)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?,
            created_at: now_ms(),
            last_used_at: None,
        };
        self.store.add_agent_token(&row, &token_hash(&token))?;
        (self.events)(Event::AgentsChanged);
        let (_, port) = self.local_agents()?;
        Ok(Response::AgentToken {
            token,
            url: local_url(port),
        })
    }

    pub(crate) fn revoke_agent_token(&mut self, id: &str) -> Result<(), ApiError> {
        if !self.store.remove_agent_token(id)? {
            return Err(bad("no such agent"));
        }
        (self.events)(Event::AgentsChanged);
        Ok(())
    }

    /// The agent a token belongs to, if it's one of this computer's.
    pub fn agent_for(&mut self, token: &str) -> Result<Option<Agent>, ApiError> {
        let Some(row) = self.store.use_agent_token(&token_hash(token), now_ms())? else {
            return Ok(None);
        };
        Ok(Some(Agent {
            access: access_of(&row),
            name: row.name,
            user: None,
            grant: row.id.parse().unwrap_or_default(),
        }))
    }

    /// A copy of the workspace document, for a tool to change.
    pub(crate) fn workspace_copy(&self) -> Result<Workspace, ApiError> {
        Ok(Workspace::load(&self.workspace.export_snapshot()?)?)
    }

    /// A copy of a page, from the open one if it's open.
    pub(crate) fn page_copy(&self, id: PageId) -> Result<Page, ApiError> {
        if let Some(open) = self.open.get(&id) {
            return Ok(Page::load(&open.page.export_snapshot()?)?);
        }
        Ok(self.store.load_page(id)?.0)
    }

    /// Saves what a tool call (or an undo) changed, as an edit from elsewhere: open pages show
    /// it, and it syncs.
    pub(crate) fn apply_agent_change(&mut self, change: Change) -> Result<(), ApiError> {
        for (id, snapshot) in &change.created {
            let page = Page::load(snapshot)?;
            if page.id() != *id {
                return Err(bad("a new page isn't the page it says"));
            }
            self.store.create_page(&page)?;
            self.changed(*id.as_uuid());
        }
        for (id, update) in &change.pages {
            if let Some(open) = self.open.get_mut(id) {
                let patch = open.page.import(update)?;
                self.store.save_page(&open.page, &mut open.at)?;
                let view = patch_for(&open.page, &patch.updated, &patch.removed)?;
                if !view.is_empty() {
                    (self.events)(Event::PageChanged {
                        page: *id,
                        patch: view,
                    });
                }
            } else {
                let (mut page, mut at) = self.store.load_page(*id)?;
                page.import(update)?;
                self.store.save_page(&page, &mut at)?;
            }
            self.changed(*id.as_uuid());
        }
        if let Some(update) = &change.workspace {
            self.workspace.import(update)?;
            self.save_workspace()?;
        }
        Ok(())
    }

    /// Saves a tool call's change and records it.
    pub(crate) fn commit_agent_call(
        &mut self,
        agent: &Agent,
        change: Change,
        activity: Activity,
    ) -> Result<(), ApiError> {
        self.apply_agent_change(change)?;
        self.store.add_agent_activity(&AgentActivityRow {
            id: uuid::Uuid::now_v7().to_string(),
            agent: agent.name.clone(),
            tool: activity.tool,
            summary: activity.summary,
            effects: serde_json::to_string(&activity.effects)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?,
            at: now_ms(),
            undone_at: None,
        })?;
        (self.events)(Event::AgentsChanged);
        Ok(())
    }

    /// Takes back what one call on this computer changed, keeping everything since.
    pub(crate) fn undo_agent_call(&mut self, id: &str) -> Result<(), ApiError> {
        let row = self
            .store
            .agent_call(id)?
            .ok_or_else(|| bad("no such agent call"))?;
        if row.undone_at.is_some() {
            return Ok(());
        }
        let activity = Activity {
            tool: row.tool,
            summary: row.summary,
            effects: serde_json::from_str(&row.effects)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?,
        };
        let mut ws = self.workspace_copy()?;
        let mut pages = HashMap::new();
        for page in activity.pages() {
            pages.insert(page, self.page_copy(page)?);
        }
        let change = drusite_mcp::undo(&activity, &mut ws, &mut pages, now_ms())
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        self.apply_agent_change(change)?;
        self.store.mark_agent_call_undone(id, now_ms())?;
        (self.events)(Event::AgentsChanged);
        Ok(())
    }
}

/// The core, shared with the local endpoint's tasks.
pub type SharedCore = Arc<Mutex<Core>>;
