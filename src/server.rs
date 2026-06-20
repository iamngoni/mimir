use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, ErrorData as McpError, ListResourceTemplatesResult, ListResourcesResult,
    PaginatedRequestParams, RawResource, RawResourceTemplate, ReadResourceRequestParams,
    ReadResourceResult, ResourceContents, ResourceUpdatedNotificationParam, ServerCapabilities,
    ServerInfo, SubscribeRequestParams,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool, tool_handler, tool_router, Peer, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::models::Agent;
use crate::resources;
use crate::{notes, sessions};

// ── Tool parameter types ──────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListSessionsRequest {
    /// Absolute path to the project directory
    pub project_path: String,
    /// Filter by agent: "claude-code", "codex", "gemini", or omit for all
    pub agent: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetSessionSummaryRequest {
    /// The session ID (UUID stem of the JSONL file)
    pub session_id: String,
    /// Which agent: "claude-code", "codex", or "gemini"
    pub agent: String,
    /// Absolute project path (required for claude-code sessions)
    pub project_path: Option<String>,
    /// Include the full transcript (`raw_turns`). Defaults to false — the
    /// transcript can be very large; use the chunk tools for big sessions.
    pub include_raw_turns: Option<bool>,
    /// If set, attach a chunk manifest computed for this per-chunk token budget,
    /// so the caller can then page the transcript with `get_session_chunk`.
    pub chunk_size: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetSessionChunkRequest {
    /// The session ID
    pub session_id: String,
    /// Which agent: "claude-code", "codex", or "gemini"
    pub agent: String,
    /// Absolute project path (required for claude-code sessions)
    pub project_path: Option<String>,
    /// Per-chunk token budget (cl100k_base). Pick this to fit your own context
    /// limits; the manifest's `total_chunks` is computed from it.
    pub chunk_size: usize,
    /// Zero-based chunk index to fetch. Chunks are stable for a given size, so
    /// you may fetch them in any order.
    pub index: usize,
    /// The `content_hash` from the manifest. If provided and the session has
    /// since changed, the response is flagged `stale`.
    pub expected_hash: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetSessionStatsRequest {
    /// The session ID
    pub session_id: String,
    /// Which agent: "claude-code", "codex", or "gemini"
    pub agent: String,
    /// Absolute project path (required for claude-code sessions)
    pub project_path: Option<String>,
    /// Optional tool/command name to get an exact call count for (e.g. "Bash").
    pub tool: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SearchSessionsRequest {
    /// Absolute path to the project directory
    pub project_path: String,
    /// Case-insensitive substring to match in prompts and transcript content.
    pub query: Option<String>,
    /// Filter to sessions that touched a file matching this substring.
    pub file: Option<String>,
    /// Filter by agent: "claude-code", "codex", "gemini", or omit for all.
    pub agent: Option<String>,
    /// Only sessions modified at/after this RFC3339 timestamp.
    pub since: Option<String>,
    /// Maximum number of results to return.
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetProjectContextRequest {
    /// Absolute path to the project directory
    pub project_path: String,
    /// Filter by agent: "claude-code", "codex", "gemini", or omit for all.
    pub agent: Option<String>,
    /// Maximum number of recent sessions to aggregate (default 10).
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LeaveNoteRequest {
    /// Absolute path to the project directory the note belongs to.
    pub project_path: String,
    /// The note content — context for the next agent.
    pub content: String,
    /// Optional session this note relates to.
    pub session_id: Option<String>,
    /// Optional agent leaving the note ("claude-code", "codex", "gemini").
    pub agent: Option<String>,
    /// Optional author label.
    pub author: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetNotesRequest {
    /// Absolute path to the project directory.
    pub project_path: String,
    /// Maximum number of notes to return (default 50).
    pub limit: Option<usize>,
}

// ── Server ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MimirServer {
    tool_router: ToolRouter<Self>,
    /// Subscribed resource URIs → last-seen content hash, for change detection.
    subscriptions: Arc<Mutex<HashMap<String, String>>>,
    /// The connected client peer, captured on first subscribe, used to push
    /// `resources/updated` notifications from the background watcher.
    peer: Arc<Mutex<Option<Peer<RoleServer>>>>,
}

impl std::fmt::Debug for MimirServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MimirServer").finish_non_exhaustive()
    }
}

fn parse_agent(s: &str) -> Result<Agent, String> {
    Agent::from_kebab(s).ok_or_else(|| {
        format!("Unknown agent: {s}. Use \"claude-code\", \"codex\", or \"gemini\".")
    })
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
    /// Returns the session's initial prompt, tool usage, files touched, errors,
    /// and final assistant message. The full transcript (`raw_turns`) is omitted
    /// by default — set `include_raw_turns: true` for it, or pass `chunk_size`
    /// to get a chunk manifest and page the transcript with `get_session_chunk`.
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
            Ok(mut summary) => {
                if let Some(chunk_size) = req.chunk_size {
                    match sessions::get_session_manifest(
                        &req.session_id,
                        agent,
                        req.project_path.as_deref(),
                        chunk_size,
                    ) {
                        Ok(manifest) => summary.chunk_manifest = Some(manifest),
                        Err(e) => {
                            return serde_json::json!({"error": format!("{e:#}")}).to_string()
                        }
                    }
                }
                if !req.include_raw_turns.unwrap_or(false) {
                    summary.raw_turns = Vec::new();
                }
                serde_json::to_string_pretty(&summary).unwrap_or_else(|e| {
                    serde_json::json!({"error": format!("Serialization error: {e}")}).to_string()
                })
            }
            Err(e) => serde_json::json!({"error": format!("{e:#}")}).to_string(),
        }
    }

    /// Fetch one chunk of a session transcript.
    ///
    /// Call `get_session_summary` with a `chunk_size` first to get a manifest
    /// (`total_chunks`, `content_hash`), then fetch chunks 0..total_chunks in any
    /// order. Pass `expected_hash` to be told if the session changed mid-paging.
    #[tool(name = "get_session_chunk")]
    pub async fn get_session_chunk(
        &self,
        Parameters(req): Parameters<GetSessionChunkRequest>,
    ) -> String {
        let agent = match parse_agent(&req.agent) {
            Ok(a) => a,
            Err(e) => return serde_json::json!({"error": e}).to_string(),
        };

        match sessions::get_session_chunk(
            &req.session_id,
            agent,
            req.project_path.as_deref(),
            req.chunk_size,
            req.index,
            req.expected_hash.as_deref(),
        ) {
            Ok(chunk) => serde_json::to_string_pretty(&chunk).unwrap_or_else(|e| {
                serde_json::json!({"error": format!("Serialization error: {e}")}).to_string()
            }),
            Err(e) => serde_json::json!({"error": format!("{e:#}")}).to_string(),
        }
    }

    /// Get quantitative stats for a session: turn counts (total/user/assistant),
    /// total + per-tool call counts, files touched, errors, and total tokens.
    /// Pass `tool` to get the exact call count for one command (e.g. "Bash").
    #[tool(name = "get_session_stats")]
    pub async fn get_session_stats(
        &self,
        Parameters(req): Parameters<GetSessionStatsRequest>,
    ) -> String {
        let agent = match parse_agent(&req.agent) {
            Ok(a) => a,
            Err(e) => return serde_json::json!({"error": e}).to_string(),
        };

        match sessions::get_session_stats(
            &req.session_id,
            agent,
            req.project_path.as_deref(),
            req.tool.as_deref(),
        ) {
            Ok(stats) => serde_json::to_string_pretty(&stats).unwrap_or_else(|e| {
                serde_json::json!({"error": format!("Serialization error: {e}")}).to_string()
            }),
            Err(e) => serde_json::json!({"error": format!("{e:#}")}).to_string(),
        }
    }

    /// Search a project's sessions by content, touched file, agent, and/or time.
    ///
    /// Every provided filter must match. Returns matching sessions with the
    /// facets that matched and a short content snippet.
    #[tool(name = "search_sessions")]
    pub async fn search_sessions(
        &self,
        Parameters(req): Parameters<SearchSessionsRequest>,
    ) -> String {
        let agent_filter = match &req.agent {
            Some(a) => match parse_agent(a) {
                Ok(agent) => Some(agent),
                Err(e) => return serde_json::json!({"error": e}).to_string(),
            },
            None => None,
        };

        let since = match &req.since {
            Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
                Ok(dt) => Some(dt.to_utc()),
                Err(e) => {
                    return serde_json::json!({"error": format!("Invalid `since` timestamp: {e}")})
                        .to_string()
                }
            },
            None => None,
        };

        match sessions::search_sessions(
            &req.project_path,
            req.query.as_deref(),
            req.file.as_deref(),
            agent_filter,
            since,
            req.limit,
        ) {
            Ok(results) => serde_json::to_string_pretty(&results).unwrap_or_else(|e| {
                serde_json::json!({"error": format!("Serialization error: {e}")}).to_string()
            }),
            Err(e) => serde_json::json!({"error": format!("{e:#}")}).to_string(),
        }
    }

    /// Get an aggregated, cross-agent digest of recent work on a project:
    /// recent sessions, files touched, surfaced errors, and handoff notes. This
    /// is the tool to call at the start of a task to catch up on prior context.
    #[tool(name = "get_project_context")]
    pub async fn get_project_context(
        &self,
        Parameters(req): Parameters<GetProjectContextRequest>,
    ) -> String {
        let agent_filter = match &req.agent {
            Some(a) => match parse_agent(a) {
                Ok(agent) => Some(agent),
                Err(e) => return serde_json::json!({"error": e}).to_string(),
            },
            None => None,
        };

        match sessions::get_project_context(&req.project_path, agent_filter, req.limit) {
            Ok(mut ctx) => {
                // Layer in handoff notes (they live in SQLite, not the logs).
                if let Ok(notes) = notes::get_notes(&req.project_path, None) {
                    ctx.notes = notes;
                }
                serde_json::to_string_pretty(&ctx).unwrap_or_else(|e| {
                    serde_json::json!({"error": format!("Serialization error: {e}")}).to_string()
                })
            }
            Err(e) => serde_json::json!({"error": format!("{e:#}")}).to_string(),
        }
    }

    /// Leave a handoff note for a project — durable context for the next agent
    /// (e.g. "auth refactor half-done, see src/auth.rs"). Persisted in SQLite.
    #[tool(name = "leave_note")]
    pub async fn leave_note(&self, Parameters(req): Parameters<LeaveNoteRequest>) -> String {
        match notes::leave_note(
            &req.project_path,
            &req.content,
            req.session_id.as_deref(),
            req.agent.as_deref(),
            req.author.as_deref(),
        ) {
            Ok(note) => serde_json::to_string_pretty(&note).unwrap_or_else(|e| {
                serde_json::json!({"error": format!("Serialization error: {e}")}).to_string()
            }),
            Err(e) => serde_json::json!({"error": format!("{e:#}")}).to_string(),
        }
    }

    /// Get handoff notes left for a project, newest first.
    #[tool(name = "get_notes")]
    pub async fn get_notes(&self, Parameters(req): Parameters<GetNotesRequest>) -> String {
        match notes::get_notes(&req.project_path, req.limit) {
            Ok(notes) => serde_json::to_string_pretty(&notes).unwrap_or_else(|e| {
                serde_json::json!({"error": format!("Serialization error: {e}")}).to_string()
            }),
            Err(e) => serde_json::json!({"error": format!("{e:#}")}).to_string(),
        }
    }
}

