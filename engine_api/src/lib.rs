//! SQLite-backed, event-sourced memory engine.
//!
//! Every change to a volume is appended to the `events` table. The `entries` table is a
//! projection of that log: it can be thrown away and rebuilt with `rebuild_projection`,
//! and any past state can be reconstructed with `state_at`. Snapshots are pointers into
//! the log, so branching, diffing and rollback are all just replays.

mod error;
mod schema;

use chrono::{DateTime, Utc};
use core_types::{
    DomainError, Entry, EntryId, EntryModification, EntrySource, Event, EventType, MemoryDiff,
    RollbackReport, SeqNumber, Snapshot, SnapshotId, Volume, VolumeId, normalize_tags,
};
use rusqlite::{OptionalExtension, Row, Transaction, TransactionBehavior};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub use error::{EngineError, Result};
// Re-exported so frontends can hold a connection without depending on rusqlite directly.
pub use rusqlite::Connection;
pub use schema::SCHEMA_VERSION;

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

/// Opens (or creates) the database at `path` and brings its schema up to date.
pub fn open_db(path: impl AsRef<Path>) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // WAL lets the CLI read while the MCP server holds the database open.
    conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    init_db(&conn)?;
    Ok(conn)
}

/// Creates all tables if they don't exist and migrates older databases.
/// Safe to call on every startup.
pub fn init_db(conn: &Connection) -> Result<()> {
    schema::migrate(conn)
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

fn parse_datetime(s: &str) -> Result<DateTime<Utc>> {
    s.parse::<DateTime<Utc>>()
        .map_err(|e| EngineError::Corrupt(format!("invalid timestamp '{s}': {e}")))
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

/// Tags are stored as one comma-separated string. Commas can't appear in a tag.
fn join_tags(tags: &[String]) -> String {
    tags.join(",")
}

fn split_tags(stored: &str) -> Vec<String> {
    if stored.is_empty() {
        Vec::new()
    } else {
        stored.split(',').map(String::from).collect()
    }
}

/// The part of an entry that put, update and restore events carry. `tags` is in stored form.
#[derive(Clone, Copy)]
struct EntryData<'a> {
    content: &'a str,
    tags: &'a str,
}

const ENTRY_COLS: &str = "id, volume_id, content, tags, created_at, updated_at, source, deleted";

struct EntryRow {
    id: EntryId,
    volume_id: VolumeId,
    content: String,
    tags: String,
    created_at: String,
    updated_at: String,
    source: String,
    deleted: bool,
}

impl EntryRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            volume_id: row.get(1)?,
            content: row.get(2)?,
            tags: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
            source: row.get(6)?,
            deleted: row.get(7)?,
        })
    }

    fn into_entry(self) -> Result<Entry> {
        let created_at = parse_datetime(&self.created_at)?;
        let updated_at = if self.updated_at.is_empty() {
            created_at
        } else {
            parse_datetime(&self.updated_at)?
        };
        Ok(Entry::new(
            self.id,
            self.volume_id,
            self.content,
            split_tags(&self.tags),
            created_at,
            updated_at,
            self.source.parse()?,
        )?)
    }
}

const EVENT_COLS: &str = "seq, volume_id, event_type, entry_id, content, tags, source, created_at";

struct EventRow {
    seq: SeqNumber,
    volume_id: VolumeId,
    event_type: String,
    entry_id: EntryId,
    content: Option<String>,
    tags: String,
    source: Option<String>,
    created_at: String,
}

impl EventRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            seq: row.get(0)?,
            volume_id: row.get(1)?,
            event_type: row.get(2)?,
            entry_id: row.get(3)?,
            content: row.get(4)?,
            tags: row.get(5)?,
            source: row.get(6)?,
            created_at: row.get(7)?,
        })
    }

    fn into_event(self) -> Result<Event> {
        let source = self.source.as_deref().map(str::parse).transpose()?;
        Ok(Event::new(
            self.seq,
            self.volume_id,
            self.event_type.parse()?,
            self.entry_id,
            self.content,
            split_tags(&self.tags),
            source,
            parse_datetime(&self.created_at)?,
        )?)
    }
}

