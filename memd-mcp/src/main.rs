use core_types::{
    Entry, EntryModification, EntrySource, Event, EventType, MemoryDiff, MergeReport, Snapshot,
    Volume,
};
use engine_api::Connection;
use rmcp::{
    Json, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct MemdServer {
    tool_router: ToolRouter<Self>,
    conn: Arc<Mutex<Connection>>,
}

#[tool_handler(router = self.tool_router)]
impl rmcp::ServerHandler for MemdServer {}

#[tool_router(router = tool_router)]
impl MemdServer {
    pub fn new() -> anyhow::Result<Self> {
        let path = std::env::var("MEMD_DB").unwrap_or_else(|_| "memd.db".to_string());
        let conn = engine_api::open_db(path)?;
        Ok(Self {
            tool_router: Self::tool_router(),
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Create a new memory volume with the given name
    #[tool(
        name = "create_volume",
        description = "Create a persistent memory namespace for a user, project, or long-running task. Use this before storing memory when you need separation between contexts. Do not use this for one-off chat turns; prefer reusing an existing volume when the context is the same."
    )]
    pub async fn create_volume(
        &self,
        params: Parameters<CreateVolumeRequest>,
    ) -> Result<Json<VolumeResponse>, String> {
        let request = params.0;
        let conn = self.conn.lock().unwrap();
        let vol = engine_api::create_volume(&conn, request.name).map_err(|e| e.to_string())?;

        Ok(Json(VolumeResponse::from(&vol)))
    }

    /// List all memory volumes
    #[tool(
        name = "list_volumes",
        description = "List available memory namespaces. Use this when you do not yet know which volume to read from or write to. Do not use this as memory retrieval; use search_memory or list_memory for stored facts."
    )]
    pub async fn list_volumes(&self) -> Result<Json<VolumeListResponse>, String> {
        let conn = self.conn.lock().unwrap();

        let vol_list = engine_api::list_volumes(&conn).map_err(|e| e.to_string())?;

        Ok(Json(VolumeListResponse {
            volumes: vol_list.iter().map(VolumeResponse::from).collect(),
        }))
    }

    /// Store a durable memory entry in a volume
    #[tool(
        name = "remember",
        description = "Store a durable fact in a memory volume. Use for user preferences, project decisions, constraints, and context likely to matter in future turns. Do not use for temporary reasoning, trivial back-and-forth chat, or information that does not need long-term persistence."
    )]
    pub async fn remember(
        &self,
        params: Parameters<RememberRequest>,
    ) -> Result<Json<EntryResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let entry = engine_api::add_entry(
            &conn,
            request.volume_id,
            request.content,
            request.source,
            request.tags,
        )
        .map_err(|e| e.to_string())?;

        Ok(Json(EntryResponse::from(&entry)))
    }

    /// List all memory entries in a volume
    #[tool(
        name = "list_memory",
        description = "Return the stored memory entries in a volume in insertion order, optionally only those carrying all of the given tags. Use this when you need full inspection or auditing of the memory state. Do not use this when you only need relevant entries; use search_memory for targeted retrieval."
    )]
    pub async fn list_memory(
        &self,
        params: Parameters<ListMemoryRequest>,
    ) -> Result<Json<EntryListResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let entries = engine_api::list_entries(&conn, request.volume_id, &request.tags)
            .map_err(|e| e.to_string())?;

        Ok(Json(EntryListResponse {
            entries: entries.iter().map(EntryResponse::from).collect(),
        }))
    }

    /// Search memory entries in a volume by content substring
    #[tool(
        name = "search_memory",
        description = "Retrieve memory entries relevant to a search phrase within one volume. Use this before answering when prior stored context may affect the response. The search looks at entry content only; to narrow by tag, pass the tags parameter. Do not use this for web search, current events, or facts never written to this memory system."
    )]
    pub async fn search_memory(
        &self,
        params: Parameters<SearchMemoryRequest>,
    ) -> Result<Json<EntryListResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let entries = engine_api::search(&conn, request.volume_id, request.query, &request.tags)
            .map_err(|e| e.to_string())?;

        Ok(Json(EntryListResponse {
            entries: entries.iter().map(EntryResponse::from).collect(),
        }))
    }

    /// Snapshot the current state of a volume
    #[tool(
        name = "create_snapshot",
        description = "Create a point-in-time checkpoint for a memory volume. Use this after important milestones so you can revisit or branch later. Do not use this as a substitute for adding memory entries; snapshots preserve state but do not add new facts."
    )]
    pub async fn create_snapshot(
        &self,
        params: Parameters<CreateSnapshotRequest>,
    ) -> Result<Json<SnapshotResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let snapshot = engine_api::snapshot(&conn, request.volume_id, request.label)
            .map_err(|e| e.to_string())?;

        Ok(Json(SnapshotResponse::from(&snapshot)))
    }

    /// List all snapshots for a volume
    #[tool(
        name = "list_snapshots",
        description = "List checkpoints for a memory volume. Use this to review memory history or pick a snapshot to branch from. Do not use this to retrieve memory facts directly; use list_memory or search_memory for content."
    )]
    pub async fn list_snapshots(
        &self,
        params: Parameters<ListSnapshotsRequest>,
    ) -> Result<Json<SnapshotListResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let snapshots =
            engine_api::list_snapshots(&conn, request.volume_id).map_err(|e| e.to_string())?;

        Ok(Json(SnapshotListResponse {
            snapshots: snapshots.iter().map(SnapshotResponse::from).collect(),
        }))
    }

    /// Branch a new volume from a snapshot
    #[tool(
        name = "branch_from_snapshot",
        description = "Create a new volume branched from a snapshot so you can continue from an earlier memory state without mutating the original timeline. Use this for alternative plans or what-if paths. Do not use this for normal ongoing updates to the same memory context."
    )]
    pub async fn branch_from_snapshot(
        &self,
        params: Parameters<BranchFromSnapshotRequest>,
    ) -> Result<Json<VolumeResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let vol = engine_api::clone_volume(&conn, request.snapshot_id, request.name)
            .map_err(|e| e.to_string())?;

        Ok(Json(VolumeResponse::from(&vol)))
    }

    /// Replace the content of a memory entry
    #[tool(
        name = "update_memory",
        description = "Correct or refine a stored memory entry in place. Use this when a fact you stored earlier has changed or was wrong, instead of adding a contradicting entry. The previous content stays in the history and can be restored by rolling back."
    )]
    pub async fn update_memory(
        &self,
        params: Parameters<UpdateMemoryRequest>,
    ) -> Result<Json<EntryResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let entry = engine_api::update_entry(&conn, request.entry_id, request.content)
            .map_err(|e| e.to_string())?;

        Ok(Json(EntryResponse::from(&entry)))
    }

    /// Replace the tags of a memory entry
    #[tool(
        name = "tag_memory",
        description = "Replace all tags on a memory entry, keeping its content. Use tags to group related memories (for example by topic or project area) so they can be filtered in list_memory and search_memory. Pass an empty list to remove all tags."
    )]
    pub async fn tag_memory(
        &self,
        params: Parameters<TagMemoryRequest>,
    ) -> Result<Json<EntryResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let entry = engine_api::tag_entry(&conn, request.entry_id, request.tags)
            .map_err(|e| e.to_string())?;

        Ok(Json(EntryResponse::from(&entry)))
    }

    /// Delete a memory entry
    #[tool(
        name = "forget",
        description = "Remove a memory entry that is obsolete, wrong, or that the user asked you to forget. The entry disappears from list_memory and search_memory but remains in memory_history, and rolling back to an earlier snapshot brings it back."
    )]
    pub async fn forget(
        &self,
        params: Parameters<ForgetRequest>,
    ) -> Result<Json<EntryResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let entry = engine_api::delete_entry(&conn, request.entry_id).map_err(|e| e.to_string())?;

        Ok(Json(EntryResponse::from(&entry)))
    }

    /// Show the event log for a volume or one entry
    #[tool(
        name = "memory_history",
        description = "Return the append-only change log for a memory volume, or for a single entry when entry_id is given. Use this to audit how memory evolved, find when a fact was added, changed, or forgotten, or pick a point to snapshot or roll back to."
    )]
    pub async fn memory_history(
        &self,
        params: Parameters<MemoryHistoryRequest>,
    ) -> Result<Json<EventListResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let events = match request.entry_id {
            Some(entry_id) => engine_api::entry_history(&conn, request.volume_id, entry_id),
            None => engine_api::list_events(&conn, request.volume_id),
        }
        .map_err(|e| e.to_string())?;

        Ok(Json(EventListResponse {
            events: events.iter().map(EventResponse::from).collect(),
        }))
    }

    /// Show the entries a volume had at a snapshot
    #[tool(
        name = "memory_at_snapshot",
        description = "Return the memory entries exactly as they were when a snapshot was taken, without changing anything. Use this to inspect an earlier state before deciding whether to branch from it or roll back to it."
    )]
    pub async fn memory_at_snapshot(
        &self,
        params: Parameters<SnapshotRequest>,
    ) -> Result<Json<EntryListResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let entries = engine_api::entries_at_snapshot(&conn, request.snapshot_id)
            .map_err(|e| e.to_string())?;

        Ok(Json(EntryListResponse {
            entries: entries.iter().map(EntryResponse::from).collect(),
        }))
    }

    /// Compare two snapshots, or a snapshot with the current state
    #[tool(
        name = "diff_snapshots",
        description = "Show which memory entries were added, removed, or modified between two snapshots of the same volume, or between a snapshot and the current state when to_snapshot_id is omitted. Use this to review what changed since a checkpoint."
    )]
    pub async fn diff_snapshots(
        &self,
        params: Parameters<DiffSnapshotsRequest>,
    ) -> Result<Json<DiffResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let diff =
            engine_api::diff_snapshots(&conn, request.from_snapshot_id, request.to_snapshot_id)
                .map_err(|e| e.to_string())?;

        Ok(Json(DiffResponse::from(&diff)))
    }

    /// Compare the current entries of two volumes
    #[tool(
        name = "diff_volumes",
        description = "Compare the current memories of two volumes, for example a branch and the volume it was branched from. Entries are matched across branches, so an edited entry shows as modified rather than as removed and added. Swap the IDs to see the other direction."
    )]
    pub async fn diff_volumes(
        &self,
        params: Parameters<DiffVolumesRequest>,
    ) -> Result<Json<VolumeDiffResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let diff = engine_api::diff_volumes(&conn, request.from_volume_id, request.to_volume_id)
            .map_err(|e| e.to_string())?;

        Ok(Json(VolumeDiffResponse {
            added: diff.added.iter().map(EntryResponse::from).collect(),
            removed: diff.removed.iter().map(EntryResponse::from).collect(),
            modified: diff
                .modified
                .iter()
                .map(ModifiedEntryResponse::from)
                .collect(),
        }))
    }

    /// Merge one volume's changes into a related volume
    #[tool(
        name = "merge_volumes",
        description = "Apply the memory changes made on one volume (usually a branch) to a related volume, such as the volume it was branched from. History is preserved: changes are recorded as new events on the target. Entries changed differently on both sides are left alone and returned as conflicts; to take the source's version, update the target's entry with update_memory. Run with dry_run first to preview."
    )]
    pub async fn merge_volumes(
        &self,
        params: Parameters<MergeVolumesRequest>,
    ) -> Result<Json<MergeResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let report = engine_api::merge_volumes(
            &conn,
            request.source_volume_id,
            request.target_volume_id,
            request.dry_run,
        )
        .map_err(|e| e.to_string())?;

        Ok(Json(MergeResponse::from(&report)))
    }

    /// Roll a volume back to a snapshot
    #[tool(
        name = "rollback_to_snapshot",
        description = "Return a memory volume to the exact state it had at a snapshot, undoing later additions, edits, and deletions. History is preserved: the rollback is recorded as new events, so it can itself be undone by rolling back to a later snapshot. Use branch_from_snapshot instead if you want to keep the current state too."
    )]
    pub async fn rollback_to_snapshot(
        &self,
        params: Parameters<SnapshotRequest>,
    ) -> Result<Json<RollbackResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let report = engine_api::rollback_to_snapshot(&conn, request.snapshot_id)
            .map_err(|e| e.to_string())?;

        Ok(Json(RollbackResponse {
            volume_id: report.volume_id,
            snapshot_id: report.snapshot_id,
            from_seq: report.from_seq,
            to_seq: report.to_seq,
            events_appended: report.events_appended,
        }))
    }
}

