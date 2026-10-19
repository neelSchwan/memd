use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::error::{EngineError, Result};

/// Bump this and add a step to `migrate` whenever the schema changes.
pub const SCHEMA_VERSION: i64 = 2;

const SCHEMA_V1: &str = "
    CREATE TABLE volumes (
        id                 INTEGER PRIMARY KEY,
        name               TEXT NOT NULL UNIQUE,
        created_at         TEXT NOT NULL,
        parent_snapshot_id INTEGER REFERENCES snapshots(id)
    );

    -- Projection of the event log: the current state of every entry.
    -- Rows are never physically removed so events can always reference them.
    CREATE TABLE entries (
        id         INTEGER PRIMARY KEY,
        volume_id  INTEGER NOT NULL REFERENCES volumes(id),
        content    TEXT NOT NULL,
        created_at TEXT NOT NULL,
        source     TEXT NOT NULL CHECK (source IN ('user', 'agent', 'tool')),
        updated_at TEXT NOT NULL DEFAULT '',
        deleted    INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE events (
        seq        INTEGER NOT NULL,
        volume_id  INTEGER NOT NULL REFERENCES volumes(id),
        event_type TEXT NOT NULL
                   CHECK (event_type IN ('put_entry', 'update_entry', 'delete_entry', 'restore_entry')),
        entry_id   INTEGER NOT NULL REFERENCES entries(id),
        content    TEXT,
        source     TEXT CHECK (source IS NULL OR source IN ('user', 'agent', 'tool')),
        created_at TEXT NOT NULL,
        PRIMARY KEY (volume_id, seq)
    );

    CREATE TABLE snapshots (
        id                 INTEGER PRIMARY KEY,
        volume_id          INTEGER NOT NULL REFERENCES volumes(id),
        event_seq_pointer  INTEGER NOT NULL,
        created_at         TEXT NOT NULL,
        label              TEXT NOT NULL
    );

    CREATE INDEX idx_entries_volume ON entries(volume_id);
    CREATE INDEX idx_events_entry ON events(entry_id);
    CREATE INDEX idx_snapshots_volume ON snapshots(volume_id);
";

/// v0 shipped without a user_version: events only had put_entry and no payload.
/// Entries are never mutated in v0, so their content is each put event's payload.
const MIGRATE_V0_TO_V1: &str = "
    ALTER TABLE volumes ADD COLUMN parent_snapshot_id INTEGER REFERENCES snapshots(id);

    ALTER TABLE entries ADD COLUMN updated_at TEXT NOT NULL DEFAULT '';
    ALTER TABLE entries ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0;
    UPDATE entries SET updated_at = created_at;

    CREATE TABLE events_v1 (
        seq        INTEGER NOT NULL,
        volume_id  INTEGER NOT NULL REFERENCES volumes(id),
        event_type TEXT NOT NULL
                   CHECK (event_type IN ('put_entry', 'update_entry', 'delete_entry', 'restore_entry')),
        entry_id   INTEGER NOT NULL REFERENCES entries(id),
        content    TEXT,
        source     TEXT CHECK (source IS NULL OR source IN ('user', 'agent', 'tool')),
        created_at TEXT NOT NULL,
        PRIMARY KEY (volume_id, seq)
    );
    INSERT INTO events_v1 (seq, volume_id, event_type, entry_id, content, source, created_at)
        SELECT ev.seq, ev.volume_id, ev.event_type, ev.entry_id, en.content, en.source, ev.created_at
        FROM events ev JOIN entries en ON en.id = ev.entry_id;
    DROP TABLE events;
    ALTER TABLE events_v1 RENAME TO events;

    CREATE INDEX idx_entries_volume ON entries(volume_id);
    CREATE INDEX idx_events_entry ON events(entry_id);
    CREATE INDEX idx_snapshots_volume ON snapshots(volume_id);
";

/// Full-text index over entry content, kept in sync with the entries projection by triggers.
/// The index has its own copy of the content; rowid is the entry id.
const MIGRATE_V1_TO_V2: &str = "
    CREATE VIRTUAL TABLE entries_fts USING fts5(content);
    INSERT INTO entries_fts (rowid, content) SELECT id, content FROM entries;

    CREATE TRIGGER entries_fts_insert AFTER INSERT ON entries BEGIN
        INSERT INTO entries_fts (rowid, content) VALUES (new.id, new.content);
    END;
    CREATE TRIGGER entries_fts_update AFTER UPDATE OF content ON entries BEGIN
        UPDATE entries_fts SET content = new.content WHERE rowid = new.id;
    END;
";

fn table_exists(tx: &Transaction, name: &str) -> Result<bool> {
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Brings the database up to `SCHEMA_VERSION`, creating it if empty.
/// Refuses databases written by a newer version of memd rather than guessing at their schema.
pub fn migrate(conn: &Connection) -> Result<()> {
    // Common case: already current, so no write lock is needed.
    if schema_version(conn)? == SCHEMA_VERSION {
        return Ok(());
    }

    // Take the write lock, then read the version again: another process starting at the same
    // time may have migrated the database while this one was waiting.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = schema_version(&tx)?;
    if version > SCHEMA_VERSION {
        return Err(EngineError::UnsupportedSchemaVersion(version));
    }
    if version < 1 {
        if table_exists(&tx, "events")? {
            tx.execute_batch(MIGRATE_V0_TO_V1)?;
        } else {
            tx.execute_batch(SCHEMA_V1)?;
        }
    }
    if version < 2 {
        tx.execute_batch(MIGRATE_V1_TO_V2)?;
    }

    tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
    tx.commit()?;
    Ok(())
}
