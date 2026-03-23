use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::models::Agent;
use crate::sessions;

// ── Tool parameter types ──────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListSessionsRequest {
    /// Absolute path to the project directory
    pub project_path: String,
    /// Filter by agent: "claude-code", "codex", or omit for both
    pub agent: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetSessionSummaryRequest {
    /// The session ID (UUID stem of the JSONL file)
    pub session_id: String,
    /// Which agent: "claude-code" or "codex"
    pub agent: String,
    /// Absolute project path (required for claude-code sessions)
    pub project_path: Option<String>,
}

// ── Server ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MimirServer {
    tool_router: ToolRouter<Self>,
}

fn parse_agent(s: &str) -> Result<Agent, String> {
    match s {
        "claude-code" => Ok(Agent::ClaudeCode),
        "codex" => Ok(Agent::Codex),
        other => Err(format!("Unknown agent: {other}. Use \"claude-code\" or \"codex\".")),
    }
}

#[tool_router]
impl MimirServer {
    /// List available AI coding agent sessions for a project.
    ///
    /// Returns metadata about each session including session ID, agent type,
    /// project path, last modified time, and file path.
    #[tool(name = "list_sessions")]
    pub async fn list_sessions(
        &self,
        Parameters(req): Parameters<ListSessionsRequest>,
    ) -> String {
        let agent_filter = match &req.agent {
            Some(a) => match parse_agent(a) {
                Ok(agent) => Some(agent),
                Err(e) => return serde_json::json!({"error": e}).to_string(),
            },
            None => None,
        };

        match sessions::list_sessions(&req.project_path, agent_filter) {
            Ok(sessions) => serde_json::to_string_pretty(&sessions).unwrap_or_else(|e| {
                serde_json::json!({"error": format!("Serialization error: {e}")}).to_string()
            }),
            Err(e) => serde_json::json!({"error": format!("{e:#}")}).to_string(),
        }
    }

    /// Get a detailed summary of a specific session.
    ///
    /// Returns the session's initial prompt, conversation turns, tool usage,
    /// files touched, errors encountered, and the final assistant message.
    #[tool(name = "get_session_summary")]
    pub async fn get_session_summary(
        &self,
        Parameters(req): Parameters<GetSessionSummaryRequest>,
    ) -> String {
        let agent = match parse_agent(&req.agent) {
            Ok(a) => a,
            Err(e) => return serde_json::json!({"error": e}).to_string(),
        };

        match sessions::get_session_summary(&req.session_id, agent, req.project_path.as_deref()) {
            Ok(summary) => serde_json::to_string_pretty(&summary).unwrap_or_else(|e| {
                serde_json::json!({"error": format!("Serialization error: {e}")}).to_string()
            }),
            Err(e) => serde_json::json!({"error": format!("{e:#}")}).to_string(),
        }
    }
}

#[tool_handler]
impl ServerHandler for MimirServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Mimir — share session context between AI coding agents. \
                 Use list_sessions to discover sessions, then get_session_summary \
                 for details.",
            )
    }
}

impl MimirServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}
