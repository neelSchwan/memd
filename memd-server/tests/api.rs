//! Exercises the HTTP API through `handle`, without a network socket.

use engine_api::Connection;
use memd_server::{Request, handle};
use serde_json::{Value, json};

fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    engine_api::init_db(&conn).unwrap();
    conn
}

/// Sends a JSON request to localhost, the way the web UI does, and returns (status, body).
fn send(conn: &Connection, method: &str, url: &str, body: Value) -> (u16, Value) {
    let body = if body.is_null() {
        String::new()
    } else {
        body.to_string()
    };
    let response = handle(
        conn,
        &Request {
            method,
            url,
            host: Some("localhost:7878"),
            content_type: Some("application/json"),
            body: &body,
        },
    );
    (
        response.status,
        serde_json::from_str(&response.body).unwrap(),
    )
}

/// Sends a request that must succeed and returns its JSON body.
fn ok(conn: &Connection, method: &str, url: &str, body: Value) -> Value {
    let (status, value) = send(conn, method, url, body);
    assert_eq!(status, 200, "{method} {url} -> {value}");
    value
}

fn contents(entries: &Value) -> Vec<&str> {
    entries
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["content"].as_str().unwrap())
        .collect()
}

#[test]
fn volumes_and_entries() {
    let conn = db();
    let volume = ok(&conn, "POST", "/api/volumes", json!({ "name": "work" }));
    assert_eq!(volume["name"], "work");
    assert_eq!(
        ok(&conn, "GET", "/api/volumes", Value::Null)[0]["id"],
        volume["id"]
    );

    let entries_url = format!("/api/volumes/{}/entries", volume["id"]);
    let entry = ok(
        &conn,
        "POST",
        &entries_url,
        json!({ "content": "use postgres", "tags": ["DB"] }),
    );
    assert_eq!(entry["source"], "User");
    assert_eq!(entry["tags"], json!(["db"]));
    ok(
        &conn,
        "POST",
        &entries_url,
        json!({ "content": "deploy fridays", "source": "Agent" }),
    );

    assert_eq!(
        contents(&ok(&conn, "GET", &entries_url, Value::Null)).len(),
        2
    );
    assert_eq!(
        contents(&ok(
            &conn,
            "GET",
            &format!("{entries_url}?tag=db"),
            Value::Null
        )),
        ["use postgres"]
    );

    let entry_url = format!("/api/entries/{}", entry["id"]);
    let changed = ok(
        &conn,
        "PATCH",
        &entry_url,
        json!({ "content": "use postgres 16", "tags": ["db", "prod"] }),
    );
    assert_eq!(changed["content"], "use postgres 16");
    assert_eq!(changed["tags"], json!(["db", "prod"]));
    assert_eq!(
        ok(&conn, "GET", &entry_url, Value::Null)["content"],
        "use postgres 16"
    );

    let history_url = format!(
        "/api/volumes/{}/entries/{}/history",
        volume["id"], entry["id"]
    );
    // Content and tags changed in one PATCH are one event.
    assert_eq!(
        ok(&conn, "GET", &history_url, Value::Null)
            .as_array()
            .unwrap()
            .len(),
        2
    );

    ok(&conn, "DELETE", &entry_url, Value::Null);
    let (status, body) = send(&conn, "GET", &entry_url, Value::Null);
    assert_eq!(
        (status, body["error"].as_str().unwrap()),
        (409, "entry 1 has been deleted")
    );
}

#[test]
fn search_decodes_the_query_string() {
    let conn = db();
    ok(&conn, "POST", "/api/volumes", json!({ "name": "v" }));
    ok(
        &conn,
        "POST",
        "/api/volumes/1/entries",
        json!({ "content": "auth bug in middleware", "tags": ["bug"] }),
    );
    ok(
        &conn,
        "POST",
        "/api/volumes/1/entries",
        json!({ "content": "café opens at nine" }),
    );

    let found = ok(
        &conn,
        "GET",
        "/api/volumes/1/search?q=middle+auth",
        Value::Null,
    );
    assert_eq!(contents(&found), ["auth bug in middleware"]);
    let found = ok(
        &conn,
        "GET",
        "/api/volumes/1/search?q=caf%C3%A9",
        Value::Null,
    );
    assert_eq!(contents(&found), ["café opens at nine"]);
    let found = ok(
        &conn,
        "GET",
        "/api/volumes/1/search?q=bug&tag=none",
        Value::Null,
    );
    assert_eq!(found, json!([]));
}

