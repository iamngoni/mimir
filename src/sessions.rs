use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use walkdir::WalkDir;

use crate::models::*;

/// Get the user's home directory.
fn home_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME environment variable not set")?;
    Ok(PathBuf::from(home))
}

/// Encode a project path for Claude Code's directory naming scheme.
/// `/home/user/myproject` → `-home-user-myproject`
fn encode_project_path(project_path: &str) -> String {
    project_path.replace('/', "-")
}

/// Discover Claude Code session files for a given project path.
fn discover_claude_code_sessions(project_path: &str) -> Result<Vec<SessionInfo>> {
    let home = home_dir()?;
    let encoded = encode_project_path(project_path);
    let sessions_dir = home.join(".claude").join("projects").join(&encoded);

    if !sessions_dir.exists() {
        return Ok(vec![]);
    }

    let mut sessions = Vec::new();
    for entry in WalkDir::new(&sessions_dir).max_depth(1).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let modified_at = fs::metadata(path)
                    .and_then(|m| m.modified())
                    .map(DateTime::<Utc>::from)
                    .unwrap_or_default();

                sessions.push(SessionInfo {
                    session_id: stem.to_string(),
                    agent: Agent::ClaudeCode,
                    project_path: project_path.to_string(),
                    modified_at,
                    file_path: path.to_string_lossy().to_string(),
                });
            }
        }
    }
    Ok(sessions)
}

/// Discover Codex session files. Codex stores sessions in a `YYYY/MM/DD/` directory hierarchy.
/// Each session file contains a `session_meta` entry with `id` and `cwd` fields.
/// Sessions are filtered by `project_path` using the `cwd` from session metadata.
fn discover_codex_sessions(project_path: &str) -> Result<Vec<SessionInfo>> {
    let home = home_dir()?;
    let sessions_dir = home.join(".codex").join("sessions");

    if !sessions_dir.exists() {
        return Ok(vec![]);
    }

    let mut sessions = Vec::new();
    for entry in WalkDir::new(&sessions_dir).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let modified_at = fs::metadata(path)
                .and_then(|m| m.modified())
                .map(DateTime::<Utc>::from)
                .unwrap_or_default();

            // Read the session_meta entry to get the real session ID and cwd
            let (session_id, cwd) = extract_codex_session_meta(path);
            let session_id = session_id.unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            });

            // Filter by project_path: match if cwd starts with the project path
            let session_project = cwd.as_deref().unwrap_or("");
            if !session_project.is_empty() && !session_project.starts_with(project_path) {
                continue;
            }

            sessions.push(SessionInfo {
                session_id,
                agent: Agent::Codex,
                project_path: cwd.unwrap_or_else(|| project_path.to_string()),
                modified_at,
                file_path: path.to_string_lossy().to_string(),
            });
        }
    }
    Ok(sessions)
}

/// Extract session ID and cwd from the session_meta entry in a Codex JSONL file.
/// The session_meta entry is typically the first line, so we only read the first
/// few lines to avoid reading entire large session files during discovery.
fn extract_codex_session_meta(path: &Path) -> (Option<String>, Option<String>) {
    use std::io::{BufRead, BufReader};

    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (None, None),
    };

    let reader = BufReader::new(file);
    for line in reader.lines().take(5) {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let entry: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if entry.get("type").and_then(|t| t.as_str()) == Some("session_meta") {
            let payload = &entry["payload"];
            let id = payload.get("id").and_then(|i| i.as_str()).map(String::from);
            let cwd = payload.get("cwd").and_then(|c| c.as_str()).map(String::from);
            return (id, cwd);
        }
    }
    (None, None)
}

/// Resolve a project path to a Gemini project alias using `~/.gemini/projects.json`.
/// Returns None if no mapping exists for the given project path.
fn resolve_gemini_project_alias(project_path: &str) -> Option<String> {
    let home = home_dir().ok()?;
    let projects_file = home.join(".gemini").join("projects.json");
    let content = fs::read_to_string(projects_file).ok()?;
    let data: serde_json::Value = serde_json::from_str(&content).ok()?;
    let projects = data.get("projects")?.as_object()?;

    // Try exact match first, then prefix match
    for (path, alias) in projects {
        if path == project_path {
            return alias.as_str().map(String::from);
        }
    }
    for (path, alias) in projects {
        if project_path.starts_with(path.as_str()) {
            return alias.as_str().map(String::from);
        }
    }
    None
}

/// Discover Gemini CLI session files for a given project path.
/// Gemini stores sessions in `~/.gemini/tmp/<project-alias>/chats/session-*.json`.
/// The project alias is resolved via `~/.gemini/projects.json`.
fn discover_gemini_sessions(project_path: &str) -> Result<Vec<SessionInfo>> {
    let home = home_dir()?;

    let alias = match resolve_gemini_project_alias(project_path) {
        Some(a) => a,
        None => return Ok(vec![]),
    };

    let chats_dir = home.join(".gemini").join("tmp").join(&alias).join("chats");
    if !chats_dir.exists() {
        return Ok(vec![]);
    }

    let mut sessions = Vec::new();
    for entry in WalkDir::new(&chats_dir).max_depth(1).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let modified_at = fs::metadata(path)
                .and_then(|m| m.modified())
                .map(DateTime::<Utc>::from)
                .unwrap_or_default();

            // Extract session ID from the JSON file content
            let session_id = extract_gemini_session_id(path).unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            });

            sessions.push(SessionInfo {
                session_id,
                agent: Agent::Gemini,
                project_path: project_path.to_string(),
                modified_at,
                file_path: path.to_string_lossy().to_string(),
            });
        }
    }
    Ok(sessions)
}