const VOLUME_COLS: &str = "id, name, created_at, parent_snapshot_id";

struct VolumeRow {
    id: VolumeId,
    name: String,
    created_at: String,
    parent_snapshot_id: Option<SnapshotId>,
}

impl VolumeRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            created_at: row.get(2)?,
            parent_snapshot_id: row.get(3)?,
        })
    }

    fn into_volume(self) -> Result<Volume> {
        Ok(Volume::new(
            self.id,
            self.name,
            parse_datetime(&self.created_at)?,
            self.parent_snapshot_id,
        )?)
    }
}

const SNAPSHOT_COLS: &str = "id, volume_id, event_seq_pointer, created_at, label";

struct SnapshotRow {
    id: SnapshotId,
    volume_id: VolumeId,
    event_seq_pointer: SeqNumber,
    created_at: String,
    label: String,
}

impl SnapshotRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            volume_id: row.get(1)?,
            event_seq_pointer: row.get(2)?,
            created_at: row.get(3)?,
            label: row.get(4)?,
        })
    }

    fn into_snapshot(self) -> Result<Snapshot> {
        Ok(Snapshot::new(
            self.id,
            self.volume_id,
            self.event_seq_pointer,
            parse_datetime(&self.created_at)?,
            self.label,
        )?)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Starts a write transaction that takes the write lock up front.
/// A deferred transaction that reads before writing fails immediately with "database is locked"
/// if another connection commits in between, instead of waiting for the busy timeout.
fn write_tx(conn: &Connection) -> Result<Transaction<'_>> {
    Ok(Transaction::new_unchecked(
        conn,
        TransactionBehavior::Immediate,
    )?)
}

fn load_entry_row(conn: &Connection, entry_id: EntryId) -> Result<EntryRow> {
    conn.query_row(
        &format!("SELECT {ENTRY_COLS} FROM entries WHERE id = ?1"),
        [entry_id],
        EntryRow::from_row,
    )
    .optional()?
    .ok_or(EngineError::EntryNotFound(entry_id))
}

/// Inserts the projection row for a new entry and returns its id.
fn insert_entry_row(
    conn: &Connection,
    volume_id: VolumeId,
    data: EntryData,
    source: EntrySource,
    at: &str,
) -> Result<EntryId> {
    conn.execute(
        "INSERT INTO entries (volume_id, content, tags, created_at, updated_at, source)
            VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
        (volume_id, data.content, data.tags, at, source.as_str()),
    )?;
    Ok(conn.last_insert_rowid())
}

/// Returns `<base>`, or `<base>_2`, `<base>_3`, ... for the first name no volume uses yet.
fn unused_volume_name(conn: &Connection, base: &str) -> Result<String> {
    let mut candidate = base.to_string();
    let mut suffix = 2;
    while volume_name_taken(conn, &candidate)? {
        candidate = format!("{base}_{suffix}");
        suffix += 1;
    }
    Ok(candidate)
}

fn head_seq(conn: &Connection, volume_id: VolumeId) -> Result<SeqNumber> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM events WHERE volume_id = ?1",
        [volume_id],
        |row| row.get(0),
    )?)
}