impl From<&Volume> for VolumeResponse {
    fn from(v: &Volume) -> Self {
        VolumeResponse {
            id: v.id(),
            name: v.name().to_string(),
            created_at: v.created_at().to_rfc3339(),
            parent_snapshot_id: v.parent_snapshot_id(),
        }
    }
}

impl From<&Snapshot> for SnapshotResponse {
    fn from(s: &Snapshot) -> Self {
        SnapshotResponse {
            id: s.id(),
            volume_id: s.volume_id(),
            event_seq_pointer: s.event_seq_pointer(),
            label: s.label().to_string(),
            created_at: s.created_at().to_rfc3339(),
        }
    }
}

impl From<&Entry> for EntryResponse {
    fn from(e: &Entry) -> Self {
        EntryResponse {
            id: e.id(),
            volume_id: e.volume_id(),
            content: e.content().to_string(),
            tags: e.tags().to_vec(),
            created_at: e.created_at().to_rfc3339(),
            updated_at: e.updated_at().to_rfc3339(),
            source: e.source(),
        }
    }
}

impl From<&Event> for EventResponse {
    fn from(ev: &Event) -> Self {
        EventResponse {
            seq: ev.seq(),
            volume_id: ev.volume_id(),
            event_type: ev.event_type(),
            entry_id: ev.entry_id(),
            content: ev.content().map(str::to_string),
            tags: ev.tags().to_vec(),
            source: ev.source(),
            created_at: ev.created_at().to_rfc3339(),
        }
    }
}

