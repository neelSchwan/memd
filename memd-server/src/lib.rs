//! HTTP API for memd: a thin layer that maps JSON requests onto `engine_api` calls.
//!
//! `handle` takes a request and returns a response without touching the network, so the whole
//! API can be tested directly. `main.rs` only connects it to a socket.
//!
//! Routes (bodies and responses are JSON; `{id}` values are numbers):
//!
//! | Method | Path                                      | Body / query                     |
//! | ------ | ----------------------------------------- | -------------------------------- |
//! | GET    | /api/volumes                              |                                  |
//! | POST   | /api/volumes                              | `{name}`                         |
//! | GET    | /api/volumes/{id}/entries                 | `?tag=a&tag=b`                   |
//! | POST   | /api/volumes/{id}/entries                 | `{content, source?, tags?}`      |
//! | GET    | /api/volumes/{id}/search                  | `?q=words&tag=a`                 |
//! | GET    | /api/volumes/{id}/events                  |                                  |
//! | GET    | /api/volumes/{id}/entries/{entry}/history |                                  |
//! | GET    | /api/volumes/{id}/snapshots               |                                  |
//! | POST   | /api/volumes/{id}/snapshots               | `{label}`                        |
//! | GET    | /api/volumes/{id}/diff/{other}            |                                  |
//! | POST   | /api/volumes/{id}/merge/{target}          | `?dry_run=true`                  |
//! | GET    | /api/entries/{id}                         |                                  |
//! | PATCH  | /api/entries/{id}                         | `{content?, tags?}`              |
//! | DELETE | /api/entries/{id}                         |                                  |
//! | GET    | /api/snapshots/{id}/entries               |                                  |
//! | GET    | /api/snapshots/{id}/diff                  | `?to={snapshot}` (default: now)  |
//! | POST   | /api/snapshots/{id}/rollback              |                                  |
//! | POST   | /api/snapshots/{id}/branch                | `{name?}`                        |
//!
//! Errors are returned as `{"error": "message"}` with a 4xx or 5xx status.
//!
//! The server has no authentication and is meant for local use only. Two checks keep web pages
//! in the user's browser from using it: requests must name a local `Host` (which defeats DNS
//! rebinding), and requests that change anything must be sent as `application/json`, which a
//! page on another origin can't do without a CORS preflight that this server never answers.

use core_types::EntrySource;
use engine_api::{Connection, EngineError};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// The parts of an HTTP request the API looks at.
pub struct Request<'a> {
    pub method: &'a str,
    /// Path plus optional query string, as sent by the client.
    pub url: &'a str,
    pub host: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub body: &'a str,
}

/// An HTTP response; `body` is JSON for every route.
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl Response {
    fn json(status: u16, value: &Value) -> Response {
        Response {
            status,
            content_type: "application/json",
            body: value.to_string(),
        }
    }

    /// A JSON error response: `{"error": message}`.
    pub fn error(status: u16, message: String) -> Response {
        Response::json(status, &json!({ "error": message }))
    }
}

/// An error on its way to becoming an HTTP error response.
struct ApiError {
    status: u16,
    message: String,
}

impl ApiError {
    fn bad_request(message: String) -> ApiError {
        ApiError {
            status: 400,
            message,
        }
    }
}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> ApiError {
        let status = match e {
            EngineError::VolumeNotFound(_)
            | EngineError::EntryNotFound(_)
            | EngineError::SnapshotNotFound(_) => 404,
            EngineError::EntryDeleted(_) | EngineError::VolumeNameTaken(_) => 409,
            EngineError::Domain(_)
            | EngineError::SnapshotVolumeMismatch { .. }
            | EngineError::VolumesNotRelated { .. } => 400,
            EngineError::UnsupportedSchemaVersion(_)
            | EngineError::Corrupt(_)
            | EngineError::Sqlite(_) => 500,
        };
        ApiError {
            status,
            message: e.to_string(),
        }
    }
}

/// True for `localhost`, `127.0.0.1` and `[::1]`, with or without a port.
fn is_local_host(host: &str) -> bool {
    let name = match host.rsplit_once(':') {
        Some((name, port)) if !name.is_empty() && port.parse::<u16>().is_ok() => name,
        _ => host,
    };
    matches!(name, "localhost" | "127.0.0.1" | "[::1]")
}

/// Handles one request.
pub fn handle(conn: &Connection, request: &Request) -> Response {
    if !request.host.is_some_and(is_local_host) {
        return Response::error(
            403,
            "memd-server only accepts requests to localhost".to_string(),
        );
    }
    let is_json = request
        .content_type
        .is_some_and(|value| value.split(';').next().unwrap_or("").trim() == "application/json");
    if request.method != "GET" && !is_json {
        return Response::error(
            415,
            "requests that change data must be sent as application/json".to_string(),
        );
    }
    match route(conn, request.method, request.url, request.body) {
        Ok(value) => Response::json(200, &value),
        Err(e) => Response::error(e.status, e.message),
    }
}

#[derive(Deserialize)]
struct CreateVolume {
    name: String,
}

#[derive(Deserialize)]
struct AddEntry {
    content: String,
    source: Option<EntrySource>,
    #[serde(default)]
    tags: Vec<String>,
}

