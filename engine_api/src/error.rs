use core_types::{DomainError, EntryId, SnapshotId, VolumeId};
use std::fmt;

/// Errors returned by the storage engine.
#[derive(Debug)]
pub enum EngineError {
    Domain(DomainError),
    VolumeNotFound(VolumeId),
    EntryNotFound(EntryId),
    SnapshotNotFound(SnapshotId),
    /// The entry exists but was deleted; it can only come back via rollback.
    EntryDeleted(EntryId),
    /// A volume with this name already exists.
    VolumeNameTaken(String),
    /// Two snapshots were expected to belong to the same volume.
    SnapshotVolumeMismatch {
        from: SnapshotId,
        to: SnapshotId,
    },
    /// Merge needs volumes that share history through cloning.
    VolumesNotRelated {
        source: VolumeId,
        target: VolumeId,
    },
    /// The database was created by a newer version of memd.
    UnsupportedSchemaVersion(i64),
    /// Stored data that cannot be interpreted, e.g. an event log that does not replay.
    Corrupt(String),
    Sqlite(rusqlite::Error),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Domain(e) => write!(f, "{e}"),
            EngineError::VolumeNotFound(id) => write!(f, "volume {id} not found"),
            EngineError::EntryNotFound(id) => write!(f, "entry {id} not found"),
            EngineError::SnapshotNotFound(id) => write!(f, "snapshot {id} not found"),
            EngineError::EntryDeleted(id) => write!(f, "entry {id} has been deleted"),
            EngineError::VolumeNameTaken(name) => {
                write!(f, "a volume named '{name}' already exists")
            }
            EngineError::SnapshotVolumeMismatch { from, to } => {
                write!(f, "snapshots {from} and {to} belong to different volumes")
            }
            EngineError::VolumesNotRelated { source, target } => write!(
                f,
                "volumes {source} and {target} share no history; only volumes related by cloning can be merged"
            ),
            EngineError::UnsupportedSchemaVersion(version) => write!(
                f,
                "database schema version {version} is newer than this build supports; upgrade memd"
            ),
            EngineError::Corrupt(msg) => write!(f, "corrupt data: {msg}"),
            EngineError::Sqlite(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EngineError::Domain(e) => Some(e),
            EngineError::Sqlite(e) => Some(e),
            _ => None,
        }
    }
}

impl From<DomainError> for EngineError {
    fn from(e: DomainError) -> Self {
        EngineError::Domain(e)
    }
}

impl From<rusqlite::Error> for EngineError {
    fn from(e: rusqlite::Error) -> Self {
        EngineError::Sqlite(e)
    }
}

pub type Result<T> = std::result::Result<T, EngineError>;
