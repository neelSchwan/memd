use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
/// Domain-level errors: only things we can validate locally (no DB lookups).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    EmptyVolumeName,
    EmptyEntryContent,
    EmptySnapshotLabel,
    InvalidSeqNumber,
    MissingEventContent,
    UnknownEventType(String),
    UnknownEntrySource(String),
    InvalidTag(String),
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DomainError::EmptyVolumeName => write!(f, "volume name must not be empty"),
            DomainError::EmptyEntryContent => write!(f, "entry content must not be empty"),
            DomainError::EmptySnapshotLabel => write!(f, "snapshot label must not be empty"),
            DomainError::InvalidSeqNumber => write!(f, "sequence number out of range"),
            DomainError::MissingEventContent => {
                write!(f, "put, update and restore events must carry content")
            }
            DomainError::UnknownEventType(s) => write!(f, "unknown event type '{s}'"),
            DomainError::UnknownEntrySource(s) => write!(f, "unknown entry source '{s}'"),
            DomainError::InvalidTag(tag) => write!(
                f,
                "invalid tag '{tag}': tags must be non-empty and contain no spaces or commas"
            ),
        }
    }
}

impl std::error::Error for DomainError {}

/// v0 IDs: switch to UUID later
pub type VolumeId = i64;
pub type EntryId = i64;
pub type SnapshotId = i64;
pub type SeqNumber = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum EntrySource {
    User,
    Agent,
    Tool,
}

impl EntrySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            EntrySource::User => "user",
            EntrySource::Agent => "agent",
            EntrySource::Tool => "tool",
        }
    }
}

impl std::str::FromStr for EntrySource {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "user" => Ok(EntrySource::User),
            "agent" => Ok(EntrySource::Agent),
            "tool" => Ok(EntrySource::Tool),
            _ => Err(DomainError::UnknownEntrySource(s.to_string())),
        }
    }
}

