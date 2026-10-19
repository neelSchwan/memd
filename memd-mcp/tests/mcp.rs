//! Starts the real `memd-mcp` binary and talks to it over stdio, the way an MCP client does.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    db_path: PathBuf,
}

impl Server {
    /// Starts the server on a fresh database file and completes the MCP handshake.
    fn start(test_name: &str) -> Server {
        let db_path = std::env::temp_dir().join(format!(
            "memd-mcp-test-{}-{test_name}.db",
            std::process::id()
        ));
        remove_db_files(&db_path);

        let mut child = Command::new(env!("CARGO_BIN_EXE_memd-mcp"))
            .env("MEMD_DB", &db_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start memd-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut server = Server {
            child,
            stdin,
            stdout,
            next_id: 1,
            db_path,
        };

        server.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "memd-mcp-test", "version": "0" }
            }),
        );
        server.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        server
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Sends a request and returns the `result` of its response.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));

        // The server may interleave notifications; skip anything that isn't our response.
        loop {
            let mut line = String::new();
            let bytes_read = self.stdout.read_line(&mut line).unwrap();
            assert!(
                bytes_read > 0,
                "server closed stdout while waiting for {method}"
            );
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == id {
                assert!(message["error"].is_null(), "{method} failed: {message}");
                return message["result"].clone();
            }
        }
    }

    fn call_tool(&mut self, tool: &str, args: Value) -> Value {
        self.request("tools/call", json!({ "name": tool, "arguments": args }))
    }

    /// Calls a tool that is expected to succeed and returns its structured result.
    fn call(&mut self, tool: &str, args: Value) -> Value {
        let result = self.call_tool(tool, args);
        assert_ne!(result["isError"], true, "{tool} failed: {result}");
        result["structuredContent"].clone()
    }

    /// Calls a tool that is expected to fail and returns its error message.
    fn call_error(&mut self, tool: &str, args: Value) -> String {
        let result = self.call_tool(tool, args);
        assert_eq!(result["isError"], true, "{tool} should fail: {result}");
        result["content"][0]["text"].as_str().unwrap().to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        remove_db_files(&self.db_path);
    }
}

fn remove_db_files(db_path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut path = db_path.as_os_str().to_owned();
        path.push(suffix);
        let _ = std::fs::remove_file(path);
    }
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
fn lists_all_tools() {
    let mut server = Server::start("lists_all_tools");

    let result = server.request("tools/list", json!({}));

    let mut names: Vec<&str> = result["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "branch_from_snapshot",
            "create_snapshot",
            "create_volume",
            "diff_snapshots",
            "diff_volumes",
            "forget",
            "list_memory",
            "list_snapshots",
            "list_volumes",
            "memory_at_snapshot",
            "memory_history",
            "merge_volumes",
            "remember",
            "rollback_to_snapshot",
            "search_memory",
            "tag_memory",
            "update_memory",
        ]
    );
}