fn volume_name_taken(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM volumes WHERE name = ?1",
        [name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Appends one event to a volume's log and returns its sequence number.
/// `data` is None only for delete events.
fn append_event(
    conn: &Connection,
    volume_id: VolumeId,
    event_type: EventType,
    entry_id: EntryId,
    data: Option<EntryData>,
    source: Option<EntrySource>,
    at: &str,
) -> Result<SeqNumber> {
    let seq = head_seq(conn, volume_id)? + 1;
    conn.execute(
        &format!("INSERT INTO events ({EVENT_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
        (
            seq,
            volume_id,
            event_type.as_str(),
            entry_id,
            data.map(|d| d.content),
            data.map_or("", |d| d.tags),
            source.map(|s| s.as_str()),
            at,
        ),
    )?;
    Ok(seq)
}

/// Applies a non-put event to the entries projection.
/// Put events insert their projection row before the event is appended.
fn project_event(
    conn: &Connection,
    event_type: EventType,
    entry_id: EntryId,
    data: Option<EntryData>,
    at: &str,
) -> Result<()> {
    match event_type {
        EventType::PutEntry => {}
        EventType::UpdateEntry | EventType::RestoreEntry => {
            conn.execute(
                "UPDATE entries SET content = ?1, tags = ?2, updated_at = ?3, deleted = 0
                    WHERE id = ?4",
                (
                    data.map(|d| d.content),
                    data.map_or("", |d| d.tags),
                    at,
                    entry_id,
                ),
            )?;
        }
        EventType::DeleteEntry => {
            conn.execute(
                "UPDATE entries SET deleted = 1, updated_at = ?1 WHERE id = ?2",
                (at, entry_id),
            )?;
        }
    }
    Ok(())
}

fn load_events(
    conn: &Connection,
    volume_id: VolumeId,
    up_to: Option<SeqNumber>,
) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {EVENT_COLS} FROM events
            WHERE volume_id = ?1 AND seq <= ?2
            ORDER BY seq ASC"
    ))?;
    let rows = stmt
        .query_map((volume_id, up_to.unwrap_or(i64::MAX)), EventRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(EventRow::into_event).collect()
}

fn create_volume_inner(
    conn: &Connection,
    name: String,
    parent_snapshot_id: Option<SnapshotId>,
) -> Result<Volume> {
    if name.trim().is_empty() {
        return Err(DomainError::EmptyVolumeName.into());
    }
    if volume_name_taken(conn, &name)? {
        return Err(EngineError::VolumeNameTaken(name));
    }
    let now = Utc::now();
    conn.execute(
        "INSERT INTO volumes (name, created_at, parent_snapshot_id) VALUES (?1, ?2, ?3)",
        (&name, now.to_rfc3339(), parent_snapshot_id),
    )?;
    let id = conn.last_insert_rowid();
    Ok(Volume::new(id, name, now, parent_snapshot_id)?)
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

/// The state of one entry as reconstructed from the event log.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplayedEntry {
    content: String,
    /// Stored form, see `join_tags`.
    tags: String,
    source: EntrySource,
    created_at: String,
    updated_at: String,
    deleted: bool,
}

/// Folds a volume's events into the state of every entry it has ever had, deleted or not.
fn replay(events: &[Event]) -> Result<BTreeMap<EntryId, ReplayedEntry>> {
    let mut state: BTreeMap<EntryId, ReplayedEntry> = BTreeMap::new();

    for ev in events {
        let at = ev.created_at().to_rfc3339();
        let bad = |why: &str| {
            EngineError::Corrupt(format!(
                "volume {} event {}: {} on entry {} {why}",
                ev.volume_id(),
                ev.seq(),
                ev.event_type().as_str(),
                ev.entry_id()
            ))
        };
        let content = || ev.content().unwrap_or_default().to_string();
        let tags = || join_tags(ev.tags());

        match ev.event_type() {
            EventType::PutEntry => {
                if state.contains_key(&ev.entry_id()) {
                    return Err(bad("which already exists"));
                }
                state.insert(
                    ev.entry_id(),
                    ReplayedEntry {
                        content: content(),
                        tags: tags(),
                        source: ev.source().unwrap_or(EntrySource::User),
                        created_at: at.clone(),
                        updated_at: at,
                        deleted: false,
                    },
                );
            }
            EventType::UpdateEntry => {
                let entry = state
                    .get_mut(&ev.entry_id())
                    .ok_or_else(|| bad("which does not exist"))?;
                if entry.deleted {
                    return Err(bad("which is deleted"));
                }
                entry.content = content();
                entry.tags = tags();
                entry.updated_at = at;
            }
            EventType::DeleteEntry => {
                let entry = state
                    .get_mut(&ev.entry_id())
                    .ok_or_else(|| bad("which does not exist"))?;
                if entry.deleted {
                    return Err(bad("which is already deleted"));
                }
                entry.deleted = true;
                entry.updated_at = at;
            }
            EventType::RestoreEntry => {
                let entry = state
                    .get_mut(&ev.entry_id())
                    .ok_or_else(|| bad("which does not exist"))?;
                if !entry.deleted {
                    return Err(bad("which is not deleted"));
                }
                entry.content = content();
                entry.tags = tags();
                entry.deleted = false;
                entry.updated_at = at;
            }
        }
    }

    Ok(state)
}

fn live_entries(
    volume_id: VolumeId,
    state: BTreeMap<EntryId, ReplayedEntry>,
) -> Result<BTreeMap<EntryId, Entry>> {
    state
        .into_iter()
        .filter(|(_, e)| !e.deleted)
        .map(|(id, e)| {
            let entry = Entry::new(
                id,
                volume_id,
                e.content,
                split_tags(&e.tags),
                parse_datetime(&e.created_at)?,
                parse_datetime(&e.updated_at)?,
                e.source,
            )?;
            Ok((id, entry))
        })
        .collect()
}

fn state_map_at(
    conn: &Connection,
    volume_id: VolumeId,
    seq: SeqNumber,
) -> Result<BTreeMap<EntryId, Entry>> {
    if seq < 0 {
        return Err(DomainError::InvalidSeqNumber.into());
    }
    let events = load_events(conn, volume_id, Some(seq))?;
    live_entries(volume_id, replay(&events)?)
}

fn rebuild_projection_inner(conn: &Connection, volume_id: VolumeId) -> Result<usize> {
    let replayed = replay(&load_events(conn, volume_id, None)?)?;

    let rows = {
        let mut stmt = conn.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM entries WHERE volume_id = ?1"
        ))?;
        stmt.query_map([volume_id], EntryRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut corrected = 0;
    for row in rows {
        match replayed.get(&row.id) {
            Some(replayed) => {
                let in_sync = row.content == replayed.content
                    && row.tags == replayed.tags
                    && row.source == replayed.source.as_str()
                    && row.created_at == replayed.created_at
                    && row.updated_at == replayed.updated_at
                    && row.deleted == replayed.deleted;
                if !in_sync {
                    conn.execute(
                        "UPDATE entries
                            SET content = ?1, tags = ?2, source = ?3, created_at = ?4,
                                updated_at = ?5, deleted = ?6
                            WHERE id = ?7",
                        (
                            &replayed.content,
                            &replayed.tags,
                            replayed.source.as_str(),
                            &replayed.created_at,
                            &replayed.updated_at,
                            replayed.deleted,
                            row.id,
                        ),
                    )?;
                    corrected += 1;
                }
            }
            // A row no event ever created is not part of the volume's history.
            None if !row.deleted => {
                conn.execute("UPDATE entries SET deleted = 1 WHERE id = ?1", [row.id])?;
                corrected += 1;
            }
            None => {}
        }
    }
    Ok(corrected)
}

// ---------------------------------------------------------------------------
// Volumes
// ---------------------------------------------------------------------------

/// Creates a new, empty volume. Volume names must be unique.
pub fn create_volume(conn: &Connection, name: String) -> Result<Volume> {
    create_volume_inner(conn, name, None)
}

/// Returns a single volume by id
pub fn get_volume(conn: &Connection, volume_id: VolumeId) -> Result<Volume> {
    conn.query_row(
        &format!("SELECT {VOLUME_COLS} FROM volumes WHERE id = ?1"),
        [volume_id],
        VolumeRow::from_row,
    )
    .optional()?
    .ok_or(EngineError::VolumeNotFound(volume_id))?
    .into_volume()
}

/// Returns all volumes in the database
pub fn list_volumes(conn: &Connection) -> Result<Vec<Volume>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {VOLUME_COLS} FROM volumes ORDER BY id ASC"
    ))?;
    let rows = stmt
        .query_map([], VolumeRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(VolumeRow::into_volume).collect()
}

