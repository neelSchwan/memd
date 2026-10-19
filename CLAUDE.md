# memd: Contributor Guidelines

These rules apply to every change in this repository, whether written by a person or an AI agent.
Claude Code loads this file automatically at the start of every session.

## 1. Simplicity comes first

Simplicity is the main goal. When two designs both work, pick the one with less code, fewer moving
parts, and fewer concepts a reader has to keep in mind.

- Solve the problem in front of you. Don't add configuration, abstraction layers, or extension
  points "for later".
- Prefer plain functions and data over traits, generics, and macros. Add a trait only when there
  are two real implementations.
- Keep functions short and doing one thing. If a function needs a comment to explain its sections,
  split it into smaller, well-named functions instead.
- Delete dead code; don't comment it out. Git keeps the history.
- If a change makes the code harder to read, it needs a strong reason.

## 2. Dependencies

Don't add a crate unless it is clearly necessary.

- First check whether the standard library or an existing dependency already does the job.
- A new dependency must be justified in the PR description: what it does, and why writing it
  ourselves would be worse.
- Remove dependencies that are no longer used. CI runs clippy, but it does not catch unused crates,
  so check `Cargo.toml` when you remove code.
- Current dependencies, for reference: `rusqlite`, `chrono`, `serde`, `schemars` (domain and
  storage); `clap` (CLI); `rmcp`, `tokio`, `anyhow` (MCP server).

## 3. Comments

Comments are for the reviewer reading the code next year, not a log of what you did today.

**Do comment:**
- Every public function, with a short `///` doc comment saying what it does and anything surprising
  about its behavior (for example: "writing identical content is a no-op").
- Anything that isn't obvious from the code itself: why a decision was made, an invariant that must
  hold, a non-obvious SQL constraint, an edge case being handled on purpose.
- Workarounds, with the reason they exist.

**Don't comment:**
- What a line plainly does (`// increment counter`). Good names make these unnecessary.
- Every line or every block. Most code should have no comments.
- History, session notes, TODO lists for agents, or "changed X to Y" remarks. Those belong in
  [`docs/DEVELOPMENT_NOTES.md`](docs/DEVELOPMENT_NOTES.md) or in the commit message, never in code.

Write comments in plain, complete sentences that a person can review without extra context.

## 4. Naming

Follow standard Rust naming (`snake_case` functions and variables, `CamelCase` types,
`SCREAMING_SNAKE_CASE` constants), plus these project conventions:

- Names describe what a thing is, not how it's stored: `entry_id`, not `eid` or `row_val`.
- No single-letter names except short closures and loop indexes.
- Booleans read as questions: `deleted`, `in_sync`, `carries_content()`.
- Domain words are used consistently: **volume**, **entry**, **event**, **snapshot**, **seq**
  (an event's sequence number). Don't introduce synonyms like "namespace", "record" or "checkpoint"
  in code.
- SQL row structs are named `<Thing>Row` and convert with `from_row` / `into_<thing>`.

## 5. Architecture rules

```
core_types/   Domain types and validation. No I/O, no database.
engine_api/   SQLite storage. All logic lives here. Also the `memd` CLI binary.
memd-mcp/     MCP server. A thin layer that maps tools to engine_api calls.
```

- **The event log is the source of truth.** The `entries` table is a projection of the log. Every
  change must append an event and update the projection in the same transaction.
- **History is never rewritten.** No `UPDATE` or `DELETE` on `events`. Undo means appending new
  events (see `rollback_to_snapshot`).
- Writes that touch more than one row run in a transaction.
- Frontends (`memd-mcp`, the CLI) contain no business logic. If they need something new, add it to
  `engine_api` first.
- Schema changes need a new step in `engine_api/src/schema.rs` and a bump of `SCHEMA_VERSION`, plus
  a migration test. Existing user databases must keep working.

## 6. Errors

- Return `engine_api::Result`. Don't `unwrap()` or `expect()` outside tests, except for a lock
  that can only fail if another thread already panicked.
- Add an `EngineError` variant when callers need to tell a case apart. Error messages are shown to
  users and agents, so make them specific: "entry 7 not found", not "query failed".

## 7. Tests and checks

Every change must pass these before it is pushed:

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

- New engine behavior gets an integration test in `engine_api/tests/`.
- Bug fixes get a test that fails without the fix.
- Tests use an in-memory database and read like a short story: set up, act, assert.

## 8. Pull requests

- One topic per PR. Keep diffs small enough to review in one sitting.
- The description says what changed, why, and how it was tested.
- Commit messages explain why, not just what.
- Before merging, review the diff against this file. The `pr-reviewer` agent
  (`.claude/agents/pr-reviewer.md`) does exactly that.
