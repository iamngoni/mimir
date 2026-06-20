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
    /// Full transcript. Omitted from the response unless explicitly requested,
    /// since it can be very large. Use the chunking tools for big sessions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_turns: Vec<Turn>,
    /// Present when the caller asked for a chunk plan via `chunk_size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_manifest: Option<ChunkManifest>,
}

/// A plan describing how a session transcript splits into token-bounded chunks.
///
/// Chunk boundaries are a pure function of `(transcript, chunk_size_tokens)`, so
/// any chunk index can be fetched independently and in any order. `content_hash`
/// lets a caller detect that the underlying session changed between fetches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkManifest {
    /// Number of chunks the transcript splits into for the requested budget.
    pub total_chunks: usize,
    /// The per-chunk token budget the caller requested.
    pub chunk_size_tokens: usize,
    /// Total tokens across the whole transcript (cl100k_base).
    pub total_tokens: usize,
    /// Number of turns in the transcript.
    pub turn_count: usize,
    /// SHA-256 of the underlying session file; pass back on chunk fetches.
    pub content_hash: String,
}

/// One chunk of a session transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionChunk {
    pub index: usize,
    pub total_chunks: usize,
    pub token_count: usize,
    pub content_hash: String,
    pub has_more: bool,
    /// True when the caller supplied an `expected_hash` that no longer matches
    /// the current file — the session changed and boundaries may have shifted.
    pub stale: bool,
    /// Turns contained in this chunk. A single oversized turn is split across
    /// chunks; the split pieces carry `partial: true`.
    pub turns: Vec<ChunkTurn>,
}

/// A turn (or turn fragment) within a chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkTurn {
    pub role: String,
    pub content: String,
    /// True if this is a fragment of a turn that was too large for one chunk.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
}

/// One hit from `search_sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub session_id: String,
    pub agent: Agent,
    pub project_path: String,
    pub modified_at: DateTime<Utc>,
    pub file_path: String,
    /// Which facets matched: any of "content", "file", "prompt".
    pub matched_on: Vec<String>,
    /// A short excerpt around the first content match, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    pub files_touched: Vec<String>,
}

/// An aggregated, cross-agent digest of recent work on a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectContext {
    pub project_path: String,
    pub session_count: usize,
    pub agents_seen: Vec<String>,
    pub recent_sessions: Vec<SessionDigest>,
    /// Files touched across the aggregated sessions, most-recently-touched first.
    pub files_touched: Vec<String>,
    /// Errors surfaced across the aggregated sessions (deduplicated).
    pub open_errors: Vec<String>,
    /// Handoff notes left for this project.
    pub notes: Vec<Note>,
}

/// A compact per-session entry inside a `ProjectContext`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDigest {
    pub session_id: String,
    pub agent: Agent,
    pub modified_at: DateTime<Utc>,
    pub initial_prompt: Option<String>,
    pub final_assistant_message: Option<String>,
    pub tool_call_count: usize,
    pub files_touched_count: usize,
    pub error_count: usize,
}

/// Quantitative stats for a single session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStats {
    pub session_id: String,
    pub agent: Agent,
    pub project_path: String,
    pub started_at: Option<DateTime<Utc>>,
    pub turn_count: usize,
    pub user_turns: usize,
    pub assistant_turns: usize,
    pub total_tool_calls: usize,
    /// Per-tool invocation counts, most-used first.
    pub tool_calls: Vec<ToolCallSummary>,
    pub files_touched_count: usize,
    pub error_count: usize,
    /// Total transcript tokens (cl100k_base).
    pub total_tokens: usize,
    /// When the request included a `tool` filter, the call count for exactly
    /// that tool (case-insensitive). Null when no filter was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_filter: Option<ToolCallSummary>,
}

/// A handoff note left by one agent for the next.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: i64,
    pub project_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub content: String,
    pub created_at: DateTime<Utc>,
}
