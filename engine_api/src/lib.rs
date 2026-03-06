use chrono::{DateTime, Utc};
use core_types::{DomainError, Entry, EntrySource, Snapshot, SnapshotId, Volume, VolumeId};
use rusqlite::{Connection, Result};

fn domain_err(e: DomainError) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}

fn parse_datetime(s: String) -> rusqlite::Result<DateTime<Utc>> {
    s.parse::<DateTime<Utc>>()
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

/// Creates a new isolated memory namespace with the given name
/// Inserts a row into volumes and returns that created Volume
pub fn create_volume(conn: &Connection, name: String) -> Result<Volume> {
    let now = Utc::now();
    conn.execute(
        "INSERT INTO volumes (name, created_at) VALUES (?1, ?2)",
        (&name, now.to_rfc3339()),
    )?;
    let id = conn.last_insert_rowid();
    let vol = Volume::new(id, name, now).map_err(domain_err)?;
    Ok(vol)
}

/// Stores a new knowledege entry in the specified volume.
/// Inserts into entries, then appends a PutEntry event into the EventLog with
/// next seq number for that volume
pub fn add_entry(
    conn: &Connection,
    volume_id: VolumeId,
    content: String,
    source: EntrySource,
) -> Result<Entry> {
    if content.trim().is_empty() {
        return Err(domain_err(core_types::DomainError::EmptyEntryContent));
    }
    let now = Utc::now();
    let source_str = match source {
        EntrySource::User => "user",
        EntrySource::Agent => "agent",
        EntrySource::Tool => "tool",
    };
    conn.execute(
        "INSERT INTO entries (volume_id, content, created_at, source) VALUES (?1, ?2, ?3, ?4)",
        (volume_id, &content, now.to_rfc3339(), source_str),
    )?;
    let entry_id = conn.last_insert_rowid();

    let next_seq = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE volume_id = ?1",
        [volume_id],
        |row| row.get::<_, i64>(0),
    )?;

    conn.execute(
        "INSERT INTO events (seq, volume_id, event_type, entry_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        (next_seq, volume_id, "put_entry", entry_id, now.to_rfc3339())
    )?;

    let entry = Entry::new(entry_id, volume_id, content, now, source).map_err(domain_err)?;
    Ok(entry)
}

/// Returns all entries in the volume whose content contains the query string
pub fn search(conn: &Connection, volume_id: VolumeId, query: String) -> Result<Vec<Entry>> {
    let mut stmt = conn.prepare(
        "SELECT id, volume_id, content, created_at, source 
            FROM entries 
            WHERE volume_id = ?1 
            AND content LIKE '%' || ?2 || '%'",
    )?;

    let entries = stmt
        .query_map((volume_id, query), |row| {
            let source_str: String = row.get(4)?;
            let source = match source_str.as_str() {
                "agent" => EntrySource::Agent,
                "tool" => EntrySource::Tool,
                _ => EntrySource::User,
            };
            let created_at: String = row.get(3)?;
            Ok(Entry::new(
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                parse_datetime(created_at)?,
                source,
            )
            .map_err(domain_err)?)
        })?
        .collect::<Result<Vec<Entry>>>()?;

    Ok(entries)
}

/// Bookmarks the current state of a volume by recording the latest event seq number.
/// Inserts into snapshots with a human-readable label.
pub fn snapshot(conn: &Connection, volume_id: VolumeId, label: String) -> Result<Snapshot> {
    let now = Utc::now();
    let event_seq_pointer = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM events WHERE volume_id = ?1",
        [volume_id],
        |row| row.get::<_, i64>(0),
    )?;
    conn.execute(
        "INSERT INTO snapshots (volume_id, event_seq_pointer, created_at, label) VALUES (?1, ?2, ?3, ?4)",
        (volume_id, event_seq_pointer, now.to_rfc3339(), &label),
    )?;
    let id = conn.last_insert_rowid();
    let snapshot =
        Snapshot::new(id, volume_id, event_seq_pointer, now, label).map_err(domain_err)?;
    Ok(snapshot)
}