/// Cleans up a list of tags: trims and lowercases each one, then sorts and removes duplicates,
/// so the same set of tags is always stored the same way.
pub fn normalize_tags(tags: Vec<String>) -> Result<Vec<String>, DomainError> {
    let mut normalized = tags
        .into_iter()
        .map(|tag| {
            let tag = tag.trim().to_lowercase();
            if tag.is_empty() || tag.contains(',') || tag.contains(char::is_whitespace) {
                Err(DomainError::InvalidTag(tag))
            } else {
                Ok(tag)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

/// A Volume is an isolated memory namespace.
/// Volumes created by branching remember the snapshot they were branched from.
#[derive(Debug, Clone, Serialize)]
pub struct Volume {
    id: VolumeId,
    name: String,
    created_at: DateTime<Utc>,
    parent_snapshot_id: Option<SnapshotId>,
}

impl Volume {
    pub fn new(
        id: VolumeId,
        name: String,
        created_at: DateTime<Utc>,
        parent_snapshot_id: Option<SnapshotId>,
    ) -> Result<Self, DomainError> {
        if name.trim().is_empty() {
            return Err(DomainError::EmptyVolumeName);
        }
        Ok(Self {
            id,
            name,
            created_at,
            parent_snapshot_id,
        })
    }

    pub fn id(&self) -> VolumeId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn parent_snapshot_id(&self) -> Option<SnapshotId> {
        self.parent_snapshot_id
    }
}

/// An Entry is a single memory item stored in a Volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    id: EntryId,
    volume_id: VolumeId,
    content: String,
    tags: Vec<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    source: EntrySource,
}

impl Entry {
    pub fn new(
        id: EntryId,
        volume_id: VolumeId,
        content: String,
        tags: Vec<String>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
        source: EntrySource,
    ) -> Result<Self, DomainError> {
        if content.trim().is_empty() {
            return Err(DomainError::EmptyEntryContent);
        }
        Ok(Self {
            id,
            volume_id,
            content,
            tags: normalize_tags(tags)?,
            created_at,
            updated_at,
            source,
        })
    }

    pub fn id(&self) -> EntryId {
        self.id
    }

    pub fn volume_id(&self) -> VolumeId {
        self.volume_id
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    /// Normalized tags, sorted alphabetically.
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    /// True when the entry carries every one of the given (normalized) tags.
    pub fn has_tags(&self, tags: &[String]) -> bool {
        tags.iter().all(|tag| self.tags.contains(tag))
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }

    pub fn source(&self) -> EntrySource {
        self.source
    }
}

/// Every change to a volume is recorded as one of these events.
/// The entries table is a projection that can always be rebuilt by replaying them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    /// A new entry was created.
    PutEntry,
    /// An existing entry's content or tags were replaced.
    UpdateEntry,
    /// An entry was removed from the live state (history is kept).
    DeleteEntry,
    /// A deleted entry was brought back, e.g. by a rollback.
    RestoreEntry,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventType::PutEntry => "put_entry",
            EventType::UpdateEntry => "update_entry",
            EventType::DeleteEntry => "delete_entry",
            EventType::RestoreEntry => "restore_entry",
        }
    }

    /// Whether events of this type must carry the entry's new content and tags.
    pub fn carries_content(&self) -> bool {
        !matches!(self, EventType::DeleteEntry)
    }
}

impl std::str::FromStr for EventType {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "put_entry" => Ok(EventType::PutEntry),
            "update_entry" => Ok(EventType::UpdateEntry),
            "delete_entry" => Ok(EventType::DeleteEntry),
            "restore_entry" => Ok(EventType::RestoreEntry),
            _ => Err(DomainError::UnknownEventType(s.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    seq: SeqNumber,
    volume_id: VolumeId,
    event_type: EventType,
    entry_id: EntryId,
    content: Option<String>,
    tags: Vec<String>,
    source: Option<EntrySource>,
    created_at: DateTime<Utc>,
}

impl Event {
    // One argument per column of the events table; a wrapper struct would only add ceremony.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        seq: SeqNumber,
        volume_id: VolumeId,
        event_type: EventType,
        entry_id: EntryId,
        content: Option<String>,
        tags: Vec<String>,
        source: Option<EntrySource>,
        created_at: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        // seq must be >= 1
        if seq < 1 {
            return Err(DomainError::InvalidSeqNumber);
        }

        if event_type.carries_content() {
            match &content {
                None => return Err(DomainError::MissingEventContent),
                Some(c) if c.trim().is_empty() => return Err(DomainError::EmptyEntryContent),
                Some(_) => {}
            }
        }

        Ok(Self {
            seq,
            volume_id,
            event_type,
            entry_id,
            content,
            tags: normalize_tags(tags)?,
            source,
            created_at,
        })
    }

    pub fn seq(&self) -> SeqNumber {
        self.seq
    }

    pub fn volume_id(&self) -> VolumeId {
        self.volume_id
    }

    pub fn event_type(&self) -> EventType {
        self.event_type
    }

    pub fn entry_id(&self) -> EntryId {
        self.entry_id
    }

    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    pub fn source(&self) -> Option<EntrySource> {
        self.source
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

/// A Snapshot is a bookmark: it points to an event sequence number.
/// A pointer of 0 marks the empty state before any event.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    id: SnapshotId,
    volume_id: VolumeId,
    event_seq_pointer: SeqNumber,
    created_at: DateTime<Utc>,
    label: String,
}

impl Snapshot {
    pub fn new(
        id: SnapshotId,
        volume_id: VolumeId,
        event_seq_pointer: SeqNumber,
        created_at: DateTime<Utc>,
        label: String,
    ) -> Result<Self, DomainError> {
        if event_seq_pointer < 0 {
            return Err(DomainError::InvalidSeqNumber);
        }

        if label.trim().is_empty() {
            return Err(DomainError::EmptySnapshotLabel);
        }

        Ok(Self {
            id,
            volume_id,
            event_seq_pointer,
            created_at,
            label,
        })
    }

    pub fn id(&self) -> SnapshotId {
        self.id
    }

    pub fn volume_id(&self) -> VolumeId {
        self.volume_id
    }

    pub fn event_seq_pointer(&self) -> SeqNumber {
        self.event_seq_pointer
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

/// An entry whose content or tags differ between two points in a volume's history.
#[derive(Debug, Clone, Serialize)]
pub struct EntryModification {
    pub before: Entry,
    pub after: Entry,
}

/// The difference between two points in a single volume's history.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryDiff {
    pub from_seq: SeqNumber,
    pub to_seq: SeqNumber,
    pub added: Vec<Entry>,
    pub removed: Vec<Entry>,
    pub modified: Vec<EntryModification>,
}

impl MemoryDiff {
    /// True when both points in history have identical live entries.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.modified.is_empty()
    }
}

/// Outcome of rolling a volume back to a snapshot.
/// Rollback never rewrites history: it appends compensating events.
#[derive(Debug, Clone, Serialize)]
pub struct RollbackReport {
    pub volume_id: VolumeId,
    pub snapshot_id: SnapshotId,
    /// Head of the log before the rollback.
    pub from_seq: SeqNumber,
    /// Head of the log after the rollback.
    pub to_seq: SeqNumber,
    pub events_appended: usize,
}

/// The difference between the live entries of two volumes. Entries are matched across volumes
/// by origin, so an entry and its copy in a branch count as the same entry.
#[derive(Debug, Clone, Default, Serialize)]
pub struct VolumeDiff {
    pub from_volume_id: VolumeId,
    pub to_volume_id: VolumeId,
    /// Entries only in `to`.
    pub added: Vec<Entry>,
    /// Entries only in `from`.
    pub removed: Vec<Entry>,
    /// `before` is the entry in `from`, `after` the entry in `to`.
    pub modified: Vec<EntryModification>,
}

impl VolumeDiff {
    /// True when both volumes have the same live entries.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.modified.is_empty()
    }
}

/// An entry that both sides of a merge changed differently since their common base.
/// Merge leaves it untouched in the target. None means absent or deleted on that side.
#[derive(Debug, Clone, Serialize)]
pub struct MergeConflict {
    pub base: Option<Entry>,
    pub source: Option<Entry>,
    pub target: Option<Entry>,
}

/// Outcome of merging one volume into another.
/// Merge never rewrites history: it appends events to the target, and a real merge that changed
/// anything or found conflicts records a "merged into <target>" snapshot on the source.
#[derive(Debug, Clone, Serialize)]
pub struct MergeReport {
    pub source_volume_id: VolumeId,
    pub target_volume_id: VolumeId,
    /// The common state both sides were compared against.
    pub base_volume_id: VolumeId,
    pub base_seq: SeqNumber,
    /// When true nothing was written; the report shows what a real merge would do.
    pub dry_run: bool,
    /// Source entries added (or restored) in the target.
    pub added: Vec<Entry>,
    /// Target entries deleted because the source deleted them.
    pub removed: Vec<Entry>,
    /// `before` is the target's entry, `after` the source's.
    pub modified: Vec<EntryModification>,
    pub conflicts: Vec<MergeConflict>,
    /// Events appended to the target (or that would be, in a dry run).
    pub events_appended: usize,
}
