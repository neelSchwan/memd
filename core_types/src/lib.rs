use chrono::{DateTime, Utc};
use std::fmt;

/// Domain-level errors: only things we can validate locally (no DB lookups).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    EmptyVolumeName,
    EmptyEntryContent,
    EmptySnapshotLabel,
    InvalidSeqNumber,
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DomainError::EmptyVolumeName => write!(f, "volume name must not be empty"),
            DomainError::EmptyEntryContent => write!(f, "entry content must not be empty"),
            DomainError::EmptySnapshotLabel => write!(f, "snapshot label must not be empty"),
            DomainError::InvalidSeqNumber => write!(f, "sequence number must be >= 1"),
        }
    }
}

impl std::error::Error for DomainError {}

/// v0 IDs: switch to UUID later
pub type VolumeId = i64;
pub type EntryId = i64;
pub type SnapshotId = i64;
pub type SeqNumber = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntrySource {
    User,
    Agent,
    Tool,
}

/// A Volume is an isolated memory namespace.
#[derive(Debug, Clone)]
pub struct Volume {
    id: VolumeId,
    name: String,
    created_at: DateTime<Utc>,
}

impl Volume {
    pub fn new(id: VolumeId, name: String, created_at: DateTime<Utc>) -> Result<Self, DomainError> {
        if name.trim().is_empty() {
            return Err(DomainError::EmptyVolumeName);
        }
        Ok(Self {
            id,
            name,
            created_at,
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
}

/// An Entry is a single memory item stored in a Volume.
#[derive(Debug, Clone)]
pub struct Entry {
    id: EntryId,
    volume_id: VolumeId,
    content: String,
    created_at: DateTime<Utc>,
    source: EntrySource,
}

impl Entry {
    pub fn new(
        id: EntryId,
        volume_id: VolumeId,
        content: String,
        created_at: DateTime<Utc>,
        source: EntrySource,
    ) -> Result<Self, DomainError> {
        if content.trim().is_empty() {
            return Err(DomainError::EmptyEntryContent);
        }
        Ok(Self {
            id,
            volume_id,
            content,
            created_at,
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

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn source(&self) -> EntrySource {
        self.source
    }
}

/// We keep it simple: the only event type is "PutEntry".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    PutEntry,
}

#[derive(Debug, Clone)]
pub struct Event {
    seq: SeqNumber,
    volume_id: VolumeId,
    event_type: EventType,
    entry_id: EntryId,
    created_at: DateTime<Utc>,
}

impl Event {
    pub fn new(
        seq: SeqNumber,
        volume_id: VolumeId,
        event_type: EventType,
        entry_id: EntryId,
        created_at: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        // seq must be >= 1
        if seq == 0 {
            return Err(DomainError::InvalidSeqNumber);
        }

        Ok(Self {
            seq,
            volume_id,
            event_type,
            entry_id,
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

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

/// A Snapshot is a bookmark: it points to an event sequence number.
#[derive(Debug, Clone)]
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
        // require pointer >= 1
        if event_seq_pointer == 0 {
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
