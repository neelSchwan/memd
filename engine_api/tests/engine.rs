use core_types::{EntrySource, EventType};
use engine_api::Connection;
use engine_api::*;

fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    init_db(&conn).unwrap();
    conn
}

fn contents(entries: &[core_types::Entry]) -> Vec<&str> {
    entries.iter().map(|e| e.content()).collect()
}

#[test]
fn add_update_delete_are_logged_and_projected() {
    let conn = db();
    let vol = create_volume(&conn, "work".into()).unwrap();
    let a = add_entry(&conn, vol.id(), "uses sqlite".into(), EntrySource::Agent).unwrap();
    let b = add_entry(&conn, vol.id(), "likes tabs".into(), EntrySource::User).unwrap();

    update_entry(&conn, a.id(), "uses postgres".into()).unwrap();
    delete_entry(&conn, b.id()).unwrap();

    assert_eq!(
        contents(&list_entries(&conn, vol.id()).unwrap()),
        ["uses postgres"]
    );
    assert!(matches!(
        get_entry(&conn, b.id()),
        Err(EngineError::EntryDeleted(_))
    ));

    let log: Vec<_> = list_events(&conn, vol.id())
        .unwrap()
        .iter()
        .map(|e| (e.seq(), e.event_type()))
        .collect();
    assert_eq!(
        log,
        [
            (1, EventType::PutEntry),
            (2, EventType::PutEntry),
            (3, EventType::UpdateEntry),
            (4, EventType::DeleteEntry),
        ]
    );

    let history = entry_history(&conn, vol.id(), a.id()).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].content(), Some("uses postgres"));
}

#[test]
fn identical_update_records_nothing() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let e = add_entry(&conn, vol.id(), "same".into(), EntrySource::User).unwrap();
    update_entry(&conn, e.id(), "same".into()).unwrap();
    assert_eq!(list_events(&conn, vol.id()).unwrap().len(), 1);
}

#[test]
fn mutating_deleted_or_missing_entries_fails() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let e = add_entry(&conn, vol.id(), "x".into(), EntrySource::User).unwrap();
    delete_entry(&conn, e.id()).unwrap();

    assert!(matches!(
        update_entry(&conn, e.id(), "y".into()),
        Err(EngineError::EntryDeleted(_))
    ));
    assert!(matches!(
        delete_entry(&conn, e.id()),
        Err(EngineError::EntryDeleted(_))
    ));
    assert!(matches!(
        delete_entry(&conn, 999),
        Err(EngineError::EntryNotFound(999))
    ));
    assert!(matches!(
        add_entry(&conn, 999, "x".into(), EntrySource::User),
        Err(EngineError::VolumeNotFound(999))
    ));
    assert!(matches!(
        create_volume(&conn, "v".into()),
        Err(EngineError::VolumeNameTaken(_))
    ));
}

#[test]
fn search_matches_words_and_ranks_best_first() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    add_entry(
        &conn,
        vol.id(),
        "Use Postgres in production".into(),
        EntrySource::User,
    )
    .unwrap();
    add_entry(
        &conn,
        vol.id(),
        "Postgres pool size is 20; postgres runs on port 5432".into(),
        EntrySource::User,
    )
    .unwrap();
    let auth = add_entry(
        &conn,
        vol.id(),
        "Authentication bug is in middleware.rs".into(),
        EntrySource::User,
    )
    .unwrap();

    // Case-insensitive, and the entry that mentions the word most ranks first.
    let found = search(&conn, vol.id(), "POSTGRES".into()).unwrap();
    assert_eq!(found.len(), 2);
    assert!(found[0].content().starts_with("Postgres pool"));

    // Words match by prefix, in any order.
    assert_eq!(
        contents(&search(&conn, vol.id(), "middleware auth".into()).unwrap()),
        [auth.content()]
    );

    // FTS5 syntax in the query is treated as plain text, not as operators.
    assert_eq!(
        search(&conn, vol.id(), "auth* AND \"NEAR(".into())
            .unwrap()
            .len(),
        1
    );
    assert!(search(&conn, vol.id(), "%%".into()).unwrap().is_empty());
}