#[test]
fn remember_change_and_roll_back() {
    let mut server = Server::start("remember_change_and_roll_back");
    let volume_id = server.call("create_volume", json!({ "name": "project" }))["id"].clone();
    let editor = server.call(
        "remember",
        json!({ "volume_id": volume_id, "content": "User prefers vim", "source": "User", "tags": ["prefs"] }),
    );
    let database = server.call(
        "remember",
        json!({ "volume_id": volume_id, "content": "Project uses SQLite", "source": "Agent", "tags": ["stack"] }),
    );
    assert_eq!(editor["tags"], json!(["prefs"]));

    let found = server.call(
        "search_memory",
        json!({ "volume_id": volume_id, "query": "vim" }),
    );
    assert_eq!(contents(&found["entries"]), ["User prefers vim"]);

    let updated = server.call(
        "update_memory",
        json!({ "entry_id": editor["id"], "content": "User prefers helix" }),
    );
    assert_eq!(updated["content"], "User prefers helix");

    let snapshot = server.call(
        "create_snapshot",
        json!({ "volume_id": volume_id, "label": "before cleanup" }),
    );
    server.call("forget", json!({ "entry_id": database["id"] }));

    let diff = server.call(
        "diff_snapshots",
        json!({ "from_snapshot_id": snapshot["id"] }),
    );
    assert_eq!(contents(&diff["removed"]), ["Project uses SQLite"]);
    assert_eq!(diff["added"], json!([]));
    assert_eq!(diff["modified"], json!([]));

    let rollback = server.call(
        "rollback_to_snapshot",
        json!({ "snapshot_id": snapshot["id"] }),
    );
    assert_eq!(rollback["events_appended"], 1);

    let listed = server.call("list_memory", json!({ "volume_id": volume_id }));
    assert_eq!(
        contents(&listed["entries"]),
        ["User prefers helix", "Project uses SQLite"]
    );

    let history = server.call("memory_history", json!({ "volume_id": volume_id }));
    let event_types: Vec<&str> = history["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["event_type"].as_str().unwrap())
        .collect();
    assert_eq!(
        event_types,
        [
            "put_entry",
            "put_entry",
            "update_entry",
            "delete_entry",
            "restore_entry"
        ]
    );
}

#[test]
fn branch_keeps_snapshot_state_and_tags_filter_entries() {
    let mut server = Server::start("branch_keeps_snapshot_state_and_tags_filter_entries");
    let volume_id = server.call("create_volume", json!({ "name": "main" }))["id"].clone();
    for (content, tags) in [
        ("Deploy on Fridays is banned", json!(["ops", "policy"])),
        ("Logs go to Loki", json!(["ops"])),
        ("Use British spelling", json!(["style"])),
    ] {
        server.call(
            "remember",
            json!({ "volume_id": volume_id, "content": content, "source": "User", "tags": tags }),
        );
    }
    let snapshot = server.call(
        "create_snapshot",
        json!({ "volume_id": volume_id, "label": "v1" }),
    );
    server.call(
        "remember",
        json!({ "volume_id": volume_id, "content": "Alerts page the on-call", "source": "Agent", "tags": ["ops"] }),
    );

    let branch = server.call(
        "branch_from_snapshot",
        json!({ "snapshot_id": snapshot["id"], "name": "experiment" }),
    );
    assert_eq!(branch["name"], "experiment");
    assert_eq!(branch["parent_snapshot_id"], snapshot["id"]);

    let branch_ops = server.call(
        "list_memory",
        json!({ "volume_id": branch["id"], "tags": ["ops"] }),
    );
    assert_eq!(
        contents(&branch_ops["entries"]),
        ["Deploy on Fridays is banned", "Logs go to Loki"]
    );

    let main_ops = server.call(
        "list_memory",
        json!({ "volume_id": volume_id, "tags": ["ops"] }),
    );
    assert_eq!(main_ops["entries"].as_array().unwrap().len(), 3);

    let main_ops_policy = server.call(
        "list_memory",
        json!({ "volume_id": volume_id, "tags": ["ops", "policy"] }),
    );
    assert_eq!(
        contents(&main_ops_policy["entries"]),
        ["Deploy on Fridays is banned"]
    );
}

#[test]
fn optional_fields_can_be_omitted() {
    let mut server = Server::start("optional_fields_can_be_omitted");
    let volume_id = server.call("create_volume", json!({ "name": "notes" }))["id"].clone();

    let entry = server.call(
        "remember",
        json!({ "volume_id": volume_id, "content": "No tags here", "source": "Tool" }),
    );

    assert_eq!(entry["tags"], json!([]));
    let history = server.call(
        "memory_history",
        json!({ "volume_id": volume_id, "entry_id": entry["id"] }),
    );
    assert_eq!(history["events"].as_array().unwrap().len(), 1);
}

#[test]
fn tool_errors_are_readable() {
    let mut server = Server::start("tool_errors_are_readable");
    let volume_id = server.call("create_volume", json!({ "name": "errors" }))["id"].clone();

    assert_eq!(
        server.call_error("forget", json!({ "entry_id": 999 })),
        "entry 999 not found"
    );
    assert_eq!(
        server.call_error("list_memory", json!({ "volume_id": 999 })),
        "volume 999 not found"
    );
    assert_eq!(
        server.call_error(
            "remember",
            json!({ "volume_id": volume_id, "content": "", "source": "User" })
        ),
        "entry content must not be empty"
    );
}

#[test]
fn branch_merge_back_and_report_conflicts() {
    let mut server = Server::start("branch_merge_back_and_report_conflicts");
    let main_id = server.call("create_volume", json!({ "name": "main" }))["id"].clone();
    let fact = server.call(
        "remember",
        json!({ "volume_id": main_id, "content": "Ship on Tuesday", "source": "User" }),
    );
    let snapshot = server.call(
        "create_snapshot",
        json!({ "volume_id": main_id, "label": "fork" }),
    );
    let branch = server.call(
        "branch_from_snapshot",
        json!({ "snapshot_id": snapshot["id"], "name": "plan-b" }),
    );
    let branch_fact =
        server.call("list_memory", json!({ "volume_id": branch["id"] }))["entries"][0].clone();

    // Both sides edit the same entry, and the branch adds one of its own.
    server.call(
        "update_memory",
        json!({ "entry_id": fact["id"], "content": "Ship on Wednesday" }),
    );
    server.call(
        "update_memory",
        json!({ "entry_id": branch_fact["id"], "content": "Ship on Thursday" }),
    );
    server.call(
        "remember",
        json!({ "volume_id": branch["id"], "content": "Feature flag rollout", "source": "Agent" }),
    );

    let diff = server.call(
        "diff_volumes",
        json!({ "from_volume_id": main_id, "to_volume_id": branch["id"] }),
    );
    assert_eq!(contents(&diff["added"]), ["Feature flag rollout"]);
    assert_eq!(diff["modified"][0]["after"]["content"], "Ship on Thursday");

    let preview = server.call(
        "merge_volumes",
        json!({ "source_volume_id": branch["id"], "target_volume_id": main_id, "dry_run": true }),
    );
    assert_eq!(preview["dry_run"], true);

    let merged = server.call(
        "merge_volumes",
        json!({ "source_volume_id": branch["id"], "target_volume_id": main_id }),
    );
    assert_eq!(contents(&merged["added"]), ["Feature flag rollout"]);
    assert_eq!(
        merged["conflicts"][0]["source"]["content"],
        "Ship on Thursday"
    );
    assert_eq!(
        merged["conflicts"][0]["target"]["content"],
        "Ship on Wednesday"
    );

    let listed = server.call("list_memory", json!({ "volume_id": main_id }));
    assert_eq!(
        contents(&listed["entries"]),
        ["Ship on Wednesday", "Feature flag rollout"]
    );
}