#[tool_handler]
impl ServerHandler for MimirServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .enable_resources_list_changed()
                .build(),
        )
        .with_instructions(
            "Mimir — share session context between AI coding agents. Use \
             get_project_context or search_sessions to catch up, get_session_summary \
             (+ get_session_chunk) for detail, and leave_note for handoffs. Sessions \
             and notes are also exposed as resources you can read and subscribe to.",
        )
    }

    /// List every session (across all agents/projects) plus per-project notes as
    /// resources.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let mut items = Vec::new();

        let sessions = sessions::list_all_sessions()
            .map_err(|e| McpError::internal_error(format!("{e:#}"), None))?;
        for s in sessions {
            let uri = resources::session_uri(s.agent, &s.session_id, &s.project_path);
            let short: String = s.session_id.chars().take(8).collect();
            let name = format!("{} · {short}", s.agent);
            let description = format!(
                "{} session in {} (modified {})",
                s.agent,
                s.project_path,
                s.modified_at.to_rfc3339()
            );
            items.push(
                RawResource::new(uri, name)
                    .with_description(description)
                    .with_mime_type("application/json")
                    .no_annotation(),
            );
        }

        if let Ok(projects) = notes::list_note_projects() {
            for p in projects {
                items.push(
                    RawResource::new(resources::notes_uri(&p), format!("notes · {p}"))
                        .with_description(format!("Handoff notes for {p}"))
                        .with_mime_type("application/json")
                        .no_annotation(),
                );
            }
        }

        Ok(ListResourcesResult::with_all_items(items))
    }

    /// Advertise the URI shapes clients can construct directly.
    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        let templates = vec![
            RawResourceTemplate::new(
                "mimir://session/{agent}/{session_id}?project={project_path}",
                "Session",
            )
            .with_description(
                "A parsed coding-agent session. agent ∈ {claude-code, codex, gemini}.",
            )
            .with_mime_type("application/json")
            .no_annotation(),
            RawResourceTemplate::new("mimir://notes/{project_path}", "Project notes")
                .with_description("Handoff notes left for a project.")
                .with_mime_type("application/json")
                .no_annotation(),
        ];
        Ok(ListResourceTemplatesResult::with_all_items(templates))
    }

    /// Read a session (compact summary) or a project's notes as JSON.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let json = resources::read_content(&request.uri)
            .map_err(|e| McpError::internal_error(format!("{e:#}"), None))?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(json, request.uri).with_mime_type("application/json"),
        ]))
    }

    /// Subscribe to change notifications for a resource. Records the current
    /// content hash so the background watcher can detect later changes.
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if resources::parse_uri(&request.uri).is_none() {
            return Err(McpError::invalid_params(
                format!("Not a mimir resource URI: {}", request.uri),
                None,
            ));
        }
        // Capture the peer so the watcher can push notifications.
        *self.peer.lock().unwrap() = Some(context.peer.clone());

        let hash = resources::uri_to_path(&request.uri)
            .and_then(|p| sessions::file_hash(&p).ok())
            .unwrap_or_default();
        self.subscriptions.lock().unwrap().insert(request.uri, hash);
        Ok(())
    }

    /// Stop receiving change notifications for a resource.
    async fn unsubscribe(
        &self,
        request: rmcp::model::UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.subscriptions.lock().unwrap().remove(&request.uri);
        Ok(())
    }
}

impl MimirServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
            peer: Arc::new(Mutex::new(None)),
        }
    }

    /// Spawn a background task that polls subscribed resources and pushes a
    /// `resources/updated` notification whenever the underlying file changes.
    pub fn spawn_resource_watcher(&self) {
        let subscriptions = self.subscriptions.clone();
        let peer = self.peer.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                // Snapshot under the lock; never hold it across an await.
                let snapshot: Vec<(String, String)> = {
                    let guard = subscriptions.lock().unwrap();
                    guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                };
                if snapshot.is_empty() {
                    continue;
                }
                let Some(peer) = peer.lock().unwrap().clone() else {
                    continue;
                };

                for (uri, last_hash) in snapshot {
                    let current = resources::uri_to_path(&uri)
                        .and_then(|p| sessions::file_hash(&p).ok());
                    if let Some(current) = current {
                        if current != last_hash {
                            subscriptions.lock().unwrap().insert(uri.clone(), current);
                            let _ = peer
                                .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri))
                                .await;
                        }
                    }
                }
            }
        });
    }
}