#[test]
fn search_follows_updates_deletes_and_volumes() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let other = create_volume(&conn, "other".into()).unwrap();
    let entry = add_entry(&conn, vol.id(), "uses sqlite".into(), EntrySource::User).unwrap();
    add_entry(
        &conn,
        other.id(),
        "uses sqlite too".into(),
        EntrySource::User,
    )
    .unwrap();

    assert_eq!(search(&conn, vol.id(), "sqlite".into()).unwrap().len(), 1);

    update_entry(&conn, entry.id(), "uses postgres".into()).unwrap();
    assert!(search(&conn, vol.id(), "sqlite".into()).unwrap().is_empty());
    assert_eq!(search(&conn, vol.id(), "postgres".into()).unwrap().len(), 1);

    delete_entry(&conn, entry.id()).unwrap();
    assert!(
        search(&conn, vol.id(), "postgres".into())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn snapshot_of_empty_volume_is_valid() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let snap = snapshot(&conn, vol.id(), "empty".into()).unwrap();
    assert_eq!(snap.event_seq_pointer(), 0);
    assert_eq!(list_snapshots(&conn, vol.id()).unwrap().len(), 1);
    assert!(entries_at_snapshot(&conn, snap.id()).unwrap().is_empty());
}

#[test]
fn state_at_replays_history() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let e = add_entry(&conn, vol.id(), "v1".into(), EntrySource::User).unwrap();
    update_entry(&conn, e.id(), "v2".into()).unwrap();
    delete_entry(&conn, e.id()).unwrap();

    assert!(state_at(&conn, vol.id(), 0).unwrap().is_empty());
    assert_eq!(contents(&state_at(&conn, vol.id(), 1).unwrap()), ["v1"]);
    assert_eq!(contents(&state_at(&conn, vol.id(), 2).unwrap()), ["v2"]);
    assert!(state_at(&conn, vol.id(), 3).unwrap().is_empty());
}

#[test]
fn diff_reports_added_removed_modified() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let keep = add_entry(&conn, vol.id(), "keep".into(), EntrySource::User).unwrap();
    let change = add_entry(&conn, vol.id(), "old".into(), EntrySource::User).unwrap();
    let drop = add_entry(&conn, vol.id(), "drop".into(), EntrySource::User).unwrap();
    let s1 = snapshot(&conn, vol.id(), "s1".into()).unwrap();

    update_entry(&conn, change.id(), "new".into()).unwrap();
    delete_entry(&conn, drop.id()).unwrap();
    add_entry(&conn, vol.id(), "added".into(), EntrySource::User).unwrap();
    let s2 = snapshot(&conn, vol.id(), "s2".into()).unwrap();

    let d = diff_snapshots(&conn, s1.id(), Some(s2.id())).unwrap();
    assert_eq!(contents(&d.added), ["added"]);
    assert_eq!(contents(&d.removed), ["drop"]);
    assert_eq!(d.modified.len(), 1);
    assert_eq!(d.modified[0].before.content(), "old");
    assert_eq!(d.modified[0].after.content(), "new");
    assert!(!d.added.iter().any(|e| e.id() == keep.id()));

    // Against the current state, which equals s2.
    assert!(diff_snapshots(&conn, s2.id(), None).unwrap().is_empty());

    let other = create_volume(&conn, "other".into()).unwrap();
    let s3 = snapshot(&conn, other.id(), "s3".into()).unwrap();
    assert!(matches!(
        diff_snapshots(&conn, s1.id(), Some(s3.id())),
        Err(EngineError::SnapshotVolumeMismatch { .. })
    ));
}

