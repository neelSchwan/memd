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
    #[tool(name = "create_volume", description = "Create a new memory volume")]
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
    #[tool(name = "list_volumes", description = "List all memory volumes")]
    pub async fn list_volumes(&self) -> Result<Json<VolumeListResponse>, String> {
        let conn = self.conn.lock().unwrap();

        let vol_list = engine_api::list_volumes(&conn).map_err(|e| e.to_string())?;

        Ok(Json(VolumeListResponse {
            volumes: vol_list.iter().map(VolumeResponse::from).collect(),
        }))
    }

    /// Add a new memory entry to a volume
    #[tool(name = "add_entry", description = "Add a new memory entry to a volume")]
    pub async fn add_entry(
        &self,
        params: Parameters<AddEntryRequest>,
    ) -> Result<Json<EntryResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let entry =
            engine_api::add_entry(&conn, request.volume_id, request.content, request.source)
                .map_err(|e| e.to_string())?;

        Ok(Json(EntryResponse::from(&entry)))
    }

    /// List all entries in a volume
    #[tool(name = "list_entries", description = "List all entries in a volume")]
    pub async fn list_entries(
        &self,
        params: Parameters<VolumeIdRequest>,
    ) -> Result<Json<EntryListResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let entries =
            engine_api::list_entries(&conn, request.volume_id).map_err(|e| e.to_string())?;

        Ok(Json(EntryListResponse {
            entries: entries.iter().map(EntryResponse::from).collect(),
        }))
    }

    /// Search entries in a volume by content substring
    #[tool(
        name = "search_entries",
        description = "Search entries in a volume by content substring"
    )]
    pub async fn search_entries(
        &self,
        params: Parameters<SearchRequest>,
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
        description = "Snapshot the current state of a volume"
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
        description = "List all snapshots for a volume"
    )]
    pub async fn list_snapshots(
        &self,
        params: Parameters<VolumeIdRequest>,
    ) -> Result<Json<SnapshotListResponse>, String> {
        let conn = self.conn.lock().unwrap();
        let request = params.0;

        let snapshots =
            engine_api::list_snapshots(&conn, request.volume_id).map_err(|e| e.to_string())?;

        Ok(Json(SnapshotListResponse {
            snapshots: snapshots.iter().map(SnapshotResponse::from).collect(),
        }))
    }

    /// Clone a volume from a snapshot
    #[tool(name = "clone_volume", description = "Clone a volume from a snapshot")]
    pub async fn clone_volume(
        &self,
        params: Parameters<CloneVolumeRequest>,
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
    pub name: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct VolumeResponse {
    pub id: i64,
    pub name: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct VolumeIdRequest {
    pub volume_id: i64,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct AddEntryRequest {
    pub volume_id: i64,
    pub content: String,
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
pub struct SearchRequest {
    pub volume_id: i64,
    pub query: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CreateSnapshotRequest {
    pub volume_id: i64,
    pub label: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CloneVolumeRequest {
    pub snapshot_id: i64,
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