/// Creates a new volume and replays all events from the source volume up to the snapshot's event_seq_pointer
/// copying the relevant entries.
/// The cloned volume starts as an independent namespace from that point in time.
pub fn clone_volume(conn: &Connection, snapshot_id: SnapshotId) -> Result<Volume> {
    let (source_volume_id, event_seq_pointer): (i64, i64) = conn.query_row(
        "SELECT volume_id, event_seq_pointer FROM snapshots WHERE id = ?1",
        [snapshot_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let source_name: String = conn.query_row(
        "SELECT name FROM volumes WHERE id =?1",
        [source_volume_id],
        |row| row.get(0),
    )?;
    let new_volume = create_volume(conn, format!("{}_clone", source_name))?;
    let new_volume_id = new_volume.id();

    let events = {
        let mut stmt = conn.prepare(
            "SELECT seq, entry_id FROM events WHERE volume_id = ?1 AND seq <= ?2
                ORDER BY seq ASC",
        )?;
        let rows: Vec<(i64, i64)> = stmt
            .query_map((source_volume_id, event_seq_pointer), |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<Vec<_>>>()?;
        rows
    };

    for (seq, entry_id) in events {
        let (content, created_at, source): (String, String, String) = conn.query_row(
            "SELECT content, created_at, source FROM entries WHERE id = ?1",
            [entry_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        conn.execute(
            "INSERT INTO entries (volume_id, content, created_at, source) VALUES (?1, ?2, ?3, ?4)",
            (new_volume_id, &content, &created_at, &source),
        )?;
        let new_entry_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO events (seq, volume_id, event_type, entry_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            (seq, new_volume_id, "put_entry", new_entry_id, &created_at),
        )?;
    }

    Ok(new_volume)
}

/// Returns all volumes in the database
pub fn list_volumes(conn: &Connection) -> Result<Vec<Volume>> {
    let mut stmt = conn.prepare("SELECT id, name, created_at FROM volumes")?;
    let rows = stmt
        .query_map([], |row| {
            let created_at: String = row.get(2)?;
            Ok(
                Volume::new(row.get(0)?, row.get(1)?, parse_datetime(created_at)?)
                    .map_err(domain_err)?,
            )
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// Returns all snapshots for a given volume
pub fn list_snapshots(conn: &Connection, volume_id: VolumeId) -> Result<Vec<Snapshot>> {
    let mut stmt = conn.prepare(
        "SELECT id, volume_id, event_seq_pointer, created_at, label FROM snapshots WHERE volume_id = ?1"
    )?;
    let rows = stmt
        .query_map([volume_id], |row| {
            let created_at: String = row.get(3)?;
            Ok(Snapshot::new(
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                parse_datetime(created_at)?,
                row.get(4)?,
            )
            .map_err(domain_err)?)
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// Creates all tables if they don't exist. Safe to call on every startup.
pub fn init_db(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS volumes (
            id         INTEGER PRIMARY KEY,
            name       TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL
        )",
        (),
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS entries (
            id         INTEGER PRIMARY KEY,
            volume_id  INTEGER NOT NULL REFERENCES volumes(id),
            content    TEXT NOT NULL,
            created_at TEXT NOT NULL,
            source     TEXT NOT NULL CHECK (source IN ('user', 'agent', 'tool'))
        )",
        (),
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS events (
            seq        INTEGER NOT NULL,
            volume_id  INTEGER NOT NULL REFERENCES volumes(id),
            event_type TEXT NOT NULL CHECK (event_type IN ('put_entry')),
            entry_id   INTEGER NOT NULL REFERENCES entries(id),
            created_at TEXT NOT NULL,
            PRIMARY KEY (volume_id, seq)
        )",
        (),
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS snapshots (
            id                 INTEGER PRIMARY KEY,
            volume_id          INTEGER NOT NULL REFERENCES volumes(id),
            event_seq_pointer  INTEGER NOT NULL,
            created_at         TEXT NOT NULL,
            label              TEXT NOT NULL
        )",
        (),
    )?;

    Ok(())
}

/// Returns all entries for a given volume, ordered by insertion
pub fn list_entries(conn: &Connection, volume_id: VolumeId) -> Result<Vec<Entry>> {
    let mut stmt = conn.prepare(
        "SELECT id, volume_id, content, created_at, source FROM entries WHERE volume_id = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([volume_id], |row| {
            let source_str: String = row.get(4)?;
            let source = match source_str.as_str() {
                "agent" => EntrySource::Agent,
                "tool" => EntrySource::Tool,
                _ => EntrySource::User,
            };
            let created_at: String = row.get(3)?;
            Ok(Entry::new(
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                parse_datetime(created_at)?,
                source,
            )
            .map_err(domain_err)?)
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}