#[test]
fn rollback_restores_state_without_rewriting_history() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let a = add_entry(&conn, vol.id(), "a1".into(), EntrySource::User).unwrap();
    let b = add_entry(&conn, vol.id(), "b".into(), EntrySource::User).unwrap();
    let before = snapshot(&conn, vol.id(), "before".into()).unwrap();

    update_entry(&conn, a.id(), "a2".into()).unwrap();
    delete_entry(&conn, b.id()).unwrap();
    add_entry(&conn, vol.id(), "c".into(), EntrySource::User).unwrap();
    let after = snapshot(&conn, vol.id(), "after".into()).unwrap();

    let report = rollback_to_snapshot(&conn, before.id()).unwrap();
    assert_eq!(report.from_seq, 5);
    assert_eq!(report.events_appended, 3); // update a, restore b, delete c
    assert_eq!(report.to_seq, 8);
    assert_eq!(
        contents(&list_entries(&conn, vol.id()).unwrap()),
        ["a1", "b"]
    );
    assert!(diff_snapshots(&conn, before.id(), None).unwrap().is_empty());

    // The pre-rollback state is still in history and the rollback can be undone.
    assert_eq!(list_events(&conn, vol.id()).unwrap().len(), 8);
    rollback_to_snapshot(&conn, after.id()).unwrap();
    assert_eq!(
        contents(&list_entries(&conn, vol.id()).unwrap()),
        ["a2", "c"]
    );

    // Rolling back to the state you are already in is a no-op.
    let noop = rollback_to_snapshot(&conn, after.id()).unwrap();
    assert_eq!(noop.events_appended, 0);

    assert_eq!(rebuild_projection(&conn, vol.id()).unwrap(), 0);
}

#[test]
fn rollback_to_empty_snapshot_clears_volume() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let empty = snapshot(&conn, vol.id(), "empty".into()).unwrap();
    add_entry(&conn, vol.id(), "x".into(), EntrySource::User).unwrap();
    rollback_to_snapshot(&conn, empty.id()).unwrap();
    assert!(list_entries(&conn, vol.id()).unwrap().is_empty());
}

#[test]
fn clone_copies_history_and_diverges_independently() {
    let conn = db();
    let vol = create_volume(&conn, "plan".into()).unwrap();
    let a = add_entry(&conn, vol.id(), "a1".into(), EntrySource::Agent).unwrap();
    let b = add_entry(&conn, vol.id(), "b".into(), EntrySource::User).unwrap();
    update_entry(&conn, a.id(), "a2".into()).unwrap();
    delete_entry(&conn, b.id()).unwrap();
    let snap = snapshot(&conn, vol.id(), "s".into()).unwrap();
    add_entry(&conn, vol.id(), "after snapshot".into(), EntrySource::User).unwrap();

    let branch = clone_volume(&conn, snap.id(), None).unwrap();
    assert_eq!(branch.name(), "plan_clone");
    assert_eq!(branch.parent_snapshot_id(), Some(snap.id()));

    let entries = list_entries(&conn, branch.id()).unwrap();
    assert_eq!(contents(&entries), ["a2"]);
    assert_eq!(entries[0].source(), EntrySource::Agent);
    assert_ne!(entries[0].id(), a.id());
    assert_eq!(list_events(&conn, branch.id()).unwrap().len(), 4);

    // Changes on the branch leave the source alone and vice versa.
    update_entry(&conn, entries[0].id(), "branch only".into()).unwrap();
    assert_eq!(
        contents(&list_entries(&conn, vol.id()).unwrap()),
        ["a2", "after snapshot"]
    );

    // History of the branch replays back to before the snapshot.
    assert_eq!(contents(&state_at(&conn, branch.id(), 1).unwrap()), ["a1"]);

    // Default names never collide; explicit names must be free.
    let second = clone_volume(&conn, snap.id(), None).unwrap();
    assert_eq!(second.name(), "plan_clone_2");
    let named = clone_volume(&conn, snap.id(), Some("what-if".into())).unwrap();
    assert_eq!(named.name(), "what-if");
    assert!(matches!(
        clone_volume(&conn, snap.id(), Some("what-if".into())),
        Err(EngineError::VolumeNameTaken(_))
    ));
}

#[test]
fn failed_clone_leaves_no_partial_volume() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    assert!(clone_volume(&conn, 42, None).is_err());
    assert_eq!(list_volumes(&conn).unwrap().len(), 1);
    assert_eq!(list_volumes(&conn).unwrap()[0].id(), vol.id());
}

