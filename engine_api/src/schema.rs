use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::error::{EngineError, Result};

/// Bump this and add a step to `migrate` whenever the schema changes.
pub const SCHEMA_VERSION: i64 = 4;

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

/// Entry tags. Put, update and restore events carry the entry's full tag list, like its content.
const MIGRATE_V2_TO_V3: &str = "
    ALTER TABLE entries ADD COLUMN tags TEXT NOT NULL DEFAULT '';
    ALTER TABLE events ADD COLUMN tags TEXT NOT NULL DEFAULT '';
";

/// Entry origins, so the same entry can be recognized across a volume and its branches, and the
/// merge bookmark on snapshots. Both are plain integers: entry rows are never deleted, so a foreign
/// key would add nothing. Existing clones are linked up by `link_existing_clones`.
const MIGRATE_V3_TO_V4: &str = "
    ALTER TABLE entries ADD COLUMN origin_entry_id INTEGER;
    ALTER TABLE snapshots ADD COLUMN merged_into_volume_id INTEGER;
";

/// Sets `origin_entry_id` on entries of volumes created by cloning before v4.
///
/// `clone_volume` copies a snapshot's events into the new volume in order, so a clone's put at
/// seq N is a copy of its source's put at seq N; the content check guards that pairing. A clone
/// always has a larger id than its source, so going through volumes in id order means the source's
/// origins are already set, and a clone of a clone points straight at the original entry.
fn link_existing_clones(tx: &Transaction) -> Result<()> {
    let clones: Vec<(i64, i64, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT v.id, s.volume_id, s.event_seq_pointer
                FROM volumes v JOIN snapshots s ON s.id = v.parent_snapshot_id
                ORDER BY v.id",
        )?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (clone_id, source_id, fork_seq) in clones {
        tx.execute(
            "UPDATE entries SET origin_entry_id = (
                SELECT COALESCE(source.origin_entry_id, source.id)
                FROM events clone_ev
                JOIN events source_ev ON source_ev.volume_id = ?2 AND source_ev.seq = clone_ev.seq
                JOIN entries source ON source.id = source_ev.entry_id
                WHERE clone_ev.volume_id = ?1
                    AND clone_ev.entry_id = entries.id
                    AND clone_ev.event_type = 'put_entry'
                    AND source_ev.event_type = 'put_entry'
                    AND clone_ev.seq <= ?3
                    AND clone_ev.content IS source_ev.content
            )
            WHERE volume_id = ?1",
            (clone_id, source_id, fork_seq),
        )?;
    }
    Ok(())
}

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
    if version < 3 {
        tx.execute_batch(MIGRATE_V2_TO_V3)?;
    }
    if version < 4 {
        tx.execute_batch(MIGRATE_V3_TO_V4)?;
        link_existing_clones(&tx)?;
    }

    tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
    tx.commit()?;
    Ok(())
}
