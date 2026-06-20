//! Handoff notes — the one writable surface in mimir.
//!
//! Notes let one agent leave durable context for the next agent working on a
//! project ("auth refactor half-done, see X"). They are stored in a small
//! SQLite database at `~/.mimir/mimir.db`, keyed by project path. SQLite (over a
//! flat file) gives us atomic writes, concurrent-reader safety, and room to back
//! a search index later.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::models::Note;

/// Resolve the SQLite database path, creating `~/.mimir/` if needed.
fn db_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME environment variable not set")?;
    let dir = PathBuf::from(home).join(".mimir");
    fs::create_dir_all(&dir).context("Failed to create ~/.mimir directory")?;
    Ok(dir.join("mimir.db"))
}

/// Ensure the schema exists on a connection.
fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS notes (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            project_path TEXT NOT NULL,
            session_id   TEXT,
            agent        TEXT,
            author       TEXT,
            content      TEXT NOT NULL,
            created_at   TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_notes_project ON notes(project_path);",
    )
    .context("Failed to initialize notes schema")?;
    Ok(())
}

/// Open the real on-disk database with schema applied.
fn open() -> Result<Connection> {
    let conn = Connection::open(db_path()?).context("Failed to open mimir database")?;
    init_schema(&conn)?;
    Ok(conn)
}

/// Insert a note on a given connection. Separated from `leave_note` so it can be
/// exercised against an in-memory database in tests.
fn insert_note(
    conn: &Connection,
    project_path: &str,
    content: &str,
    session_id: Option<&str>,
    agent: Option<&str>,
    author: Option<&str>,
) -> Result<Note> {
    if content.trim().is_empty() {
        anyhow::bail!("Note content must not be empty");
    }
    let created_at: DateTime<Utc> = Utc::now();
    let created_str = created_at.to_rfc3339();

    conn.execute(
        "INSERT INTO notes (project_path, session_id, agent, author, content, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![project_path, session_id, agent, author, content, created_str],
    )
    .context("Failed to insert note")?;

    let id = conn.last_insert_rowid();
    Ok(Note {
        id,
        project_path: project_path.to_string(),
        session_id: session_id.map(String::from),
        agent: agent.map(String::from),
        author: author.map(String::from),
        content: content.to_string(),
        created_at,
    })
}

/// Persist a new note and return it (with its assigned id and timestamp).
pub fn leave_note(
    project_path: &str,
    content: &str,
    session_id: Option<&str>,
    agent: Option<&str>,
    author: Option<&str>,
) -> Result<Note> {
    let conn = open()?;
    insert_note(&conn, project_path, content, session_id, agent, author)
}

/// Fetch notes for a project on a given connection.
fn fetch_notes(conn: &Connection, project_path: &str, limit: Option<usize>) -> Result<Vec<Note>> {
    let cap = limit.unwrap_or(50) as i64;

    let mut stmt = conn.prepare(
        "SELECT id, project_path, session_id, agent, author, content, created_at
         FROM notes
         WHERE project_path = ?1
         ORDER BY id DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![project_path, cap], |row| {
        let created_str: String = row.get(6)?;
        let created_at = DateTime::parse_from_rfc3339(&created_str)
            .map(|dt| dt.to_utc())
            .unwrap_or_else(|_| Utc::now());
        Ok(Note {
            id: row.get(0)?,
            project_path: row.get(1)?,
            session_id: row.get(2)?,
            agent: row.get(3)?,
            author: row.get(4)?,
            content: row.get(5)?,
            created_at,
        })
    })?;

    let mut notes = Vec::new();
    for note in rows {
        notes.push(note?);
    }
    Ok(notes)
}

/// Fetch notes for a project, newest first.
pub fn get_notes(project_path: &str, limit: Option<usize>) -> Result<Vec<Note>> {
    let conn = open()?;
    fetch_notes(&conn, project_path, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn insert_and_fetch_roundtrip() {
        let conn = mem_db();
        let note = insert_note(
            &conn,
            "/proj",
            "auth refactor half-done",
            Some("sess-1"),
            Some("claude-code"),
            Some("alice"),
        )
        .unwrap();
        assert_eq!(note.id, 1);
        assert_eq!(note.session_id.as_deref(), Some("sess-1"));

        let fetched = fetch_notes(&conn, "/proj", None).unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].content, "auth refactor half-done");
        assert_eq!(fetched[0].author.as_deref(), Some("alice"));
    }

    #[test]
    fn fetch_is_scoped_by_project_and_newest_first() {
        let conn = mem_db();
        insert_note(&conn, "/proj-a", "first", None, None, None).unwrap();
        insert_note(&conn, "/proj-a", "second", None, None, None).unwrap();
        insert_note(&conn, "/proj-b", "other", None, None, None).unwrap();

        let a = fetch_notes(&conn, "/proj-a", None).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].content, "second"); // newest first
        assert_eq!(a[1].content, "first");

        let b = fetch_notes(&conn, "/proj-b", None).unwrap();
        assert_eq!(b.len(), 1);

        let limited = fetch_notes(&conn, "/proj-a", Some(1)).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].content, "second");
    }

    #[test]
    fn empty_content_is_rejected() {
        let conn = mem_db();
        assert!(insert_note(&conn, "/proj", "   ", None, None, None).is_err());
    }
}