#[test]
fn snapshots_rollback_branch_and_merge() {
    let conn = db();
    ok(&conn, "POST", "/api/volumes", json!({ "name": "main" }));
    let entry = ok(
        &conn,
        "POST",
        "/api/volumes/1/entries",
        json!({ "content": "v1" }),
    );
    let snap = ok(
        &conn,
        "POST",
        "/api/volumes/1/snapshots",
        json!({ "label": "start" }),
    );
    ok(
        &conn,
        "PATCH",
        &format!("/api/entries/{}", entry["id"]),
        json!({ "content": "v2" }),
    );

    let diff = ok(
        &conn,
        "GET",
        &format!("/api/snapshots/{}/diff", snap["id"]),
        Value::Null,
    );
    assert_eq!(diff["modified"][0]["after"]["content"], "v2");
    let at_snapshot = ok(
        &conn,
        "GET",
        &format!("/api/snapshots/{}/entries", snap["id"]),
        Value::Null,
    );
    assert_eq!(contents(&at_snapshot), ["v1"]);

    let rollback = ok(
        &conn,
        "POST",
        &format!("/api/snapshots/{}/rollback", snap["id"]),
        Value::Null,
    );
    assert_eq!(rollback["events_appended"], 1);
    assert_eq!(
        contents(&ok(&conn, "GET", "/api/volumes/1/entries", Value::Null)),
        ["v1"]
    );
    assert_eq!(
        ok(&conn, "GET", "/api/volumes/1/events", Value::Null)
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        ok(&conn, "GET", "/api/volumes/1/snapshots", Value::Null)[0]["label"],
        "start"
    );

    // An empty body is fine when every field is optional.
    let branch = ok(
        &conn,
        "POST",
        &format!("/api/snapshots/{}/branch", snap["id"]),
        Value::Null,
    );
    assert_eq!(branch["name"], "main_clone");
    let branch_entries = format!("/api/volumes/{}/entries", branch["id"]);
    ok(
        &conn,
        "POST",
        &branch_entries,
        json!({ "content": "from branch" }),
    );

    let diff = ok(
        &conn,
        "GET",
        &format!("/api/volumes/1/diff/{}", branch["id"]),
        Value::Null,
    );
    assert_eq!(contents(&diff["added"]), ["from branch"]);

    let merge_url = format!("/api/volumes/{}/merge/1", branch["id"]);
    let preview = ok(
        &conn,
        "POST",
        &format!("{merge_url}?dry_run=true"),
        Value::Null,
    );
    assert_eq!(preview["dry_run"], true);
    assert_eq!(
        contents(&ok(&conn, "GET", "/api/volumes/1/entries", Value::Null)),
        ["v1"]
    );
    ok(&conn, "POST", &merge_url, Value::Null);
    assert_eq!(
        contents(&ok(&conn, "GET", "/api/volumes/1/entries", Value::Null)),
        ["v1", "from branch"]
    );
}

#[test]
fn errors_have_matching_status_codes() {
    let conn = db();
    ok(&conn, "POST", "/api/volumes", json!({ "name": "v" }));

    let cases = [
        (
            "GET",
            "/api/nope",
            Value::Null,
            404,
            "no route for GET /api/nope",
        ),
        (
            "GET",
            "/api/volumes/9/entries",
            Value::Null,
            404,
            "volume 9 not found",
        ),
        (
            "GET",
            "/api/volumes/x/entries",
            Value::Null,
            400,
            "'x' is not a valid id",
        ),
        (
            "POST",
            "/api/volumes",
            json!({ "name": "v" }),
            409,
            "a volume named 'v' already exists",
        ),
        (
            "POST",
            "/api/volumes/1/entries",
            json!({ "content": "" }),
            400,
            "entry content must not be empty",
        ),
        (
            "POST",
            "/api/volumes",
            json!({}),
            400,
            "invalid body: missing field `name` at line 1 column 2",
        ),
    ];
    for (method, url, body, status, message) in cases {
        let (actual_status, response) = send(&conn, method, url, body);
        assert_eq!(
            (actual_status, response["error"].as_str().unwrap()),
            (status, message),
            "{method} {url}"
        );
    }
}

#[test]
fn rejects_requests_web_pages_could_forge() {
    let conn = db();
    let request = |host: Option<&'static str>, content_type: Option<&'static str>| Request {
        method: "POST",
        url: "/api/volumes",
        host,
        content_type,
        body: r#"{"name": "planted"}"#,
    };

    // A form post or no-cors fetch from another site can only send "simple" content types.
    let form = handle(&conn, &request(Some("127.0.0.1:7878"), Some("text/plain")));
    assert_eq!(form.status, 415);
    // DNS rebinding reaches the server under the attacker's host name.
    let rebound = handle(
        &conn,
        &request(Some("evil.example:7878"), Some("application/json")),
    );
    assert_eq!(rebound.status, 403);
    assert_eq!(
        handle(&conn, &request(None, Some("application/json"))).status,
        403
    );
    assert_eq!(ok(&conn, "GET", "/api/volumes", Value::Null), json!([]));

    for host in ["localhost", "[::1]:7878", "127.0.0.1:80"] {
        let allowed = handle(
            &conn,
            &request(Some(host), Some("application/json; charset=utf-8")),
        );
        assert_ne!(allowed.status, 403, "{host}");
    }
}

#[test]
fn patch_changes_content_and_tags_together_or_not_at_all() {
    let conn = db();
    ok(&conn, "POST", "/api/volumes", json!({ "name": "v" }));
    ok(
        &conn,
        "POST",
        "/api/volumes/1/entries",
        json!({ "content": "old" }),
    );

    let (status, _) = send(
        &conn,
        "PATCH",
        "/api/entries/1",
        json!({ "content": "new", "tags": ["bad tag"] }),
    );
    assert_eq!(status, 400);
    assert_eq!(
        ok(&conn, "GET", "/api/entries/1", Value::Null)["content"],
        "old"
    );

    // An empty patch changes nothing and returns the entry.
    assert_eq!(
        ok(&conn, "PATCH", "/api/entries/1", json!({}))["content"],
        "old"
    );
}

#[test]
fn serves_the_web_ui_at_the_root() {
    let response = handle(
        &db(),
        &Request {
            method: "GET",
            url: "/",
            host: Some("localhost:7878"),
            content_type: None,
            body: "",
        },
    );
    assert_eq!(response.status, 200);
    assert!(response.content_type.starts_with("text/html"));
    assert!(response.body.contains("<title>memd</title>"));
}
