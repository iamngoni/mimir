# Mimir

> *In Norse mythology, Mimir is the keeper of wisdom — the one the gods consult when they need to know what has come before.*

**Mimir** is an MCP (Model Context Protocol) server that lets AI coding agents share session context with each other. It reads existing session files written by Claude Code, Codex CLI, and Gemini CLI, parses them into structured data, and exposes them as MCP tools.

No storage. No LLM calls. Just intelligent parsing of what your agents already write to disk.

## Why

Claude Code, Codex, and Gemini work in isolation by default. Each session starts cold, with no knowledge of what the other agent did. Mimir bridges that gap — an agent can call `list_sessions` or `get_session_summary` to understand what happened in a prior session before picking up work.

## Tools

### `list_sessions`
List available sessions for a given project path and optional agent filter.

```json
{
  "project_path": "/home/user/myproject",
  "agent": "claude-code"  // optional: "claude-code" | "codex" | "gemini"
}
```

### `get_session_summary`
Parse a session file and return structured data about what happened — initial prompt, files modified, tool calls made, errors, and final state.

```json
{
  "session_id": "abc123",
  "agent": "claude-code"
}
```

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

## Tech Stack

- Rust + `rmcp` (MCP SDK)
- `serde_json` for JSONL/JSON parsing
- `walkdir` for session discovery
- stdio transport only

## License

MIT
