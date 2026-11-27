// SPDX-License-Identifier: AGPL-3.0-or-later

//! The MCP server: lists the tools and runs them for the agent the request is from.
//!
//! The place serving it checks the request's token first and puts the [`Agent`] in the HTTP
//! request's extensions. A request without one never reaches a tool.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};

use crate::tools::{self, TOOLS, ToolSpec};
use crate::{Agent, Notes, ToolError};

const INSTRUCTIONS: &str = "Drusite holds the person's notes: pages in a tree, and a journal \
    with an entry per day. Find pages with search or list_pages, and read them with read_page \
    (Markdown; each block ends with its ID). When you add to the notes, prefer append_to_page \
    for adding and update_page for edits, keeping the block references read_page gave you. \
    Nothing you do deletes a page, and the person can undo any change you make.";

/// Drusite's MCP server, over whatever [`Notes`] the place running it has.
#[derive(Debug)]
pub struct McpServer<N> {
    notes: Arc<N>,
    version: &'static str,
}

impl<N> Clone for McpServer<N> {
    fn clone(&self) -> Self {
        Self {
            notes: Arc::clone(&self.notes),
            version: self.version,
        }
    }
}

impl<N: Notes> McpServer<N> {
    /// `version` is the app's or server's own, shown to clients.
    pub fn new(notes: Arc<N>, version: &'static str) -> Self {
        Self { notes, version }
    }
}

fn tool(spec: &ToolSpec) -> Tool {
    let annotations = ToolAnnotations::with_title(spec.title)
        .read_only(spec.read_only)
        .destructive(spec.destructive)
        .idempotent(spec.read_only)
        .open_world(false);
    Tool::new(spec.name, spec.description, (spec.schema)())
        .with_title(spec.title)
        .annotate(annotations)
}

fn agent_of(context: &RequestContext<RoleServer>) -> Option<Agent> {
    context
        .extensions
        .get::<http::request::Parts>()
        .and_then(|p| p.extensions.get::<Agent>())
        .cloned()
}

impl<N: Notes> ServerHandler for McpServer<N> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("drusite", self.version)
                    .with_title("Drusite")
                    .with_description("Your Drusite notes and journal"),
            )
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            TOOLS.iter().map(tool).collect(),
        ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        TOOLS.iter().find(|t| t.name == name).map(tool)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(agent) = agent_of(&context) else {
            // The place serving this let a request through without checking it.
            return Err(ErrorData::internal_error(
                "the request isn't signed in",
                None,
            ));
        };
        let arguments = request
            .arguments
            .map_or(serde_json::Value::Null, serde_json::Value::Object);
        let result = match tools::call(self.notes.as_ref(), &agent, &request.name, arguments).await
        {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(e) => {
                if let ToolError::Server(detail) = &e {
                    tracing::error!(tool = %request.name, error = %detail, "an agent's tool call failed");
                }
                CallToolResult::error(vec![ContentBlock::text(e.to_string())])
            }
        };
        Ok(CallToolResponse::Complete(result))
    }
}