/// Fields left out keep their current value.
#[derive(Deserialize)]
struct ChangeEntry {
    content: Option<String>,
    tags: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct CreateSnapshot {
    label: String,
}

#[derive(Deserialize)]
struct Branch {
    name: Option<String>,
}

fn route(conn: &Connection, method: &str, url: &str, body: &str) -> Result<Value, ApiError> {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let query = Query::parse(query);
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();

    match (method, segments.as_slice()) {
        ("GET", ["api", "volumes"]) => to_json(engine_api::list_volumes(conn)?),
        ("POST", ["api", "volumes"]) => {
            let request: CreateVolume = parse_body(body)?;
            to_json(engine_api::create_volume(conn, request.name)?)
        }
        ("GET", ["api", "volumes", id, "entries"]) => to_json(engine_api::list_entries(
            conn,
            number(id)?,
            &query.all("tag"),
        )?),
        ("POST", ["api", "volumes", id, "entries"]) => {
            let request: AddEntry = parse_body(body)?;
            let source = request.source.unwrap_or(EntrySource::User);
            to_json(engine_api::add_entry(
                conn,
                number(id)?,
                request.content,
                source,
                request.tags,
            )?)
        }
        ("GET", ["api", "volumes", id, "search"]) => {
            let words = query.first("q").unwrap_or_default();
            to_json(engine_api::search(
                conn,
                number(id)?,
                words,
                &query.all("tag"),
            )?)
        }
        ("GET", ["api", "volumes", id, "events"]) => {
            to_json(engine_api::list_events(conn, number(id)?)?)
        }
        ("GET", ["api", "volumes", id, "entries", entry_id, "history"]) => to_json(
            engine_api::entry_history(conn, number(id)?, number(entry_id)?)?,
        ),
        ("GET", ["api", "volumes", id, "snapshots"]) => {
            to_json(engine_api::list_snapshots(conn, number(id)?)?)
        }
        ("POST", ["api", "volumes", id, "snapshots"]) => {
            let request: CreateSnapshot = parse_body(body)?;
            to_json(engine_api::snapshot(conn, number(id)?, request.label)?)
        }
        ("GET", ["api", "volumes", id, "diff", other]) => {
            to_json(engine_api::diff_volumes(conn, number(id)?, number(other)?)?)
        }
        ("POST", ["api", "volumes", id, "merge", target]) => {
            let dry_run = query.first("dry_run").is_some_and(|value| value == "true");
            to_json(engine_api::merge_volumes(
                conn,
                number(id)?,
                number(target)?,
                dry_run,
            )?)
        }
        ("GET", ["api", "entries", id]) => to_json(engine_api::get_entry(conn, number(id)?)?),
        ("PATCH", ["api", "entries", id]) => {
            let request: ChangeEntry = parse_body(body)?;
            to_json(engine_api::edit_entry(
                conn,
                number(id)?,
                request.content,
                request.tags,
            )?)
        }
        ("DELETE", ["api", "entries", id]) => to_json(engine_api::delete_entry(conn, number(id)?)?),
        ("GET", ["api", "snapshots", id, "entries"]) => {
            to_json(engine_api::entries_at_snapshot(conn, number(id)?)?)
        }
        ("GET", ["api", "snapshots", id, "diff"]) => {
            let to = query.first("to").map(|to| number(&to)).transpose()?;
            to_json(engine_api::diff_snapshots(conn, number(id)?, to)?)
        }
        ("POST", ["api", "snapshots", id, "rollback"]) => {
            to_json(engine_api::rollback_to_snapshot(conn, number(id)?)?)
        }
        ("POST", ["api", "snapshots", id, "branch"]) => {
            let request: Branch = parse_body(body)?;
            to_json(engine_api::clone_volume(conn, number(id)?, request.name)?)
        }
        _ => Err(ApiError {
            status: 404,
            message: format!("no route for {method} {path}"),
        }),
    }
}

fn to_json(value: impl Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|e| ApiError {
        status: 500,
        message: format!("could not encode response: {e}"),
    })
}

/// Parses a JSON request body. An empty body counts as `{}`, so requests whose fields are all
/// optional can leave it out.
fn parse_body<T: DeserializeOwned>(body: &str) -> Result<T, ApiError> {
    let body = if body.trim().is_empty() { "{}" } else { body };
    serde_json::from_str(body).map_err(|e| ApiError::bad_request(format!("invalid body: {e}")))
}

fn number(segment: &str) -> Result<i64, ApiError> {
    segment
        .parse()
        .map_err(|_| ApiError::bad_request(format!("'{segment}' is not a valid id")))
}

/// Decoded query string parameters, in order. Keys may repeat (`?tag=a&tag=b`).
struct Query(Vec<(String, String)>);

impl Query {
    fn parse(query: &str) -> Query {
        Query(
            query
                .split('&')
                .filter(|pair| !pair.is_empty())
                .map(|pair| {
                    let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                    (percent_decode(key), percent_decode(value))
                })
                .collect(),
        )
    }

    fn all(&self, key: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, value)| value.clone())
            .collect()
    }

    fn first(&self, key: &str) -> Option<String> {
        self.all(key).into_iter().next()
    }
}

/// Decodes `%XX` escapes and `+` (a space in query strings). Malformed escapes are kept as is.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = bytes
            .get(i + 1..i + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[i], escaped) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                i += 3;
            }
            (b'+', _) => {
                decoded.push(b' ');
                i += 1;
            }
            (byte, _) => {
                decoded.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