/// Extract the session ID from a Gemini session JSON file.
fn extract_gemini_session_id(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};

    // The sessionId is near the top of the JSON file. Read enough to find it.
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(10) {
        let line = line.ok()?;
        if line.contains("\"sessionId\"") {
            // Parse "sessionId": "UUID"
            if let Some(start) = line.find("\"sessionId\"") {
                let rest = &line[start..];
                if let Some(colon) = rest.find(':') {
                    let value = rest[colon + 1..].trim().trim_matches(|c: char| c == '"' || c == ',');
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
        }
    }
    None
}

/// List all sessions for a project, optionally filtered by agent.
pub fn list_sessions(project_path: &str, agent: Option<Agent>) -> Result<Vec<SessionInfo>> {
    let mut sessions = Vec::new();

    match agent {
        Some(Agent::ClaudeCode) => {
            sessions.extend(discover_claude_code_sessions(project_path)?);
        }
        Some(Agent::Codex) => {
            sessions.extend(discover_codex_sessions(project_path)?);
        }
        Some(Agent::Gemini) => {
            sessions.extend(discover_gemini_sessions(project_path)?);
        }
        None => {
            sessions.extend(discover_claude_code_sessions(project_path)?);
            sessions.extend(discover_codex_sessions(project_path)?);
            sessions.extend(discover_gemini_sessions(project_path)?);
        }
    }

    // Sort by most recently modified first
    sessions.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    Ok(sessions)
}

/// Discover Gemini sessions across *all* known project aliases (for global
/// resource listing, where no single project path is supplied).
fn discover_all_gemini_sessions() -> Result<Vec<SessionInfo>> {
    let home = home_dir()?;
    let projects_file = home.join(".gemini").join("projects.json");
    let content = match fs::read_to_string(&projects_file) {
        Ok(c) => c,
        Err(_) => return Ok(vec![]),
    };
    let data: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Ok(vec![]),
    };

    let mut sessions = Vec::new();
    if let Some(projects) = data.get("projects").and_then(|p| p.as_object()) {
        for path in projects.keys() {
            sessions.extend(discover_gemini_sessions(path).unwrap_or_default());
        }
    }
    Ok(sessions)
}

/// List every discoverable session across all projects and all agents. Used for
/// the global MCP `resources/list`. This scans all agent session directories.
pub fn list_all_sessions() -> Result<Vec<SessionInfo>> {
    let mut sessions = Vec::new();

    // Claude Code: each subdirectory of ~/.claude/projects is one (encoded) project.
    if let Ok(home) = home_dir() {
        let projects = home.join(".claude").join("projects");
        if projects.exists() {
            for entry in WalkDir::new(&projects)
                .min_depth(1)
                .max_depth(1)
                .into_iter()
                .flatten()
            {
                if entry.file_type().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        // Reverse the path-encoding. This round-trips through
                        // encode_project_path() so resolution still works even
                        // for project names that contained hyphens.
                        let project_path = name.replace('-', "/");
                        sessions.extend(
                            discover_claude_code_sessions(&project_path).unwrap_or_default(),
                        );
                    }
                }
            }
        }
    }

    // Codex: an empty project filter returns all sessions (real cwd preserved).
    sessions.extend(discover_codex_sessions("").unwrap_or_default());

    // Gemini: walk every alias in projects.json.
    sessions.extend(discover_all_gemini_sessions().unwrap_or_default());

    sessions.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    Ok(sessions)
}

/// Public wrapper around session path resolution, for the resources layer.
pub fn resolve_path(session_id: &str, agent: Agent, project_path: Option<&str>) -> Result<PathBuf> {
    resolve_session_path(session_id, agent, project_path)
}

/// Public SHA-256 of a file, for subscription change detection.
pub fn file_hash(path: &Path) -> Result<String> {
    content_hash(path)
}

/// Resolve the file path for a session.
fn resolve_session_path(session_id: &str, agent: Agent, project_path: Option<&str>) -> Result<PathBuf> {
    let home = home_dir()?;
    match agent {
        Agent::ClaudeCode => {
            let project_path = project_path.context(
                "project_path is required for claude-code sessions",
            )?;
            let encoded = encode_project_path(project_path);
            Ok(home
                .join(".claude")
                .join("projects")
                .join(&encoded)
                .join(format!("{session_id}.jsonl")))
        }
        Agent::Codex => {
            // Codex sessions are in YYYY/MM/DD/ subdirectories with filenames
            // like `rollout-DATE-UUID.jsonl`. Search for the session ID in filenames.
            let sessions_dir = home.join(".codex").join("sessions");
            for entry in WalkDir::new(&sessions_dir).into_iter().flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        // Match by exact stem or by embedded UUID
                        if stem == session_id || stem.contains(session_id) {
                            return Ok(path.to_path_buf());
                        }
                    }
                    // Also check session_meta id inside the file
                    let (meta_id, _) = extract_codex_session_meta(path);
                    if meta_id.as_deref() == Some(session_id) {
                        return Ok(path.to_path_buf());
                    }
                }
            }
            // Fallback to flat path for backwards compatibility
            Ok(sessions_dir.join(format!("{session_id}.jsonl")))
        }
        Agent::Gemini => {
            // Gemini sessions are in ~/.gemini/tmp/<alias>/chats/session-*.json
            // Search all project aliases for the session ID.
            let gemini_tmp = home.join(".gemini").join("tmp");
            if gemini_tmp.exists() {
                for entry in WalkDir::new(&gemini_tmp).into_iter().flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("json")
                        && path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) == Some("chats")
                    {
                        // Check if filename contains the session ID
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            if stem.contains(session_id) {
                                return Ok(path.to_path_buf());
                            }
                        }
                        // Check sessionId inside the file
                        if extract_gemini_session_id(path).as_deref() == Some(session_id) {
                            return Ok(path.to_path_buf());
                        }
                    }
                }
            }
            anyhow::bail!("Gemini session not found: {session_id}")
        }
    }
}

