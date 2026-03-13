use core_types::{Entry, EntrySource, Snapshot, Volume};
use engine_api::init_db;
use rmcp::{
    Json, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use rusqlite::Connection;
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
        let conn = Connection::open("memd.db")?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        init_db(&conn)?;
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

        let entry =
            engine_api::add_entry(&conn, request.volume_id, request.content, request.source)
                .map_err(|e| e.to_string())?;

        Ok(Json(EntryResponse::from(&entry)))
    }

    /// List all memory entries in a volume
    #[tool(
        name = "list_memory",
        description = "Return all stored memory entries in a volume in insertion order. Use this when you need full inspection or auditing of the memory state. Do not use this when you only need relevant entries; use search_memory for targeted retrieval."
    )]
    pub async fn list_memory(
        &self,
        params: Parameters<ListMemoryRequest>,
    ) -> Result<Json<EntryListResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let entries =
            engine_api::list_entries(&conn, request.volume_id).map_err(|e| e.to_string())?;

        Ok(Json(EntryListResponse {
            entries: entries.iter().map(EntryResponse::from).collect(),
        }))
    }

    /// Search memory entries in a volume by content substring
    #[tool(
        name = "search_memory",
        description = "Retrieve memory entries relevant to a search phrase within one volume. Use this before answering when prior stored context may affect the response. Do not use this for web search, current events, or facts never written to this memory system."
    )]
    pub async fn search_memory(
        &self,
        params: Parameters<SearchMemoryRequest>,
    ) -> Result<Json<EntryListResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let entries = engine_api::search(&conn, request.volume_id, request.query)
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

        let vol =
            engine_api::clone_volume(&conn, request.snapshot_id).map_err(|e| e.to_string())?;

        Ok(Json(VolumeResponse::from(&vol)))
    }
}

impl From<&Volume> for VolumeResponse {
    fn from(v: &Volume) -> Self {
        VolumeResponse {
            id: v.id(),
            name: v.name().to_string(),
            created_at: v.created_at().to_rfc3339(),
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
            created_at: e.created_at().to_rfc3339(),
            source: e.source(),
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
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ListMemoryRequest {
    #[schemars(
        description = "Numeric ID of the memory volume to inspect. Use list_volumes first if the ID is unknown."
    )]
    pub volume_id: i64,
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
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct EntryResponse {
    pub id: i64,
    pub volume_id: i64,
    pub content: String,
    pub created_at: String,
    pub source: EntrySource,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SearchMemoryRequest {
    #[schemars(
        description = "Numeric ID of the memory volume to search. Use list_volumes first if the ID is unknown."
    )]
    pub volume_id: i64,
    #[schemars(
        description = "Search phrase for relevant stored memory. Prefer specific keywords, names, decisions, or constraints."
    )]
    pub query: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CreateSnapshotRequest {
    #[schemars(
        description = "Numeric ID of the memory volume to checkpoint."
    )]
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
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = MemdServer::new()?;
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
