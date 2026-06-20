# Mimir

> *In Norse mythology, Mimir is the keeper of wisdom — the one the gods consult when they need to know what has come before.*

**Mimir** is an MCP (Model Context Protocol) server that lets AI coding agents share session context with each other. It reads existing session files written by Claude Code, Codex CLI, and Gemini CLI, parses them into structured data, and exposes them as MCP tools.

No storage. No LLM calls. Just intelligent parsing of what your agents already write to disk.

## Why

Claude Code, Codex, and Gemini work in isolation by default. Each session starts cold, with no knowledge of what the other agent did. Mimir bridges that gap — an agent can call `get_project_context` or `search_sessions` to understand what happened in prior sessions (across *all* agents) before picking up work, page through large transcripts in token-bounded chunks, and leave handoff notes for the next agent.

## Tools

### Discovery & aggregation

#### `list_sessions`
List available sessions for a given project path and optional agent filter.

```json
{
  "project_path": "/home/user/myproject",
  "agent": "claude-code"  // optional: "claude-code" | "codex" | "gemini"
}
```

#### `search_sessions`
Search a project's sessions by content, touched file, agent, and/or time. Every provided filter must match. Returns matching sessions with the facets that matched (`prompt` / `content` / `file`) and a short snippet.

```json
{
  "project_path": "/home/user/myproject",
  "query": "auth refactor",   // optional: substring in prompts/transcript
  "file": "src/auth.rs",       // optional: sessions that touched this file
  "agent": "codex",            // optional
  "since": "2026-06-01T00:00:00Z", // optional: RFC3339, modified at/after
  "limit": 20                   // optional
}
```

#### `get_project_context`
Aggregated, cross-agent digest of recent work on a project — recent sessions, files touched, surfaced errors, and handoff notes. The tool to call at the **start** of a task to catch up.

```json
{
  "project_path": "/home/user/myproject",
  "agent": "claude-code",  // optional
  "limit": 10               // optional: how many recent sessions to aggregate
}
```

### Session detail

#### `get_session_summary`
Parse a session into structured data — initial prompt, files modified, tool calls, errors, and final state. **Compact by default:** the full transcript (`raw_turns`) is omitted unless you ask for it, since it can be huge.

```json
{
  "session_id": "abc123",
  "agent": "claude-code",
  "project_path": "/home/user/myproject", // required for claude-code
  "include_raw_turns": false,  // optional: include the full transcript
  "chunk_size": 6000            // optional: attach a chunk manifest for this token budget
}
```

When `chunk_size` is set, the response includes a `chunk_manifest` (`total_chunks`, `total_tokens`, `content_hash`) so you can then page the transcript.

#### `get_session_chunk`
Fetch one chunk of a transcript. Chunk boundaries are a pure function of `(transcript, chunk_size)`, so chunks are stable and can be fetched in **any order** (e.g. last chunk first). Pass `expected_hash` from the manifest to be told if the session changed mid-paging (`stale: true`). Tokens are counted with `tiktoken` (cl100k_base); whole turns are packed up to the budget and a single oversized turn is split into `partial` fragments.

```json
{
  "session_id": "abc123",
  "agent": "claude-code",
  "project_path": "/home/user/myproject",
  "chunk_size": 6000,
  "index": 0,
  "expected_hash": "<content_hash from manifest>" // optional
}
```

#### `get_session_stats`
Quantitative stats for a session — turn counts (total / user / assistant), total and per-tool call counts, files touched, errors, and total tokens. Pass `tool` to get the exact call count for one command (e.g. how many `Bash` calls).

```json
{
  "session_id": "abc123",
  "agent": "claude-code",
  "project_path": "/home/user/myproject",
  "tool": "Bash"  // optional: exact call count for this tool
}
```

### Handoff notes

Notes are the one **writable** surface — durable context one agent leaves for the next. They are stored in a small SQLite database at `~/.mimir/mimir.db`, keyed by project path.

#### `leave_note`
```json
{
  "project_path": "/home/user/myproject",
  "content": "Auth refactor half-done — token validation still TODO in src/auth.rs",
  "session_id": "abc123",   // optional
  "agent": "claude-code",   // optional
  "author": "claude"         // optional
}
```

#### `get_notes`
```json
{
  "project_path": "/home/user/myproject",
  "limit": 50  // optional
}
```

## Resources

Beyond tools, Mimir exposes sessions and notes as MCP **resources**, so clients with a resource picker can browse and attach them directly, and subscribe to live updates.

- **`resources/list`** — every session across all agents/projects, plus a notes resource per project.
- **`resources/templates/list`** — advertises the URI shapes:
  - `mimir://session/{agent}/{session_id}?project={project_path}`
  - `mimir://notes/{project_path}`
- **`resources/read`** — returns the compact session summary (or the project's notes) as JSON. URIs are stable, so a client can read any of them directly.
- **`resources/subscribe`** — get a `notifications/resources/updated` when a session file (or the notes DB) changes. Mimir polls subscribed resources by content hash in the background.

## Session File Locations

| Agent | Path |
|-------|------|
| Claude Code | `~/.claude/projects/<encoded-path>/<uuid>.jsonl` |
| Codex | `~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<date>-<uuid>.jsonl` |
| Gemini | `~/.gemini/tmp/<project-alias>/chats/session-<date>-<uuid>.json` |

## Installation

```bash
curl -fsSL https://raw.githubusercontent.com/iamngoni/mimir/master/install.sh | sh
```

Or if you have Rust installed:
```bash
cargo install --git https://github.com/iamngoni/mimir
```

## MCP Configuration

### Claude Code
```bash
claude mcp add mimir --transport stdio -- mimir
```

Or manually in `~/.claude.json`:
```json
{
  "mcpServers": {
    "mimir": {
      "type": "stdio",
      "command": "mimir",
      "args": []
    }
  }
}
```

### Codex
```bash
codex mcp add mimir -- mimir
```

Or manually in `~/.codex/config.toml`:
```toml
[mcp_servers.mimir]
command = "mimir"
args = []
```

### Gemini CLI

Manually add to `~/.gemini/settings.json`:
```json
{
  "mcpServers": {
    "mimir": {
      "command": "mimir",
      "args": []
    }
  }
}
```

## Storage

Mimir reads agent session logs (it never writes to them). The only data Mimir itself stores is **handoff notes**, in a SQLite database at `~/.mimir/mimir.db`, created on first `leave_note`.

## Tech Stack

- Rust + `rmcp` (MCP SDK)
- `serde_json` for JSONL/JSON parsing
- `walkdir` for session discovery
- `tiktoken-rs` (cl100k_base) for transcript token counting / chunking
- `sha2` for chunk drift detection + resource change detection
- `rusqlite` (bundled SQLite) for handoff notes
- Tools **and** resources (with subscriptions); stdio transport only

## License

MIT
