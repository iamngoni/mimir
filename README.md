# Mimir

> *In Norse mythology, Mimir is the keeper of wisdom — the one the gods consult when they need to know what has come before.*

**Mimir** is an MCP (Model Context Protocol) server that lets AI coding agents share session context with each other. It reads existing session files written by Claude Code and Codex CLI, parses them into structured data, and exposes them as MCP tools.

No storage. No LLM calls. Just intelligent parsing of what your agents already write to disk.

## Why

Claude Code and Codex work in isolation by default. Each session starts cold, with no knowledge of what the other agent did. Mimir bridges that gap — an agent can call `list_sessions` or `get_session_summary` to understand what happened in a prior session before picking up work.

## Tools

### `list_sessions`
List available sessions for a given project path and optional agent filter.

```json
{
  "project_path": "/home/user/myproject",
  "agent": "claude-code"  // optional: "claude-code" | "codex"
}
```

### `get_session_summary`
Parse a session JSONL file and return structured data about what happened — initial prompt, files modified, tool calls made, errors, and final state.

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
| Codex | `~/.codex/sessions/<uuid>.jsonl` |

## Installation

```bash
# Clone and install the binary
git clone https://github.com/iamngoni/mimir.git
cd mimir
cargo install --path .
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
Add to `~/.codex/config.toml`:
```toml
[mcp_servers.mimir]
command = "mimir"
args = []
```

## Tech Stack

- Rust + `rmcp` (MCP SDK)
- `serde_json` for JSONL parsing
- `walkdir` for session discovery
- stdio transport only

## License

MIT
