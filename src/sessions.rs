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
    })
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

    match agent {
        Agent::ClaudeCode => parse_claude_code_session(&path, session_id, project),
        Agent::Codex => parse_codex_session(&path, session_id, project),
        Agent::Gemini => parse_gemini_session(&path, session_id, project),
    }
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