/// Parse a Claude Code JSONL session file into a summary.
fn parse_claude_code_session(path: &Path, session_id: &str, project_path: &str) -> Result<SessionSummary> {
    let content = fs::read_to_string(path).context("Failed to read session file")?;

    let mut turns = Vec::new();
    let mut tool_calls: HashMap<String, usize> = HashMap::new();
    let mut files_touched = Vec::new();
    let mut errors = Vec::new();
    let mut initial_prompt: Option<String> = None;
    let mut final_assistant_message: Option<String> = None;
    let mut started_at: Option<DateTime<Utc>> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let entry: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // Skip malformed lines
        };

        // Try to extract a timestamp if present
        if started_at.is_none() {
            if let Some(ts) = entry.get("timestamp").and_then(|t| t.as_str()) {
                started_at = DateTime::parse_from_rfc3339(ts).ok().map(|dt| dt.to_utc());
            }
        }

        let entry_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match entry_type {
            "user" => {
                // Extract user message text
                let text = extract_claude_text(&entry["message"]["content"]);
                if !text.is_empty() {
                    if initial_prompt.is_none() {
                        initial_prompt = Some(text.clone());
                    }
                    turns.push(Turn {
                        role: "user".to_string(),
                        content: text,
                    });
                }
            }
            "assistant" => {
                let content_arr = &entry["message"]["content"];
                let mut text_parts = Vec::new();

                if let Some(arr) = content_arr.as_array() {
                    for item in arr {
                        let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match item_type {
                            "text" => {
                                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                                    text_parts.push(t.to_string());
                                }
                            }
                            "tool_use" => {
                                if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                                    *tool_calls.entry(name.to_string()).or_insert(0) += 1;
                                    // Track files touched via common tool patterns
                                    extract_files_from_tool_input(name, &item["input"], &mut files_touched);
                                }
                            }
                            _ => {}
                        }
                    }
                }

                let combined = text_parts.join("\n");
                if !combined.is_empty() {
                    final_assistant_message = Some(combined.clone());
                    turns.push(Turn {
                        role: "assistant".to_string(),
                        content: combined,
                    });
                }
            }
            "tool" => {
                // Check tool results for errors
                if let Some(arr) = entry.get("content").and_then(|c| c.as_array()) {
                    for item in arr {
                        if item.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false) {
                            if let Some(text) = item
                                .get("content")
                                .and_then(|c| c.as_array())
                                .and_then(|a| a.first())
                                .and_then(|i| i.get("text"))
                                .and_then(|t| t.as_str())
                            {
                                errors.push(text.to_string());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Deduplicate files_touched
    files_touched.sort();
    files_touched.dedup();

    let tool_call_summaries: Vec<ToolCallSummary> = {
        let mut v: Vec<_> = tool_calls
            .into_iter()
            .map(|(name, count)| ToolCallSummary { name, count })
            .collect();
        v.sort_by(|a, b| b.count.cmp(&a.count));
        v
    };

    Ok(SessionSummary {
        session_id: session_id.to_string(),
        agent: Agent::ClaudeCode,
        project_path: project_path.to_string(),
        started_at,
        initial_prompt,
        turn_count: turns.len(),
        tool_calls: tool_call_summaries,
        files_touched,
        errors,
        final_assistant_message,
        raw_turns: turns,
        chunk_manifest: None,
    })
}

/// Parse a Codex JSONL session file into a summary.
///
/// Codex JSONL format uses a wrapper structure where each line has:
/// - `type`: "session_meta" | "response_item" | "event_msg" | "turn_context"
/// - `payload`: the actual data, with its own `type` field
///
/// Key payload types within `response_item`:
/// - `message`: with `role` and `content` (array of `{type: "text", text: "..."}`)
/// - `function_call`: with `name`, `arguments` (JSON string), `call_id`
/// - `function_call_output`: with `call_id`, `output`
///
/// Key payload types within `event_msg`:
/// - `user_message`: with `message` field containing user input
/// - `agent_message`: with `message` field containing agent commentary
fn parse_codex_session(path: &Path, session_id: &str, project_path: &str) -> Result<SessionSummary> {
    let content = fs::read_to_string(path).context("Failed to read session file")?;

    let mut turns = Vec::new();
    let mut tool_calls: HashMap<String, usize> = HashMap::new();
    let mut files_touched = Vec::new();
    let mut errors = Vec::new();
    let mut initial_prompt: Option<String> = None;
    let mut final_assistant_message: Option<String> = None;
    let mut started_at: Option<DateTime<Utc>> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let entry: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Extract timestamp from wrapper
        if started_at.is_none() {
            if let Some(ts) = entry.get("timestamp").and_then(|t| t.as_str()) {
                started_at = DateTime::parse_from_rfc3339(ts).ok().map(|dt| dt.to_utc());
            }
        }

        let entry_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let payload = &entry["payload"];
        let payload_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match entry_type {
            "response_item" => {
                match payload_type {
                    "message" => {
                        let role = payload.get("role").and_then(|r| r.as_str()).unwrap_or("");
                        let text = extract_codex_content_text(&payload["content"]);

                        if !text.is_empty() {
                            if role == "user" && initial_prompt.is_none() {
                                initial_prompt = Some(text.clone());
                            }
                            if role == "assistant" {
                                final_assistant_message = Some(text.clone());
                            }
                            turns.push(Turn {
                                role: role.to_string(),
                                content: text,
                            });
                        }
                    }
                    "function_call" | "custom_tool_call" => {
                        let name_key = if payload_type == "function_call" { "name" } else { "name" };
                        if let Some(name) = payload.get(name_key).and_then(|n| n.as_str()) {
                            *tool_calls.entry(name.to_string()).or_insert(0) += 1;
                            // function_call uses "arguments" (JSON string), custom_tool_call uses "input" (object)
                            if let Some(args) = payload.get("arguments").and_then(|a| a.as_str()) {
                                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(args) {
                                    extract_files_from_tool_input(name, &parsed, &mut files_touched);
                                }
                            } else if let Some(input) = payload.get("input") {
                                extract_files_from_tool_input(name, input, &mut files_touched);
                            }
                        }
                    }
                    "function_call_output" | "custom_tool_call_output" => {
                        if let Some(output) = payload.get("output").and_then(|o| o.as_str()) {
                            if output.contains("error") || output.contains("Error") {
                                errors.push(output.chars().take(200).collect());
                            }
                        }
                    }
                    _ => {}
                }
            }
            "event_msg" => {
                match payload_type {
                    "user_message" => {
                        if let Some(msg) = payload.get("message").and_then(|m| m.as_str()) {
                            if !msg.is_empty() {
                                if initial_prompt.is_none() {
                                    initial_prompt = Some(msg.to_string());
                                }
                                turns.push(Turn {
                                    role: "user".to_string(),
                                    content: msg.to_string(),
                                });
                            }
                        }
                    }
                    "agent_message" => {
                        if let Some(msg) = payload.get("message").and_then(|m| m.as_str()) {
                            if !msg.is_empty() {
                                final_assistant_message = Some(msg.to_string());
                                turns.push(Turn {
                                    role: "assistant".to_string(),
                                    content: msg.to_string(),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    files_touched.sort();
    files_touched.dedup();

    let tool_call_summaries: Vec<ToolCallSummary> = {
        let mut v: Vec<_> = tool_calls
            .into_iter()
            .map(|(name, count)| ToolCallSummary { name, count })
            .collect();
        v.sort_by(|a, b| b.count.cmp(&a.count));
        v
    };

    Ok(SessionSummary {
        session_id: session_id.to_string(),
        agent: Agent::Codex,
        project_path: project_path.to_string(),
        started_at,
        initial_prompt,
        turn_count: turns.len(),
        tool_calls: tool_call_summaries,
        files_touched,
        errors,
        final_assistant_message,
        raw_turns: turns,
        chunk_manifest: None,
    })
}

/// Extract text from Codex content array.
/// Codex messages use `content: [{type: "text", text: "..."}]` format.
fn extract_codex_content_text(content: &serde_json::Value) -> String {
    // Handle string content (older format)
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    // Handle array content (current format)
    let mut parts = Vec::new();
    if let Some(arr) = content.as_array() {
        for item in arr {
            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                    parts.push(t.to_string());
                }
            }
        }
    }
    parts.join("\n")
}

/// Parse a Gemini CLI session JSON file into a summary.
///
/// Gemini uses a single JSON object (not JSONL) with:
/// - `sessionId`, `startTime`, `lastUpdated`
/// - `messages[]` with `type: "user" | "gemini" | "info"`
/// - User messages have `content: [{text: "..."}]`
/// - Gemini messages have `content: "..."` (string), `toolCalls[]`, `thoughts[]`
/// - Tool calls: `{name, args, result, status}`
fn parse_gemini_session(path: &Path, session_id: &str, project_path: &str) -> Result<SessionSummary> {
    let content = fs::read_to_string(path).context("Failed to read session file")?;
    let session: serde_json::Value = serde_json::from_str(&content).context("Failed to parse session JSON")?;

    let mut turns = Vec::new();
    let mut tool_calls: HashMap<String, usize> = HashMap::new();
    let mut files_touched = Vec::new();
    let mut errors = Vec::new();
    let mut initial_prompt: Option<String> = None;
    let mut final_assistant_message: Option<String> = None;

    let started_at = session
        .get("startTime")
        .and_then(|t| t.as_str())
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.to_utc());

    let messages = session.get("messages").and_then(|m| m.as_array());

    if let Some(msgs) = messages {
        for msg in msgs {
            let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match msg_type {
                "user" => {
                    // User content is an array of {text: "..."} objects
                    let text = if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                        arr.iter()
                            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    } else {
                        String::new()
                    };

                    if !text.is_empty() {
                        if initial_prompt.is_none() {
                            initial_prompt = Some(text.clone());
                        }
                        turns.push(Turn {
                            role: "user".to_string(),
                            content: text,
                        });
                    }
                }
                "gemini" => {
                    // Gemini content is a plain string
                    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();

                    if !text.is_empty() {
                        final_assistant_message = Some(text.clone());
                        turns.push(Turn {
                            role: "assistant".to_string(),
                            content: text,
                        });
                    }

                    // Process tool calls
                    if let Some(calls) = msg.get("toolCalls").and_then(|tc| tc.as_array()) {
                        for call in calls {
                            if let Some(name) = call.get("name").and_then(|n| n.as_str()) {
                                *tool_calls.entry(name.to_string()).or_insert(0) += 1;

                                // Extract file paths from args
                                if let Some(args) = call.get("args") {
                                    extract_files_from_tool_input(name, args, &mut files_touched);
                                }

                                // Check for errors in results
                                if call.get("status").and_then(|s| s.as_str()) != Some("success") {
                                    if let Some(result) = call.get("result").and_then(|r| r.as_array()) {
                                        for r in result {
                                            if let Some(resp) = r.get("functionResponse")
                                                .and_then(|fr| fr.get("response"))
                                                .and_then(|resp| resp.get("output"))
                                                .and_then(|o| o.as_str())
                                            {
                                                if resp.contains("error") || resp.contains("Error") {
                                                    errors.push(resp.chars().take(200).collect());
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {} // Skip "info" and other types
            }
        }
    }

    files_touched.sort();
    files_touched.dedup();

    let tool_call_summaries: Vec<ToolCallSummary> = {
        let mut v: Vec<_> = tool_calls
            .into_iter()
            .map(|(name, count)| ToolCallSummary { name, count })
            .collect();
        v.sort_by(|a, b| b.count.cmp(&a.count));
        v
    };

    Ok(SessionSummary {
        session_id: session_id.to_string(),
        agent: Agent::Gemini,
        project_path: project_path.to_string(),
        started_at,
        initial_prompt,
        turn_count: turns.len(),
        tool_calls: tool_call_summaries,
        files_touched,
        errors,
        final_assistant_message,
        raw_turns: turns,
        chunk_manifest: None,
    })
}

/// Dispatch to the agent-specific parser for an already-resolved path.
fn parse_session_at(
    path: &Path,
    session_id: &str,
    agent: Agent,
    project: &str,
) -> Result<SessionSummary> {
    match agent {
        Agent::ClaudeCode => parse_claude_code_session(path, session_id, project),
        Agent::Codex => parse_codex_session(path, session_id, project),
        Agent::Gemini => parse_gemini_session(path, session_id, project),
    }
}

/// Get a full summary of a session.
pub fn get_session_summary(
    session_id: &str,
    agent: Agent,
    project_path: Option<&str>,
) -> Result<SessionSummary> {
    let path = resolve_session_path(session_id, agent, project_path)?;

    if !path.exists() {
        anyhow::bail!("Session file not found: {}", path.display());
    }

    let project = project_path.unwrap_or("unknown");
    parse_session_at(&path, session_id, agent, project)
}

// ── Transcript chunking ───────────────────────────────────────────────

/// Lazily-initialized cl100k_base tokenizer (GPT-4 / Claude-ish).
fn tokenizer() -> &'static tiktoken_rs::CoreBPE {
    use std::sync::OnceLock;
    static BPE: OnceLock<tiktoken_rs::CoreBPE> = OnceLock::new();
    BPE.get_or_init(|| tiktoken_rs::cl100k_base().expect("load cl100k_base tokenizer"))
}

/// Count tokens in a string. `encode_ordinary` ignores special-token strings so
/// arbitrary transcript content is counted safely.
fn count_tokens(text: &str) -> usize {
    tokenizer().encode_ordinary(text).len()
}

/// SHA-256 of a file's bytes, used to detect that a session changed between
/// chunk fetches.
fn content_hash(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).context("Failed to read session file for hashing")?;
    let digest = Sha256::digest(&bytes);
    Ok(format!("{digest:x}"))
}

/// Split a single oversized turn into token-bounded fragments.
fn split_turn(turn: &Turn, size_tokens: usize) -> Vec<ChunkTurn> {
    let tokens = tokenizer().encode_ordinary(&turn.content);
    let mut pieces = Vec::new();
    for window in tokens.chunks(size_tokens.max(1)) {
        let content = tokenizer().decode(window.to_vec()).unwrap_or_default();
        pieces.push(ChunkTurn {
            role: turn.role.clone(),
            content,
            partial: true,
        });
    }
    pieces
}

/// Plan how a transcript splits into chunks using turn-packing: whole turns are
/// packed up to the token budget; a single turn larger than the budget is split.
/// This is a pure, deterministic function of `(turns, chunk_size_tokens)`.
fn plan_chunks(turns: &[Turn], chunk_size_tokens: usize) -> Vec<Vec<ChunkTurn>> {
    let budget = chunk_size_tokens.max(1);
    let mut chunks: Vec<Vec<ChunkTurn>> = Vec::new();
    let mut current: Vec<ChunkTurn> = Vec::new();
    let mut current_tokens = 0usize;

    for turn in turns {
        let turn_tokens = count_tokens(&turn.content);

        if turn_tokens > budget {
            // Flush whatever is buffered, then emit the oversized turn as its
            // own sequence of partial-fragment chunks.
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                current_tokens = 0;
            }
            for piece in split_turn(turn, budget) {
                chunks.push(vec![piece]);
            }
            continue;
        }

        if current_tokens + turn_tokens > budget && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_tokens = 0;
        }

        current.push(ChunkTurn {
            role: turn.role.clone(),
            content: turn.content.clone(),
            partial: false,
        });
        current_tokens += turn_tokens;
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Build a chunk manifest for a session transcript.
pub fn get_session_manifest(
    session_id: &str,
    agent: Agent,
    project_path: Option<&str>,
    chunk_size_tokens: usize,
) -> Result<ChunkManifest> {
    let path = resolve_session_path(session_id, agent, project_path)?;
    if !path.exists() {
        anyhow::bail!("Session file not found: {}", path.display());
    }
    let project = project_path.unwrap_or("unknown");
    let summary = parse_session_at(&path, session_id, agent, project)?;

    let hash = content_hash(&path)?;
    let total_tokens: usize = summary.raw_turns.iter().map(|t| count_tokens(&t.content)).sum();
    let chunks = plan_chunks(&summary.raw_turns, chunk_size_tokens);

    Ok(ChunkManifest {
        total_chunks: chunks.len(),
        chunk_size_tokens,
        total_tokens,
        turn_count: summary.raw_turns.len(),
        content_hash: hash,
    })
}

/// Fetch a single chunk of a session transcript by index.
pub fn get_session_chunk(
    session_id: &str,
    agent: Agent,
    project_path: Option<&str>,
    chunk_size_tokens: usize,
    index: usize,
    expected_hash: Option<&str>,
) -> Result<SessionChunk> {
    let path = resolve_session_path(session_id, agent, project_path)?;
    if !path.exists() {
        anyhow::bail!("Session file not found: {}", path.display());
    }
    let project = project_path.unwrap_or("unknown");
    let summary = parse_session_at(&path, session_id, agent, project)?;

    let hash = content_hash(&path)?;
    let stale = expected_hash.map(|h| h != hash).unwrap_or(false);
    let chunks = plan_chunks(&summary.raw_turns, chunk_size_tokens);
    let total_chunks = chunks.len();

    if index >= total_chunks {
        anyhow::bail!(
            "Chunk index {index} out of range (transcript has {total_chunks} chunk(s) at this size)"
        );
    }

    let turns = chunks.into_iter().nth(index).unwrap_or_default();
    let token_count: usize = turns.iter().map(|t| count_tokens(&t.content)).sum();

    Ok(SessionChunk {
        index,
        total_chunks,
        token_count,
        content_hash: hash,
        has_more: index + 1 < total_chunks,
        stale,
        turns,
    })
}

/// Compute quantitative stats for a session, optionally with the call count for
/// one specific tool/command (case-insensitive).
pub fn get_session_stats(
    session_id: &str,
    agent: Agent,
    project_path: Option<&str>,
    tool: Option<&str>,
) -> Result<SessionStats> {
    let summary = get_session_summary(session_id, agent, project_path)?;

    let user_turns = summary.raw_turns.iter().filter(|t| t.role == "user").count();
    let assistant_turns = summary
        .raw_turns
        .iter()
        .filter(|t| t.role == "assistant")
        .count();
    let total_tool_calls = summary.tool_calls.iter().map(|t| t.count).sum();
    let total_tokens = summary
        .raw_turns
        .iter()
        .map(|t| count_tokens(&t.content))
        .sum();

    let tool_filter = tool.map(|name| {
        let lower = name.to_lowercase();
        let count = summary
            .tool_calls
            .iter()
            .filter(|t| t.name.to_lowercase() == lower)
            .map(|t| t.count)
            .sum();
        ToolCallSummary {
            name: name.to_string(),
            count,
        }
    });

    Ok(SessionStats {
        session_id: summary.session_id,
        agent: summary.agent,
        project_path: summary.project_path,
        started_at: summary.started_at,
        turn_count: summary.turn_count,
        user_turns,
        assistant_turns,
        total_tool_calls,
        tool_calls: summary.tool_calls,
        files_touched_count: summary.files_touched.len(),
        error_count: summary.errors.len(),
        total_tokens,
        tool_filter,
    })
}

// ── Search & aggregation ──────────────────────────────────────────────

/// Build a short excerpt around the first occurrence of `needle` in `haystack`.
fn make_snippet(haystack: &str, needle_lower: &str) -> Option<String> {
    let lower = haystack.to_lowercase();
    let pos = lower.find(needle_lower)?;
    let start = pos.saturating_sub(60);
    let end = (pos + needle_lower.len() + 60).min(haystack.len());
    // Snap to char boundaries to avoid slicing inside a UTF-8 sequence.
    let start = (0..=start).rev().find(|i| haystack.is_char_boundary(*i)).unwrap_or(0);
    let end = (end..=haystack.len()).find(|i| haystack.is_char_boundary(*i)).unwrap_or(haystack.len());
    let mut snip = String::new();
    if start > 0 {
        snip.push('…');
    }
    snip.push_str(haystack[start..end].trim());
    if end < haystack.len() {
        snip.push('…');
    }
    Some(snip)
}

/// Search sessions for a project by content substring, touched file, modified
/// time, and/or agent. Any provided filter must match.
pub fn search_sessions(
    project_path: &str,
    query: Option<&str>,
    file: Option<&str>,
    agent: Option<Agent>,
    since: Option<DateTime<Utc>>,
    limit: Option<usize>,
) -> Result<Vec<SearchResult>> {
    let sessions = list_sessions(project_path, agent)?;
    let query_lower = query.map(|q| q.to_lowercase());
    let file_lower = file.map(|f| f.to_lowercase());
    let cap = limit.unwrap_or(usize::MAX);

    let mut results = Vec::new();
    for info in sessions {
        if let Some(since) = since {
            if info.modified_at < since {
                continue;
            }
        }

        let summary = match parse_session_at(
            Path::new(&info.file_path),
            &info.session_id,
            info.agent,
            &info.project_path,
        ) {
            Ok(s) => s,
            Err(_) => continue, // skip unreadable/corrupt sessions
        };

        let mut matched_on = Vec::new();
        let mut snippet = None;

        if let Some(ql) = &query_lower {
            // Check the initial prompt first so prompt hits get their own facet.
            if summary
                .initial_prompt
                .as_deref()
                .map(|p| p.to_lowercase().contains(ql))
                .unwrap_or(false)
            {
                matched_on.push("prompt".to_string());
            }
            // Then scan all turn content for a body match + snippet.
            for turn in &summary.raw_turns {
                if turn.content.to_lowercase().contains(ql) {
                    matched_on.push("content".to_string());
                    snippet = make_snippet(&turn.content, ql);
                    break;
                }
            }
        }

        if let Some(fl) = &file_lower {
            if summary
                .files_touched
                .iter()
                .any(|f| f.to_lowercase().contains(fl))
            {
                matched_on.push("file".to_string());
            }
        }

        // A session qualifies if every provided filter matched.
        let query_ok = query_lower.is_none()
            || matched_on.iter().any(|m| m == "content" || m == "prompt");
        let file_ok = file_lower.is_none() || matched_on.iter().any(|m| m == "file");
        if !query_ok || !file_ok {
            continue;
        }

        results.push(SearchResult {
            session_id: info.session_id,
            agent: info.agent,
            project_path: info.project_path,
            modified_at: info.modified_at,
            file_path: info.file_path,
            matched_on,
            snippet,
            files_touched: summary.files_touched,
        });

        if results.len() >= cap {
            break;
        }
    }

    Ok(results)
}

/// Truncate a string to at most `max` characters, appending an ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max).collect();
    format!("{truncated}…")
}

/// Aggregate recent cross-agent work on a project into a single digest. Notes
/// are layered in by the caller (the server) since they live in SQLite.
pub fn get_project_context(
    project_path: &str,
    agent: Option<Agent>,
    limit: Option<usize>,
) -> Result<ProjectContext> {
    let sessions = list_sessions(project_path, agent)?; // already sorted newest-first
    let cap = limit.unwrap_or(10);

    let mut agents_seen: Vec<String> = Vec::new();
    let mut recent_sessions = Vec::new();
    let mut files_touched: Vec<String> = Vec::new();
    let mut open_errors: Vec<String> = Vec::new();
    let total = sessions.len();

    for info in sessions.into_iter().take(cap) {
        let agent_label = info.agent.to_string();
        if !agents_seen.contains(&agent_label) {
            agents_seen.push(agent_label);
        }

        let summary = match parse_session_at(
            Path::new(&info.file_path),
            &info.session_id,
            info.agent,
            &info.project_path,
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };

        for f in &summary.files_touched {
            if !files_touched.contains(f) {
                files_touched.push(f.clone());
            }
        }
        for e in &summary.errors {
            if !open_errors.contains(e) {
                open_errors.push(e.clone());
            }
        }

        recent_sessions.push(SessionDigest {
            session_id: info.session_id,
            agent: info.agent,
            modified_at: info.modified_at,
            initial_prompt: summary.initial_prompt.as_deref().map(|p| truncate(p, 200)),
            final_assistant_message: summary
                .final_assistant_message
                .as_deref()
                .map(|m| truncate(m, 200)),
            tool_call_count: summary.tool_calls.iter().map(|t| t.count).sum(),
            files_touched_count: summary.files_touched.len(),
            error_count: summary.errors.len(),
        });
    }

    Ok(ProjectContext {
        project_path: project_path.to_string(),
        session_count: total,
        agents_seen,
        recent_sessions,
        files_touched,
        open_errors,
        notes: Vec::new(),
    })
}

/// Extract text content from a Claude Code content array.
fn extract_claude_text(content: &serde_json::Value) -> String {
    let mut parts = Vec::new();
    if let Some(arr) = content.as_array() {
        for item in arr {
            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                    parts.push(t.to_string());
                }
            }
        }
    }
    parts.join("\n")
}

/// Extract file paths from tool input parameters.
/// Handles common tool patterns like Read, Write, Edit, Bash, etc.
fn extract_files_from_tool_input(
    tool_name: &str,
    input: &serde_json::Value,
    files: &mut Vec<String>,
) {
    // Common parameter names that contain file paths
    let path_keys = ["file_path", "path", "filePath", "filename", "file"];

    for key in &path_keys {
        if let Some(path) = input.get(*key).and_then(|p| p.as_str()) {
            if !path.is_empty() {
                files.push(path.to_string());
            }
        }
    }

    // For Bash/command tools, try to extract file paths from the command
    if tool_name.to_lowercase().contains("bash") || tool_name.to_lowercase().contains("command") {
        if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
            // Simple heuristic: look for paths starting with / or ./
            for word in cmd.split_whitespace() {
                let clean = word.trim_matches(|c: char| c == '"' || c == '\'');
                if (clean.starts_with('/') || clean.starts_with("./"))
                    && clean.contains('.')
                    && !clean.contains("//")
                {
                    files.push(clean.to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ── Pure functions ────────────────────────────────────────────────

    #[test]
    fn count_tokens_is_nonzero_for_text() {
        assert!(count_tokens("hello world") > 0);
        assert_eq!(count_tokens(""), 0);
    }

    #[test]
    fn content_hash_is_stable_and_distinguishing() {
        let dir = std::env::temp_dir().join("mimir-hash-test");
        fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        fs::write(&a, "same content").unwrap();
        fs::write(&b, "same content").unwrap();
        assert_eq!(content_hash(&a).unwrap(), content_hash(&b).unwrap());
        fs::write(&b, "different").unwrap();
        assert_ne!(content_hash(&a).unwrap(), content_hash(&b).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_chunks_packs_small_turns_into_one() {
        let turns = vec![
            Turn { role: "user".into(), content: "hello world".into() },
            Turn { role: "assistant".into(), content: "hi there friend".into() },
        ];
        let chunks = plan_chunks(&turns, 1000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 2);
        assert!(chunks[0].iter().all(|t| !t.partial));
    }

    #[test]
    fn plan_chunks_separates_turns_that_dont_fit_together() {
        let t1 = "alpha beta gamma delta epsilon";
        let t2 = "one two three four five six";
        let turns = vec![
            Turn { role: "user".into(), content: t1.into() },
            Turn { role: "assistant".into(), content: t2.into() },
        ];
        // Budget that fits either turn alone but not both together.
        let budget = count_tokens(t1).max(count_tokens(t2));
        let chunks = plan_chunks(&turns, budget);
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| c.len() == 1));
    }

    #[test]
    fn plan_chunks_splits_oversized_turn_into_partials() {
        let content = "word ".repeat(200);
        let turns = vec![Turn { role: "user".into(), content: content.clone() }];
        let chunks = plan_chunks(&turns, 10);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() == 1 && c[0].partial));
        let reconstructed: String = chunks.iter().map(|c| c[0].content.clone()).collect();
        assert!(reconstructed.contains("word"));
    }

    #[test]
    fn make_snippet_brackets_the_match() {
        let snip = make_snippet("the quick brown fox jumps", "brown").unwrap();
        assert!(snip.to_lowercase().contains("brown"));
        assert!(make_snippet("nothing here", "zzz").is_none());
    }

    #[test]
    fn truncate_respects_limit() {
        assert_eq!(truncate("hello", 3), "hel…");
        assert_eq!(truncate("hi", 5), "hi");
    }

    // ── Claude Code parser ────────────────────────────────────────────

    const CLAUDE_FIXTURE: &str = r#"{"type":"user","message":{"content":[{"type":"text","text":"fix the auth bug"}]},"timestamp":"2026-06-20T10:00:00Z"}
{"type":"assistant","message":{"content":[{"type":"text","text":"looking into it"},{"type":"tool_use","name":"Read","input":{"file_path":"/src/main.rs"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/src/auth.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}
{"type":"tool","content":[{"is_error":true,"content":[{"text":"compile error: boom"}]}]}
{"type":"assistant","message":{"content":[{"type":"text","text":"fixed it"}]}}"#;

    fn write_claude_fixture(session_id: &str, project: &str) -> std::path::PathBuf {
        let tmp = std::env::temp_dir().join(format!("mimir-home-{session_id}"));
        let encoded = encode_project_path(project);
        let dir = tmp.join(".claude").join("projects").join(&encoded);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{session_id}.jsonl")), CLAUDE_FIXTURE).unwrap();
        tmp
    }

    #[test]
    fn parses_claude_session_fields() {
        let path = std::env::temp_dir().join("mimir-parse-claude.jsonl");
        fs::write(&path, CLAUDE_FIXTURE).unwrap();
        let s = parse_claude_code_session(&path, "sess", "/proj").unwrap();
        assert_eq!(s.initial_prompt.as_deref(), Some("fix the auth bug"));
        assert_eq!(s.final_assistant_message.as_deref(), Some("fixed it"));
        // 1 user + 2 assistant text turns; the tool-only message emits no turn.
        assert_eq!(s.turn_count, 3);
        let read = s.tool_calls.iter().find(|t| t.name == "Read").unwrap();
        assert_eq!(read.count, 2);
        assert!(s.files_touched.iter().any(|f| f == "/src/auth.rs"));
        assert_eq!(s.errors.len(), 1);
        assert!(s.errors[0].contains("boom"));
        let _ = fs::remove_file(&path);
    }

    // ── End-to-end via a fake HOME ────────────────────────────────────

    static HOME_LOCK: Mutex<()> = Mutex::new(());

    fn with_fake_home<F: FnOnce()>(home: &std::path::Path, f: F) {
        let _guard = HOME_LOCK.lock().unwrap();
        let prev = std::env::var("HOME").ok();
        // SAFETY: HOME mutation is serialized by HOME_LOCK; restored after `f`.
        unsafe { std::env::set_var("HOME", home) };
        f();
        unsafe {
            match prev {
                Some(p) => std::env::set_var("HOME", p),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn end_to_end_claude_code_flow() {
        let project = "/work/myproj";
        let session_id = "e2e-session";
        let home = write_claude_fixture(session_id, project);

        with_fake_home(&home, || {
            // list_sessions discovers it
            let listed = list_sessions(project, Some(Agent::ClaudeCode)).unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].session_id, session_id);

            // stats
            let stats =
                get_session_stats(session_id, Agent::ClaudeCode, Some(project), Some("Read"))
                    .unwrap();
            assert_eq!(stats.user_turns, 1);
            assert_eq!(stats.assistant_turns, 2); // tool-only message emits no turn
            assert_eq!(stats.total_tool_calls, 3); // Read x2 + Bash x1
            assert_eq!(stats.tool_filter.unwrap().count, 2);
            assert!(stats.total_tokens > 0);

            // manifest + chunk round-trip
            let manifest =
                get_session_manifest(session_id, Agent::ClaudeCode, Some(project), 1000).unwrap();
            assert!(manifest.total_chunks >= 1);
            assert_eq!(manifest.content_hash.len(), 64); // sha256 hex
            let chunk = get_session_chunk(
                session_id,
                Agent::ClaudeCode,
                Some(project),
                1000,
                0,
                Some(&manifest.content_hash),
            )
            .unwrap();
            assert!(!chunk.stale);
            assert!(!chunk.turns.is_empty());

            // stale detection
            let stale = get_session_chunk(
                session_id,
                Agent::ClaudeCode,
                Some(project),
                1000,
                0,
                Some("deadbeef"),
            )
            .unwrap();
            assert!(stale.stale);

            // out-of-range index errors
            assert!(get_session_chunk(
                session_id,
                Agent::ClaudeCode,
                Some(project),
                1000,
                999,
                None
            )
            .is_err());

            // search by content and by file
            let by_content =
                search_sessions(project, Some("auth"), None, None, None, None).unwrap();
            assert_eq!(by_content.len(), 1);
            assert!(by_content[0].matched_on.iter().any(|m| m == "prompt"));

            let by_file =
                search_sessions(project, None, Some("auth.rs"), None, None, None).unwrap();
            assert_eq!(by_file.len(), 1);
            assert!(by_file[0].matched_on.iter().any(|m| m == "file"));

            let no_match =
                search_sessions(project, Some("nonexistent-xyz"), None, None, None, None).unwrap();
            assert!(no_match.is_empty());

            // project context aggregation
            let ctx = get_project_context(project, None, None).unwrap();
            assert_eq!(ctx.session_count, 1);
            assert_eq!(ctx.recent_sessions.len(), 1);
            assert!(ctx.agents_seen.iter().any(|a| a == "claude-code"));
            assert!(ctx.files_touched.iter().any(|f| f == "/src/main.rs"));
            assert!(!ctx.open_errors.is_empty());
        });

        let _ = fs::remove_dir_all(&home);
    }
}
