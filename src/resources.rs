//! MCP resource layer.
//!
//! Exposes sessions and handoff notes as MCP *resources* with stable URIs, so
//! clients can browse and attach them through their resource picker rather than
//! calling a tool. URIs are opaque to clients — they list them, then pass them
//! back verbatim to `resources/read` and `resources/subscribe`.
//!
//! Scheme:
//! - `mimir://session/<agent>/<session_id>?project=<pct-encoded path>`
//! - `mimir://notes/<pct-encoded project path>`

use std::path::PathBuf;

use anyhow::Result;

use crate::models::Agent;
use crate::{notes, sessions};

const SCHEME: &str = "mimir://";

/// Percent-encode a string for safe inclusion in a URI segment / query value.
pub fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Reverse [`pct_encode`].
pub fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Build the resource URI for a session.
pub fn session_uri(agent: Agent, session_id: &str, project_path: &str) -> String {
    format!(
        "{SCHEME}session/{agent}/{}?project={}",
        pct_encode(session_id),
        pct_encode(project_path)
    )
}

/// Build the resource URI for a project's handoff notes.
pub fn notes_uri(project_path: &str) -> String {
    format!("{SCHEME}notes/{}", pct_encode(project_path))
}

/// A parsed mimir resource URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Locator {
    Session {
        agent: Agent,
        session_id: String,
        project: String,
    },
    Notes {
        project: String,
    },
}

/// Parse a mimir resource URI back into a [`Locator`].
pub fn parse_uri(uri: &str) -> Option<Locator> {
    let rest = uri.strip_prefix(SCHEME)?;
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    let mut segs = path.split('/');

    match segs.next()? {
        "session" => {
            let agent = Agent::from_kebab(segs.next()?)?;
            let session_id = pct_decode(segs.next()?);
            let project = query
                .and_then(|q| q.strip_prefix("project="))
                .map(pct_decode)?;
            Some(Locator::Session {
                agent,
                session_id,
                project,
            })
        }
        "notes" => {
            let project = pct_decode(segs.next()?);
            Some(Locator::Notes { project })
        }
        _ => None,
    }
}

/// Resolve a URI to the on-disk file whose changes drive update notifications.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    match parse_uri(uri)? {
        Locator::Session {
            agent,
            session_id,
            project,
        } => sessions::resolve_path(&session_id, agent, Some(&project)).ok(),
        Locator::Notes { .. } => notes::database_path().ok(),
    }
}

/// Produce the JSON text content for a resource URI.
pub fn read_content(uri: &str) -> Result<String> {
    let locator = parse_uri(uri).ok_or_else(|| anyhow::anyhow!("Unrecognized resource URI: {uri}"))?;
    match locator {
        Locator::Session {
            agent,
            session_id,
            project,
        } => {
            // Return the compact summary (no raw_turns) — clients that want the
            // full transcript use the chunk tools.
            let mut summary = sessions::get_session_summary(&session_id, agent, Some(&project))?;
            summary.raw_turns = Vec::new();
            Ok(serde_json::to_string_pretty(&summary)?)
        }
        Locator::Notes { project } => {
            let notes = notes::get_notes(&project, None)?;
            Ok(serde_json::to_string_pretty(&notes)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_roundtrips_paths_with_specials() {
        for s in ["/Users/foo/my project", "a-b_c.d~e", "/x/y z/å"] {
            assert_eq!(pct_decode(&pct_encode(s)), s);
        }
    }

    #[test]
    fn session_uri_roundtrips() {
        let uri = session_uri(Agent::ClaudeCode, "abc-123", "/Users/foo/proj dir");
        let loc = parse_uri(&uri).unwrap();
        assert_eq!(
            loc,
            Locator::Session {
                agent: Agent::ClaudeCode,
                session_id: "abc-123".to_string(),
                project: "/Users/foo/proj dir".to_string(),
            }
        );
    }

    #[test]
    fn notes_uri_roundtrips() {
        let uri = notes_uri("/Users/foo/proj");
        assert_eq!(
            parse_uri(&uri).unwrap(),
            Locator::Notes {
                project: "/Users/foo/proj".to_string()
            }
        );
    }

    #[test]
    fn rejects_foreign_uris() {
        assert!(parse_uri("file:///etc/passwd").is_none());
        assert!(parse_uri("mimir://bogus/thing").is_none());
        assert!(parse_uri("mimir://session/not-an-agent/x?project=/p").is_none());
    }
}