#[test]
fn rebuild_projection_repairs_tampered_entries() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let a = add_entry(&conn, vol.id(), "truth".into(), EntrySource::User).unwrap();
    let b = add_entry(&conn, vol.id(), "gone".into(), EntrySource::User).unwrap();
    delete_entry(&conn, b.id()).unwrap();

    conn.execute(
        "UPDATE entries SET content = 'tampered' WHERE id = ?1",
        [a.id()],
    )
    .unwrap();
    conn.execute("UPDATE entries SET deleted = 0 WHERE id = ?1", [b.id()])
        .unwrap();

    assert_eq!(rebuild_projection(&conn, vol.id()).unwrap(), 2);
    assert_eq!(contents(&list_entries(&conn, vol.id()).unwrap()), ["truth"]);
    assert_eq!(rebuild_projection(&conn, vol.id()).unwrap(), 0);
}

#[test]
fn migrates_v0_database() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "
        CREATE TABLE volumes (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL);
        CREATE TABLE entries (
            id INTEGER PRIMARY KEY, volume_id INTEGER NOT NULL REFERENCES volumes(id),
            content TEXT NOT NULL, created_at TEXT NOT NULL,
            source TEXT NOT NULL CHECK (source IN ('user', 'agent', 'tool')));
        CREATE TABLE events (
            seq INTEGER NOT NULL, volume_id INTEGER NOT NULL REFERENCES volumes(id),
            event_type TEXT NOT NULL CHECK (event_type IN ('put_entry')),
            entry_id INTEGER NOT NULL REFERENCES entries(id), created_at TEXT NOT NULL,
            PRIMARY KEY (volume_id, seq));
        CREATE TABLE snapshots (
            id INTEGER PRIMARY KEY, volume_id INTEGER NOT NULL REFERENCES volumes(id),
            event_seq_pointer INTEGER NOT NULL, created_at TEXT NOT NULL, label TEXT NOT NULL);

        INSERT INTO volumes VALUES (1, 'legacy', '2026-01-01T00:00:00+00:00');
        INSERT INTO entries VALUES (1, 1, 'old fact', '2026-01-01T00:00:01+00:00', 'agent');
        INSERT INTO events VALUES (1, 1, 'put_entry', 1, '2026-01-01T00:00:01+00:00');
        INSERT INTO snapshots VALUES (1, 1, 1, '2026-01-01T00:00:02+00:00', 's');
        ",
    )
    .unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

    init_db(&conn).unwrap();
    init_db(&conn).unwrap(); // idempotent

    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    let entries = list_entries(&conn, 1).unwrap();
    assert_eq!(contents(&entries), ["old fact"]);
    assert_eq!(entries[0].updated_at(), entries[0].created_at());
    assert_eq!(
        list_events(&conn, 1).unwrap()[0].content(),
        Some("old fact")
    );
    assert_eq!(rebuild_projection(&conn, 1).unwrap(), 0);

    // New features work on migrated data.
    assert_eq!(search(&conn, 1, "fact".into()).unwrap().len(), 1);
    update_entry(&conn, 1, "new fact".into()).unwrap();
    rollback_to_snapshot(&conn, 1).unwrap();
    assert_eq!(contents(&list_entries(&conn, 1).unwrap()), ["old fact"]);
}

#[test]
fn refuses_database_from_newer_version() {
    let conn = db();
    conn.execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION + 1))
        .unwrap();

    assert!(matches!(
        init_db(&conn),
        Err(EngineError::UnsupportedSchemaVersion(_))
    ));
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION + 1);
}

#[test]
fn concurrent_writers_on_one_file_all_succeed() {
    let path = std::env::temp_dir().join(format!("memd-concurrency-{}.db", std::process::id()));
    let volume_id = create_volume(&open_db(&path).unwrap(), "shared".into())
        .unwrap()
        .id();

    let writers: Vec<_> = (0..4)
        .map(|writer| {
            let path = path.clone();
            std::thread::spawn(move || {
                let conn = open_db(&path).unwrap();
                for i in 0..25 {
                    add_entry(
                        &conn,
                        volume_id,
                        format!("{writer}-{i}"),
                        EntrySource::Agent,
                    )
                    .unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }

    let seqs: Vec<_> = list_events(&open_db(&path).unwrap(), volume_id)
        .unwrap()
        .iter()
        .map(|e| e.seq())
        .collect();
    assert_eq!(seqs, (1..=100).collect::<Vec<_>>());

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
