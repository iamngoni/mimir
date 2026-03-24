use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which AI coding agent produced the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    ClaudeCode,
    Codex,
    Gemini,
}

impl std::fmt::Display for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Agent::ClaudeCode => write!(f, "claude-code"),
            Agent::Codex => write!(f, "codex"),
            Agent::Gemini => write!(f, "gemini"),
        }
    }
}

/// Metadata about a discovered session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub agent: Agent,
    pub project_path: String,
    pub modified_at: DateTime<Utc>,
    pub file_path: String,
}

/// A single tool invocation count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallSummary {
    pub name: String,
    pub count: usize,
}

/// A single conversation turn (user prompt + assistant response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub role: String,
    pub content: String,
}

/// Full summary of a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub agent: Agent,
    pub project_path: String,
    pub started_at: Option<DateTime<Utc>>,
    pub initial_prompt: Option<String>,
    pub turn_count: usize,
    pub tool_calls: Vec<ToolCallSummary>,
    pub files_touched: Vec<String>,
    pub errors: Vec<String>,
    pub final_assistant_message: Option<String>,
    pub raw_turns: Vec<Turn>,
}
