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

/// Discover Codex session files. Codex doesn't organize by project,
/// so we return all sessions and let callers filter if needed.
fn discover_codex_sessions(project_path: &str) -> Result<Vec<SessionInfo>> {
    let home = home_dir()?;
    let sessions_dir = home.join(".codex").join("sessions");

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
                    agent: Agent::Codex,
                    project_path: project_path.to_string(),
                    modified_at,
                    file_path: path.to_string_lossy().to_string(),
                });
            }
        }
    }
    Ok(sessions)
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
        None => {
            sessions.extend(discover_claude_code_sessions(project_path)?);
            sessions.extend(discover_codex_sessions(project_path)?);
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
        Agent::Codex => Ok(home
            .join(".codex")
            .join("sessions")
            .join(format!("{session_id}.jsonl"))),
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

        // Try to extract a timestamp
        if started_at.is_none() {
            if let Some(ts) = entry.get("timestamp").and_then(|t| t.as_str()) {
                started_at = DateTime::parse_from_rfc3339(ts).ok().map(|dt| dt.to_utc());
            }
        }

        let entry_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match entry_type {
            "message" => {
                let role = entry.get("role").and_then(|r| r.as_str()).unwrap_or("");
                let text = entry
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();

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
            "function_call" => {
                if let Some(name) = entry.get("name").and_then(|n| n.as_str()) {
                    *tool_calls.entry(name.to_string()).or_insert(0) += 1;
                    // Try to extract file paths from arguments
                    if let Some(args) = entry.get("arguments").and_then(|a| a.as_str()) {
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(args) {
                            extract_files_from_tool_input(name, &parsed, &mut files_touched);
                        }
                    }
                }
            }
            "function_call_output" => {
                // Check for error indicators
                if let Some(output) = entry.get("output").and_then(|o| o.as_str()) {
                    if output.contains("error") || output.contains("Error") {
                        errors.push(output.chars().take(200).collect());
                    }
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