impl From<&MemoryDiff> for DiffResponse {
    fn from(d: &MemoryDiff) -> Self {
        DiffResponse {
            from_seq: d.from_seq,
            to_seq: d.to_seq,
            added: d.added.iter().map(EntryResponse::from).collect(),
            removed: d.removed.iter().map(EntryResponse::from).collect(),
            modified: d.modified.iter().map(ModifiedEntryResponse::from).collect(),
        }
    }
}

impl From<&EntryModification> for ModifiedEntryResponse {
    fn from(m: &EntryModification) -> Self {
        ModifiedEntryResponse {
            entry_id: m.before.id(),
            before: EntryResponse::from(&m.before),
            after: EntryResponse::from(&m.after),
        }
    }
}

impl From<&MergeReport> for MergeResponse {
    fn from(r: &MergeReport) -> Self {
        let entry = |e: &Option<Entry>| e.as_ref().map(EntryResponse::from);
        MergeResponse {
            dry_run: r.dry_run,
            base_volume_id: r.base_volume_id,
            base_seq: r.base_seq,
            events_appended: r.events_appended,
            added: r.added.iter().map(EntryResponse::from).collect(),
            removed: r.removed.iter().map(EntryResponse::from).collect(),
            modified: r.modified.iter().map(ModifiedEntryResponse::from).collect(),
            conflicts: r
                .conflicts
                .iter()
                .map(|c| ConflictResponse {
                    base: entry(&c.base),
                    source: entry(&c.source),
                    target: entry(&c.target),
                })
                .collect(),
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CreateVolumeRequest {
    #[schemars(
        description = "Human-readable volume name for the memory namespace, such as a user, project, or task identifier."
    )]
    pub name: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct VolumeResponse {
    pub id: i64,
    pub name: String,
    pub created_at: String,
    /// Snapshot this volume was branched from, if any.
    pub parent_snapshot_id: Option<i64>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ListMemoryRequest {
    #[schemars(
        description = "Numeric ID of the memory volume to inspect. Use list_volumes first if the ID is unknown."
    )]
    pub volume_id: i64,
    #[serde(default)]
    #[schemars(description = "Optional. Only return entries that have all of these tags.")]
    pub tags: Vec<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct RememberRequest {
    #[schemars(
        description = "Numeric ID of the memory volume where this durable memory should be stored."
    )]
    pub volume_id: i64,
    #[schemars(
        description = "Durable memory content to store, such as a user preference, project decision, constraint, or long-running context."
    )]
    pub content: String,
    #[schemars(
        description = "Who produced the memory: user, agent, or tool. Use the source that best reflects origin."
    )]
    pub source: EntrySource,
    #[serde(default)]
    #[schemars(
        description = "Optional short labels for grouping and filtering, such as a topic or project area. Lowercase, no spaces or commas."
    )]
    pub tags: Vec<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct EntryResponse {
    pub id: i64,
    pub volume_id: i64,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub source: EntrySource,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SearchMemoryRequest {
    #[schemars(
        description = "Numeric ID of the memory volume to search. Use list_volumes first if the ID is unknown."
    )]
    pub volume_id: i64,
    #[schemars(
        description = "Keywords to search for. Entries matching any keyword (or a word starting with it) are returned, best matches first. Prefer specific names, decisions, or constraints."
    )]
    pub query: String,
    #[serde(default)]
    #[schemars(description = "Optional. Only return entries that have all of these tags.")]
    pub tags: Vec<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CreateSnapshotRequest {
    #[schemars(description = "Numeric ID of the memory volume to checkpoint.")]
    pub volume_id: i64,
    #[schemars(
        description = "Human-readable checkpoint label describing the milestone or state being captured."
    )]
    pub label: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct BranchFromSnapshotRequest {
    #[schemars(
        description = "Numeric ID of the snapshot to branch from. Use list_snapshots to discover valid snapshot IDs."
    )]
    pub snapshot_id: i64,
    #[schemars(
        description = "Optional name for the new volume. Defaults to the source volume's name with a _clone suffix."
    )]
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ListSnapshotsRequest {
    #[schemars(
        description = "Numeric ID of the memory volume whose checkpoints you want to review."
    )]
    pub volume_id: i64,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SnapshotResponse {
    pub id: i64,
    pub volume_id: i64,
    pub event_seq_pointer: i64,
    pub label: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct VolumeListResponse {
    pub volumes: Vec<VolumeResponse>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct EntryListResponse {
    pub entries: Vec<EntryResponse>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SnapshotListResponse {
    pub snapshots: Vec<SnapshotResponse>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct UpdateMemoryRequest {
    #[schemars(
        description = "Numeric ID of the memory entry to update. Use list_memory or search_memory to find it."
    )]
    pub entry_id: i64,
    #[schemars(description = "The full replacement content for the entry.")]
    pub content: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TagMemoryRequest {
    #[schemars(
        description = "Numeric ID of the memory entry to tag. Use list_memory or search_memory to find it."
    )]
    pub entry_id: i64,
    #[schemars(
        description = "The complete new list of tags. Lowercase, no spaces or commas. An empty list removes all tags."
    )]
    pub tags: Vec<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ForgetRequest {
    #[schemars(
        description = "Numeric ID of the memory entry to remove. Use list_memory or search_memory to find it."
    )]
    pub entry_id: i64,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct MemoryHistoryRequest {
    #[schemars(description = "Numeric ID of the memory volume whose change log to return.")]
    pub volume_id: i64,
    #[schemars(
        description = "Optional entry ID. When set, only the changes to that entry are returned."
    )]
    pub entry_id: Option<i64>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SnapshotRequest {
    #[schemars(
        description = "Numeric ID of the snapshot. Use list_snapshots to discover valid snapshot IDs."
    )]
    pub snapshot_id: i64,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct DiffSnapshotsRequest {
    #[schemars(description = "Snapshot ID marking the earlier point to compare from.")]
    pub from_snapshot_id: i64,
    #[schemars(
        description = "Snapshot ID to compare to, from the same volume. Omit to compare against the current state."
    )]
    pub to_snapshot_id: Option<i64>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct EventResponse {
    pub seq: i64,
    pub volume_id: i64,
    pub event_type: EventType,
    pub entry_id: i64,
    /// New content for put, update and restore events.
    pub content: Option<String>,
    /// The entry's full tag list after put, update and restore events.
    pub tags: Vec<String>,
    pub source: Option<EntrySource>,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct EventListResponse {
    pub events: Vec<EventResponse>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ModifiedEntryResponse {
    pub entry_id: i64,
    pub before: EntryResponse,
    pub after: EntryResponse,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct DiffVolumesRequest {
    #[schemars(description = "Volume ID to compare from, e.g. the parent volume.")]
    pub from_volume_id: i64,
    #[schemars(description = "Volume ID to compare to, e.g. a branch of it.")]
    pub to_volume_id: i64,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct VolumeDiffResponse {
    /// Entries only in the `to` volume.
    pub added: Vec<EntryResponse>,
    /// Entries only in the `from` volume.
    pub removed: Vec<EntryResponse>,
    pub modified: Vec<ModifiedEntryResponse>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct MergeVolumesRequest {
    #[schemars(description = "Volume whose changes should be applied, usually a branch.")]
    pub source_volume_id: i64,
    #[schemars(description = "Volume to apply the changes to, usually the branch's parent.")]
    pub target_volume_id: i64,
    #[serde(default)]
    #[schemars(description = "When true, only report what would change. Defaults to false.")]
    pub dry_run: bool,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ConflictResponse {
    /// The entry as both sides last agreed on it; null if it did not exist then.
    pub base: Option<EntryResponse>,
    /// The source's version; null if the source deleted it.
    pub source: Option<EntryResponse>,
    /// The target's version, which the merge kept; null if the target deleted it.
    pub target: Option<EntryResponse>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct MergeResponse {
    pub dry_run: bool,
    /// The common state both sides were compared against.
    pub base_volume_id: i64,
    pub base_seq: i64,
    /// Events appended to the target (or that would be, in a dry run).
    pub events_appended: usize,
    pub added: Vec<EntryResponse>,
    pub removed: Vec<EntryResponse>,
    /// `before` is the target's version, `after` the source's.
    pub modified: Vec<ModifiedEntryResponse>,
    /// Entries changed differently on both sides; left as they are in the target.
    pub conflicts: Vec<ConflictResponse>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct DiffResponse {
    pub from_seq: i64,
    pub to_seq: i64,
    pub added: Vec<EntryResponse>,
    pub removed: Vec<EntryResponse>,
    pub modified: Vec<ModifiedEntryResponse>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct RollbackResponse {
    pub volume_id: i64,
    pub snapshot_id: i64,
    pub from_seq: i64,
    pub to_seq: i64,
    pub events_appended: usize,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = MemdServer::new()?;
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
