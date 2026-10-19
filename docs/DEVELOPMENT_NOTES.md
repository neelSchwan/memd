# Development notes

Context and history for people and agents picking up work on memd. Code comments explain the code
as it is; this file explains how it got here and what is planned next. Add a dated entry when you
finish a meaningful piece of work.

## Current state

- Event sourcing works end to end. Event types: `put_entry`, `update_entry`, `delete_entry`,
  `restore_entry`. Each event stores its own content, so the log alone can rebuild any state.
- `entries` is a projection. Rows are never physically deleted (events reference them through a
  foreign key); deletion sets `deleted = 1`.
- Schema version is 1 (`PRAGMA user_version`). Databases from before versioning ("v0") are
  migrated automatically.
- Tests: `engine_api/tests/engine.rs`. The MCP server has no automated tests; it was checked by
  hand over stdio (see below).

## Design decisions

**Rollback appends instead of deleting.** Rolling back writes compensating events, so the log
still shows everything that happened, and a rollback can be undone by rolling back to a later
snapshot. Truncating the log would have been simpler but would lose history, which is the whole
point of the project.

**Clones copy the event history, not just the current state.** A branch can replay back to before
the snapshot it came from. Entry ids are remapped, because entry ids are global across volumes.
The clone stores `parent_snapshot_id` for lineage.

**Snapshot pointer 0 means "empty".** A snapshot of a volume with no events is valid and rolls back
to an empty volume.

**Identical updates are no-ops.** They record no event, to keep the log free of noise.

**Search uses `instr(lower(...))`, not `LIKE`.** `LIKE` treated `%` and `_` in user queries as
wildcards.

**Write transactions are IMMEDIATE.** Every write reads before it writes (for example, to find the
next seq). In WAL mode a deferred transaction fails with "database is locked" if another connection
commits in between, and the busy timeout doesn't help. Taking the write lock up front avoids this,
and the CLI and MCP server can share a database. `concurrent_writers_on_one_file_all_succeed`
covers it.

**Newer databases are refused.** `migrate` returns `UnsupportedSchemaVersion` when the database
is newer than the build, instead of quietly marking it with an older version.

**Update events don't record a source.** The entry keeps the source of whoever created it. If
per-change attribution becomes important, add it to `update_entry` events.

## Manual MCP check

To check the MCP server by hand, pipe JSON-RPC lines into it:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | MEMD_DB=/tmp/test.db target/debug/memd-mcp
```

## Ideas for next steps

- **Cross-branch diff and merge.** Clones get new entry ids, so comparing a branch with its parent
  needs a way to track which entry a cloned entry came from (for example an `origin_entry_id`
  column set during cloning).
- **Full-text search** with SQLite FTS5 (built into the bundled SQLite, so no new dependency).
- **Entry tags** and filtering.
- **MCP tests** that start the server in-process and call tools.

## History

### 2026-10-19: Event sourcing, rollback, diff

- Added update/delete/restore events with payloads, replay (`state_at`), `diff_snapshots`,
  `rollback_to_snapshot`, `rebuild_projection`, history-preserving clones and schema migrations.
- Replaced error smuggling through `rusqlite::Error` with a typed `EngineError`.
- Fixed: snapshot of an empty volume failed, cloning twice failed on a duplicate name, search
  treated `%`/`_` as wildcards, multi-step writes weren't transactional.
- Added `MEMD_DB` / `--db`, renamed the CLI binary to `memd`, added six MCP tools.
- Removed unused dependencies (`serde_json`, and `rusqlite` from `memd-mcp`, which now uses
  `engine_api::Connection`).
- Added `CLAUDE.md` (guidelines), this file, CI, and the `pr-reviewer` agent.
- The `pr-reviewer` agent's first review found that concurrent writers could fail with "database
  is locked", and that newer schema versions were overwritten. Both are fixed and have regression
  tests. `entry_history` now also takes the volume id and rejects entries from other volumes.