/// Creates a new volume branched from a snapshot.
/// The snapshot's event history is copied into the new volume (with fresh entry ids), so the
/// branch keeps the full lineage up to that point and then evolves independently.
/// When no name is given, `<source>_clone`, `<source>_clone_2`, ... is used.
pub fn clone_volume(
    conn: &Connection,
    snapshot_id: SnapshotId,
    name: Option<String>,
) -> Result<Volume> {
    let tx = write_tx(conn)?;
    let snap = get_snapshot(&tx, snapshot_id)?;
    let source = get_volume(&tx, snap.volume_id())?;

    let name = match name {
        Some(name) => name,
        None => unused_volume_name(&tx, &format!("{}_clone", source.name()))?,
    };
    let new_volume = create_volume_inner(&tx, name, Some(snapshot_id))?;

    let events = load_events(&tx, source.id(), Some(snap.event_seq_pointer()))?;

    // Entry ids are global, so each copied entry gets a new id in the clone.
    let mut new_entry_ids: HashMap<EntryId, EntryId> = HashMap::new();
    for ev in &events {
        let at = ev.created_at().to_rfc3339();
        let tags = join_tags(ev.tags());
        let data = ev.content().map(|content| EntryData {
            content,
            tags: &tags,
        });
        if ev.event_type() == EventType::PutEntry {
            let source = ev.source().unwrap_or(EntrySource::User);
            // Put events always carry content, so `data` is Some here.
            let data = data.unwrap_or(EntryData {
                content: "",
                tags: "",
            });
            let new_id = insert_entry_row(&tx, new_volume.id(), data, source, &at)?;
            new_entry_ids.insert(ev.entry_id(), new_id);
        }
        let entry_id = *new_entry_ids.get(&ev.entry_id()).ok_or_else(|| {
            EngineError::Corrupt(format!(
                "volume {} event {} references entry {} before it was created",
                source.id(),
                ev.seq(),
                ev.entry_id()
            ))
        })?;
        append_event(
            &tx,
            new_volume.id(),
            ev.event_type(),
            entry_id,
            data,
            ev.source(),
            &at,
        )?;
    }

    rebuild_projection_inner(&tx, new_volume.id())?;
    tx.commit()?;
    Ok(new_volume)
}

