# Development notes

Context and history for people and agents picking up work on memd. Code comments explain the code
as it is; this file explains how it got here and what is planned next. Add a dated entry when you
finish a meaningful piece of work.

## Current state

- Event sourcing works end to end. Event types: `put_entry`, `update_entry`, `delete_entry`,
  `restore_entry`. Each event stores its own content, so the log alone can rebuild any state.
- `entries` is a projection. Rows are never physically deleted (events reference them through a
  foreign key); deletion sets `deleted = 1`.
- Schema version is 4 (`PRAGMA user_version`). Databases from before versioning ("v0") are
  migrated automatically.
- Tests: `engine_api/tests/engine.rs` for the engine, and `memd-mcp/tests/mcp.rs`, which starts the
  real `memd-mcp` binary on a temporary database and calls tools over stdio JSON-RPC.

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

**Search uses FTS5.** `entries_fts` is a standalone FTS5 table (it keeps its own copy of the
content) kept in sync with `entries` by triggers. Because the triggers live on the projection,
search follows updates, rollbacks and `rebuild_projection` with no extra code. The query is split
on whitespace, each word is quoted and prefix-matched, and the words are OR'ed together and
ranked with `bm25`. Quoting means FTS5 operators typed by a user or an agent can never cause a
syntax error, and FTS5's own tokenizer decides what a word is inside each quoted term, so the two
can't disagree. OR rather than AND because agents tend to search with several loosely related
keywords.

Known limitation: the index covers every volume and keeps deleted entries, so `bm25`'s word
statistics are global. A volume full of one word can change the ranking order in another volume.
Matching is unaffected. If this matters, add an `UNINDEXED` volume column to the index.

**Migrations take the write lock before reading the version.** The MCP server and the CLI often
start together. Without the lock, both read the old version and the second one fails to re-create
tables. `processes_starting_together_migrate_once` covers it.

**Write transactions are IMMEDIATE.** Every write reads before it writes (for example, to find the
next seq). In WAL mode a deferred transaction fails with "database is locked" if another connection
commits in between, and the busy timeout doesn't help. Taking the write lock up front avoids this,
and the CLI and MCP server can share a database. `concurrent_writers_on_one_file_all_succeed`
covers it.

**Newer databases are refused.** `migrate` returns `UnsupportedSchemaVersion` when the database
is newer than the build, instead of quietly marking it with an older version.

**Tags travel with content.** Put, update and restore events carry the entry's full tag list,
stored as one comma-separated string (tags can't contain commas). Treating tags as part of the
entry's state, rather than as separate tag/untag events, means replay, rollback, diff and clone
handle them with no tag-specific code. Tag filtering happens in Rust after loading entries:
volumes are small, and it avoids a join table. `Event::new` takes one argument per column, so it
allows clippy's `too_many_arguments`.

**Entries are matched across volumes by origin.** `entries.origin_entry_id` points at the
original entry a copy was made from; `COALESCE(origin_entry_id, id)` is an entry's origin. Clones
and merges copy the origin, so a clone of a clone still points at the root entry, and a volume never
holds two entries with the same origin. Migration v4 links existing clones by pairing each copied
put event with the source's put at the same seq.

**Merge is three-way and appends ordinary events.** The base is the most recent merge between the
two volumes (recorded as a snapshot on the source with `merged_into_volume_id`), or else the fork
point found by walking both volumes' `parent_snapshot_id` chains. Without the merge bookmark,
merging a long-lived branch twice would turn every re-edited entry into a false conflict. Merge
writes put/update/delete/restore events, so there is no new event type and rollback undoes a
merge like any other change. It lives in `engine_api/src/merge.rs`.

**Conflicts are reported and skipped.** Agents often merge without reading the preview, and "theirs
wins" would silently overwrite the target's edits. After a merge, the base moves past a skipped
conflict, so it counts as settled in favor of the target. To take the source's version, update the
target's entry, or add it again if the target deleted it.

**A version the target already held since the base is not re-applied.** Changes can reach a target
through a third volume (left → main → right). The base between left and right is still their fork
point, so without this rule merging left into right directly would undo right's later deletions
and reverts. "Since the base" is measured in time: clones copy events with their original
timestamps, so target events newer than the base are the target's own. This also makes rolling a
target back to before a merge stick.

**The v4 migration links clones in a loop, not a recursive query.** A clone always has a larger id
than its source, so going through volumes in id order lets each clone copy its source's
already-set origins with one indexed `UPDATE`. The first version used a recursive CTE and took
minutes on a 10,000-entry clone chain; `migration_links_long_clone_chains_quickly` guards this.

**Update events don't record a source.** The entry keeps the source of whoever created it. If
per-change attribution becomes important, add it to `update_entry` events.

## Manual MCP check

The automated tests cover the tools. To poke at the server by hand, pipe JSON-RPC lines into it:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | MEMD_DB=/tmp/test.db target/debug/memd-mcp
```

## Ideas for next steps

- **One representation for tags.** Inside the engine, tags are sometimes a `Vec<String>` and
  sometimes the stored comma-joined string (`EntryData`, `ReplayedEntry`). Keeping `Vec<String>`
  everywhere and joining/splitting only at the SQL boundary would remove the "stored form" notes.

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

### 2026-10-19: Full-text search

- `search` now uses an FTS5 index (schema v2) with prefix matching and `bm25` ranking.
- Review found that two processes upgrading the same database at once could fail; `migrate` now
  takes the write lock first and re-reads the version.

### 2026-10-19: Entry tags

- Schema v3 adds `tags` to `entries` and `events`. New `tag_entry`; `add_entry`, `list_entries` and
  `search` take tags. MCP: `tag_memory`, plus `tags` on `remember`, `list_memory`, `search_memory`.

### 2026-10-19: MCP tests

- Added `memd-mcp/tests/mcp.rs`. It runs the compiled server binary (not an in-process server) so
  the tests also cover the stdio transport, request parsing and how tool errors are reported.
  Covered: the tool list, a remember/update/forget/rollback workflow, branching, tag filtering,
  omitted optional fields and `isError` results. `serde_json` is a dev-dependency only; it was
  already in the dependency tree through `rmcp`.

### 2026-10-19: Cross-branch diff and merge

- Schema v4 adds `entries.origin_entry_id` and `snapshots.merged_into_volume_id`, backfilling
  origins for existing clones. New `diff_volumes` and `merge_volumes`, MCP tools of the same name,
  and `memd volume diff/merge`. Designed by a planning agent, then implemented and reviewed.
- MCP diff responses now return full entries (with tags) for `before`/`after`, not just content.
