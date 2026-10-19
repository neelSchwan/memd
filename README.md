# memd

A persistent, versioned memory store exposed as an MCP server. Lets AI agents (Claude Desktop, Cursor, etc.) read and write structured memory that survives across conversations.

## What it does

Memory is organized into **volumes** (isolated namespaces). Each volume holds **entries** (text snippets) and an append-only event log. You can **snapshot** a volume at any point in time, **diff** snapshots, **roll back** to one, or **branch** a new volume from one, giving you Git-like version control for memory.

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
├── memd-mcp/      MCP server — exposes engine_api as tools over stdio
└── memd-server/   HTTP API — exposes engine_api as JSON over HTTP
```

### Event sourcing

Every change to a volume is appended to its event log as one of `put_entry`, `update_entry`, `delete_entry` or `restore_entry`. Each event carries its own payload, so the log alone fully describes a volume's history:

- The `entries` table is a **projection** of the log. `memd volume rebuild` replays the log and repairs any rows that drifted.
- A **snapshot** is just a pointer to a sequence number. Viewing or diffing a snapshot replays the log up to that point.
- **Rollback** never rewrites history. It appends compensating events that bring the live state back to the snapshot, so a rollback shows up in the log and can itself be undone.
- **Branching** copies the log up to the snapshot into a new volume, which keeps the full lineage and then evolves independently. The new volume records which snapshot it came from.
- **Merging** brings a branch's changes back into its parent (or any related volume). Every entry
  is compared three ways: as both sides last agreed on it, on the source, and on the target.
  Changes made only on the source are applied by appending events. Entries changed differently
  on both sides are reported as conflicts and left alone. A dry run previews the merge.
- Deleted entries disappear from listing and search but stay in history.
- Entries can carry **tags** (lowercase, no spaces or commas). Tags are versioned exactly like
  content, so rollback, diff and branching cover them too. Listing and search can filter by tags.
- Search uses SQLite's built-in FTS5 index. It matches whole words and word prefixes in any
  order, and ranks the best matches first.

The database schema is versioned with `PRAGMA user_version`, and databases created by earlier versions of memd are migrated automatically on startup.

`engine_api` is the stable core. `memd-mcp`, `memd-server` and the `memd` CLI are thin frontends over it; more can be added without touching the storage layer.

## Tools

| Tool                   | Description                                                     |
| ---------------------- | --------------------------------------------------------------- |
| `create_volume`        | Create a new memory namespace                                   |
| `list_volumes`         | List all volumes                                                |
| `remember`             | Add a text entry to a volume, optionally with tags              |
| `tag_memory`           | Replace an entry's tags (old tags stay in history)              |
| `update_memory`        | Replace an entry's content (old content stays in history)       |
| `forget`               | Delete an entry (it stays in history)                           |
| `list_memory`          | List live entries in a volume, optionally filtered by tags      |
| `search_memory`        | Full-text search over entries, best matches first, tag filter   |
| `memory_history`       | Show the event log for a volume or a single entry               |
| `create_snapshot`      | Bookmark the current state of a volume                          |
| `list_snapshots`       | List all snapshots for a volume                                 |
| `memory_at_snapshot`   | Show the entries a volume had at a snapshot                     |
| `diff_snapshots`       | Compare two snapshots, or a snapshot with the current state     |
| `rollback_to_snapshot` | Return a volume to a snapshot's state without rewriting history |
| `branch_from_snapshot` | Create a new volume from a snapshot                             |
| `diff_volumes`         | Compare two volumes, e.g. a branch and its parent               |
| `merge_volumes`        | Apply a branch's changes to a related volume (with dry run)     |

Diff tools (`diff_snapshots`, `diff_volumes`) return full entries, with tags, as `before` and
`after` of each modified entry. Before cross-branch merge they returned plain strings.

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

## HTTP API

`memd-server` serves the same engine as JSON over HTTP, so any service can use memd, not just MCP
clients:

```bash
cargo build -p memd-server
MEMD_DB=/path/to/memd.db target/debug/memd-server     # listens on 127.0.0.1:7878

json='Content-Type: application/json'
curl -X POST -H "$json" localhost:7878/api/volumes -d '{"name": "work"}'
curl -X POST -H "$json" localhost:7878/api/volumes/1/entries -d '{"content": "use postgres", "tags": ["db"]}'
curl "localhost:7878/api/volumes/1/search?q=postgres"
```

The server has no authentication and only serves local use:
- It answers only requests addressed to `localhost`, `127.0.0.1` or `[::1]`, which blocks DNS
  rebinding.
- Requests that change data must be sent as `application/json`. Web pages on other sites can't
  send that to it, so pages open in your browser can't plant or change memories.

`MEMD_ADDR` changes the listen port. All routes are listed at the top of
[`memd-server/src/lib.rs`](memd-server/src/lib.rs).

## CLI

The `memd` binary exposes the same engine for scripting and inspection:

```bash
cargo build -p engine_api            # builds target/debug/memd

memd volume create notes
memd entry add 1 "use sqlite in prod" --source agent --tag db
memd snapshot create 1 before-migration
memd entry update 1 "use postgres in prod"
memd snapshot diff 1                 # snapshot 1 vs current state
memd snapshot rollback 1             # back to "use sqlite in prod"
memd volume log 1                    # full event history, including the rollback
memd entry list 1 --tag db           # only entries tagged "db"
memd clone 1 --name what-if          # branch from snapshot 1 (creates volume 2)
memd volume diff 1 2                 # compare the branch with its parent
memd volume merge 2 1 --dry-run      # preview applying the branch's changes
memd volume merge 2 1                # apply them
```

Run `memd --help` for every command.

## Data

By default the SQLite database is `memd.db` in the working directory the server is launched from. Set `MEMD_DB` to use a fixed path instead, which is recommended for MCP clients since they may launch the server from an arbitrary directory:

```json
{
  "mcpServers": {
    "memd": {
      "command": "/path/to/memd/target/debug/memd-mcp",
      "env": { "MEMD_DB": "/path/to/memd.db" }
    }
  }
}
```

The CLI reads `MEMD_DB` too, or takes `--db <path>`. All data persists across restarts.

## Development

```bash
cargo test
cargo clippy --all-targets
```

## Roadmap

- [x] HTTP API (`memd-server`) — so any service can call memd, not just MCP clients
- [x] CLI (`memd`) — for scripting and git hooks
- [ ] Web UI (`memd-ui`) — dashboard for browsing volumes, entries, and snapshot history
- [x] Full-text search via SQLite FTS5
- [x] Entry tagging and filtering
- [x] `diff_snapshots`, `delete_entry`, `update_entry`, rollback
- [x] Cross-branch diff and merge between a volume and its branches