/// Replays a volume's event log and rewrites any projection rows that disagree with it.
/// Returns the number of entries that were corrected (0 means the projection was intact).
pub fn rebuild_projection(conn: &Connection, volume_id: VolumeId) -> Result<usize> {
    let tx = write_tx(conn)?;
    get_volume(&tx, volume_id)?;
    let corrected = rebuild_projection_inner(&tx, volume_id)?;
    tx.commit()?;
    Ok(corrected)
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

/// Stores a new entry in a volume and records a PutEntry event.
pub fn add_entry(
    conn: &Connection,
    volume_id: VolumeId,
    content: String,
    source: EntrySource,
    tags: Vec<String>,
) -> Result<Entry> {
    if content.trim().is_empty() {
        return Err(DomainError::EmptyEntryContent.into());
    }
    let tags = normalize_tags(tags)?;
    let tx = write_tx(conn)?;
    get_volume(&tx, volume_id)?;

    let now = Utc::now();
    let at = now.to_rfc3339();
    let stored_tags = join_tags(&tags);
    let data = EntryData {
        content: &content,
        tags: &stored_tags,
    };
    let entry_id = insert_entry_row(&tx, volume_id, data, source, &at)?;
    append_event(
        &tx,
        volume_id,
        EventType::PutEntry,
        entry_id,
        Some(data),
        Some(source),
        &at,
    )?;
    tx.commit()?;

    Ok(Entry::new(
        entry_id, volume_id, content, tags, now, now, source,
    )?)
}

/// Returns a live entry by id
pub fn get_entry(conn: &Connection, entry_id: EntryId) -> Result<Entry> {
    let row = load_entry_row(conn, entry_id)?;
    if row.deleted {
        return Err(EngineError::EntryDeleted(entry_id));
    }
    row.into_entry()
}

/// Replaces the content of an entry, keeping its tags, and records an UpdateEntry event.
/// Writing identical content is a no-op and records nothing.
pub fn update_entry(conn: &Connection, entry_id: EntryId, content: String) -> Result<Entry> {
    if content.trim().is_empty() {
        return Err(DomainError::EmptyEntryContent.into());
    }
    change_entry(conn, entry_id, Some(content), None)
}

/// Replaces all tags of an entry, keeping its content, and records an UpdateEntry event.
/// Pass an empty list to remove every tag. Setting the same tags again records nothing.
pub fn tag_entry(conn: &Connection, entry_id: EntryId, tags: Vec<String>) -> Result<Entry> {
    change_entry(conn, entry_id, None, Some(normalize_tags(tags)?))
}

/// Shared by `update_entry` and `tag_entry`: whatever is None keeps its current value.
fn change_entry(
    conn: &Connection,
    entry_id: EntryId,
    new_content: Option<String>,
    new_tags: Option<Vec<String>>,
) -> Result<Entry> {
    let tx = write_tx(conn)?;
    let row = load_entry_row(&tx, entry_id)?;
    if row.deleted {
        return Err(EngineError::EntryDeleted(entry_id));
    }
    let content = new_content.unwrap_or_else(|| row.content.clone());
    let tags = new_tags.map_or_else(|| row.tags.clone(), |tags| join_tags(&tags));
    if content != row.content || tags != row.tags {
        let at = now();
        let data = EntryData {
            content: &content,
            tags: &tags,
        };
        append_event(
            &tx,
            row.volume_id,
            EventType::UpdateEntry,
            entry_id,
            Some(data),
            None,
            &at,
        )?;
        project_event(&tx, EventType::UpdateEntry, entry_id, Some(data), &at)?;
    }
    let entry = load_entry_row(&tx, entry_id)?.into_entry()?;
    tx.commit()?;
    Ok(entry)
}

/// Removes an entry from the volume's live state, recording a DeleteEntry event.
/// The entry stays in history and comes back if the volume is rolled back to before the delete.
/// Returns the entry as it was just before deletion.
pub fn delete_entry(conn: &Connection, entry_id: EntryId) -> Result<Entry> {
    let tx = write_tx(conn)?;
    let row = load_entry_row(&tx, entry_id)?;
    if row.deleted {
        return Err(EngineError::EntryDeleted(entry_id));
    }
    let volume_id = row.volume_id;
    let entry = row.into_entry()?;
    let at = now();
    append_event(
        &tx,
        volume_id,
        EventType::DeleteEntry,
        entry_id,
        None,
        None,
        &at,
    )?;
    project_event(&tx, EventType::DeleteEntry, entry_id, None, &at)?;
    tx.commit()?;
    Ok(entry)
}

/// Keeps only the entries that carry every one of `tags`. An empty list keeps everything.
fn filter_by_tags(rows: Vec<EntryRow>, tags: &[String]) -> Result<Vec<Entry>> {
    let tags = normalize_tags(tags.to_vec())?;
    rows.into_iter()
        .map(EntryRow::into_entry)
        .filter(|entry| entry.as_ref().map_or(true, |entry| entry.has_tags(&tags)))
        .collect()
}

/// Returns the live entries of a volume that carry all of `tags` (all entries when `tags` is
/// empty), ordered by insertion
pub fn list_entries(conn: &Connection, volume_id: VolumeId, tags: &[String]) -> Result<Vec<Entry>> {
    get_volume(conn, volume_id)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {ENTRY_COLS} FROM entries WHERE volume_id = ?1 AND deleted = 0 ORDER BY id ASC"
    ))?;
    let rows = stmt
        .query_map([volume_id], EntryRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    filter_by_tags(rows, tags)
}

