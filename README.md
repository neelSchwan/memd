# memd

A persistent, versioned memory store exposed as an MCP server. Lets AI agents (Claude Desktop, Cursor, etc.) read and write structured memory that survives across conversations.

## What it does

Memory is organized into **volumes** (isolated namespaces). Each volume holds **entries** (text snippets) and an append-only event log. You can **snapshot** a volume at any point in time and **clone** it from a snapshot, giving you Git-like branching for memory.

```
Volume "work-notes"
├── Entry: "auth bug is in middleware.rs line 42"
├── Entry: "use postgres not sqlite in prod"
├── Snapshot "before-refactor" ──► clone ──► Volume "work-notes_clone"
└── Entry: "switched to axum for the HTTP layer"
```

## Architecture

```
memd/
├── core_types/    Domain types: Volume, Entry, Snapshot, Event
├── engine_api/    SQLite-backed storage logic
└── memd-mcp/      MCP server — exposes engine_api as tools over stdio
```

`engine_api` is the stable core. `memd-mcp` is one frontend — additional frontends (HTTP API, CLI, UI) can be added without touching the storage layer.

## Tools

| Tool              | Description                            |
| ----------------- | -------------------------------------- |
| `create_volume`   | Create a new memory namespace          |
| `list_volumes`    | List all volumes                       |
| `add_entry`       | Add a text entry to a volume           |
| `list_entries`    | List all entries in a volume           |
| `search_entries`  | Search entries by content substring    |
| `create_snapshot` | Bookmark the current state of a volume |
| `list_snapshots`  | List all snapshots for a volume        |
| `clone_volume`    | Create a new volume from a snapshot    |

## Getting started

### Prerequisites

- Rust (stable)
- Claude Desktop (or any MCP-compatible client)

### Build

```bash
cargo build -p memd-mcp
```

### Connect to Claude Desktop

Create or edit the Claude Desktop config file for your platform:

**Windows** — `%APPDATA%\Claude\claude_desktop_config.json`
```json
{
  "mcpServers": {
    "memd": {
      "command": "C:\\path\\to\\memd\\target\\debug\\memd-mcp.exe"
    }
  }
}
```

**macOS** — `~/Library/Application Support/Claude/claude_desktop_config.json`
```json
{
  "mcpServers": {
    "memd": {
      "command": "/path/to/memd/target/debug/memd-mcp"
    }
  }
}
```

Restart Claude Desktop. The tools will be available in every conversation.

### Connect to Cursor

Create or edit the Cursor MCP config file for your platform:

**Windows** — `%APPDATA%\Cursor\User\globalStorage\cursor.mcp\mcp.json`
```json
{
  "mcpServers": {
    "memd": {
      "command": "C:\\path\\to\\memd\\target\\debug\\memd-mcp.exe"
    }
  }
}
```

**macOS** — `~/.cursor/mcp.json`
```json
{
  "mcpServers": {
    "memd": {
      "command": "/path/to/memd/target/debug/memd-mcp"
    }
  }
}
```

Restart Cursor. The tools will be available to the AI in every conversation.

## Data

The SQLite database is created as `memd.db` in the working directory the server is launched from. All data persists across restarts.

## Roadmap

- [ ] HTTP API (`memd-server`) — so any service can call memd, not just MCP clients
- [ ] CLI (`memd-cli`) — for scripting and git hooks
- [ ] Web UI (`memd-ui`) — dashboard for browsing volumes, entries, and snapshot history
- [ ] Full-text search via SQLite FTS5
- [ ] Entry tagging and filtering
- [ ] `diff_snapshots`, `delete_entry`, `update_entry`