/// Turns free text into an FTS5 query that matches entries containing any of its words,
/// including words that start with them ("auth" matches "authentication").
/// Each word is quoted, so FTS5 operators in the input are matched as plain text, and FTS5's
/// own tokenizer decides what counts as a word. Returns None when the text has no words.
fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split_whitespace()
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

/// Full-text search over a volume's live entries, best matches first, keeping only entries
/// that carry all of `tags`. Matching is case-insensitive and word-based; see `fts_query`.
pub fn search(
    conn: &Connection,
    volume_id: VolumeId,
    query: String,
    tags: &[String],
) -> Result<Vec<Entry>> {
    get_volume(conn, volume_id)?;
    let Some(match_expr) = fts_query(&query) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {ENTRY_COLS} FROM entries
            JOIN (SELECT rowid AS match_id, bm25(entries_fts) AS rank
                  FROM entries_fts WHERE entries_fts MATCH ?2)
            ON id = match_id
            WHERE volume_id = ?1 AND deleted = 0
            ORDER BY rank, id"
    ))?;
    let rows = stmt
        .query_map((volume_id, match_expr), EntryRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    filter_by_tags(rows, tags)
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Returns a volume's full event log, oldest first
pub fn list_events(conn: &Connection, volume_id: VolumeId) -> Result<Vec<Event>> {
    get_volume(conn, volume_id)?;
    load_events(conn, volume_id, None)
}

/// Returns every event that touched one entry of a volume, oldest first.
pub fn entry_history(
    conn: &Connection,
    volume_id: VolumeId,
    entry_id: EntryId,
) -> Result<Vec<Event>> {
    if load_entry_row(conn, entry_id)?.volume_id != volume_id {
        return Err(EngineError::EntryNotFound(entry_id));
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {EVENT_COLS} FROM events WHERE entry_id = ?1 ORDER BY seq ASC"
    ))?;
    let rows = stmt
        .query_map([entry_id], EventRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(EventRow::into_event).collect()
}

/// Reconstructs the live entries of a volume as they were right after event `seq`
/// (0 is the empty initial state)
pub fn state_at(conn: &Connection, volume_id: VolumeId, seq: SeqNumber) -> Result<Vec<Entry>> {
    get_volume(conn, volume_id)?;
    Ok(state_map_at(conn, volume_id, seq)?.into_values().collect())
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

/// Bookmarks the current state of a volume by pointing at its latest event.
pub fn snapshot(conn: &Connection, volume_id: VolumeId, label: String) -> Result<Snapshot> {
    if label.trim().is_empty() {
        return Err(DomainError::EmptySnapshotLabel.into());
    }
    let tx = write_tx(conn)?;
    get_volume(&tx, volume_id)?;
    let now = Utc::now();
    let event_seq_pointer = head_seq(&tx, volume_id)?;
    tx.execute(
        "INSERT INTO snapshots (volume_id, event_seq_pointer, created_at, label) VALUES (?1, ?2, ?3, ?4)",
        (volume_id, event_seq_pointer, now.to_rfc3339(), &label),
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(Snapshot::new(id, volume_id, event_seq_pointer, now, label)?)
}

/// Returns a single snapshot by id
pub fn get_snapshot(conn: &Connection, snapshot_id: SnapshotId) -> Result<Snapshot> {
    conn.query_row(
        &format!("SELECT {SNAPSHOT_COLS} FROM snapshots WHERE id = ?1"),
        [snapshot_id],
        SnapshotRow::from_row,
    )
    .optional()?
    .ok_or(EngineError::SnapshotNotFound(snapshot_id))?
    .into_snapshot()
}

/// Returns all snapshots for a given volume
pub fn list_snapshots(conn: &Connection, volume_id: VolumeId) -> Result<Vec<Snapshot>> {
    get_volume(conn, volume_id)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {SNAPSHOT_COLS} FROM snapshots WHERE volume_id = ?1 ORDER BY id ASC"
    ))?;
    let rows = stmt
        .query_map([volume_id], SnapshotRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(SnapshotRow::into_snapshot).collect()
}

/// Reconstructs the live entries of a volume as they were when the snapshot was taken
pub fn entries_at_snapshot(conn: &Connection, snapshot_id: SnapshotId) -> Result<Vec<Entry>> {
    let snap = get_snapshot(conn, snapshot_id)?;
    state_at(conn, snap.volume_id(), snap.event_seq_pointer())
}

/// Compares two points in a volume's history
pub fn diff(
    conn: &Connection,
    volume_id: VolumeId,
    from_seq: SeqNumber,
    to_seq: SeqNumber,
) -> Result<MemoryDiff> {
    get_volume(conn, volume_id)?;
    let before = state_map_at(conn, volume_id, from_seq)?;
    let mut after = state_map_at(conn, volume_id, to_seq)?;

    let mut result = MemoryDiff {
        from_seq,
        to_seq,
        ..MemoryDiff::default()
    };
    for (id, old) in before {
        match after.remove(&id) {
            None => result.removed.push(old),
            Some(new) if new.content() != old.content() || new.tags() != old.tags() => {
                result.modified.push(EntryModification {
                    before: old,
                    after: new,
                })
            }
            Some(_) => {}
        }
    }
    result.added = after.into_values().collect();
    Ok(result)
}

/// Compares a snapshot against a later (or earlier) snapshot of the same volume,
/// or against the volume's current state when `to` is None
pub fn diff_snapshots(
    conn: &Connection,
    from: SnapshotId,
    to: Option<SnapshotId>,
) -> Result<MemoryDiff> {
    let from_snap = get_snapshot(conn, from)?;
    let to_seq = match to {
        Some(to_id) => {
            let to_snap = get_snapshot(conn, to_id)?;
            if to_snap.volume_id() != from_snap.volume_id() {
                return Err(EngineError::SnapshotVolumeMismatch { from, to: to_id });
            }
            to_snap.event_seq_pointer()
        }
        None => head_seq(conn, from_snap.volume_id())?,
    };
    diff(
        conn,
        from_snap.volume_id(),
        from_snap.event_seq_pointer(),
        to_seq,
    )
}

/// Returns the snapshot's volume to the state it had when the snapshot was taken.
/// History is never rewritten: compensating update/delete/restore events are appended,
/// so the rollback itself shows up in the log and can be undone by rolling back again.
pub fn rollback_to_snapshot(conn: &Connection, snapshot_id: SnapshotId) -> Result<RollbackReport> {
    let tx = write_tx(conn)?;
    let snap = get_snapshot(&tx, snapshot_id)?;
    let volume_id = snap.volume_id();

    let events = load_events(&tx, volume_id, None)?;
    let from_seq = events.last().map_or(0, Event::seq);
    let current = replay(&events)?;
    let target = state_map_at(&tx, volume_id, snap.event_seq_pointer())?;

    let at = now();
    let mut events_appended = 0;
    for (id, cur) in &current {
        let wanted = target
            .get(id)
            .map(|want| (want.content(), join_tags(want.tags())));
        let event_type = match &wanted {
            Some(_) if cur.deleted => Some(EventType::RestoreEntry),
            Some((content, tags)) if cur.content != *content || cur.tags != *tags => {
                Some(EventType::UpdateEntry)
            }
            Some(_) => None,
            None if !cur.deleted => Some(EventType::DeleteEntry),
            None => None,
        };
        if let Some(event_type) = event_type {
            let data = wanted
                .as_ref()
                .map(|(content, tags)| EntryData { content, tags });
            append_event(&tx, volume_id, event_type, *id, data, None, &at)?;
            project_event(&tx, event_type, *id, data, &at)?;
            events_appended += 1;
        }
    }

    let to_seq = head_seq(&tx, volume_id)?;
    tx.commit()?;
    Ok(RollbackReport {
        volume_id,
        snapshot_id,
        from_seq,
        to_seq,
        events_appended,
    })
}
