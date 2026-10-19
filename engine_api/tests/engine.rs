use core_types::{EntrySource, EventType, VolumeId};
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
    let a = add_entry(
        &conn,
        vol.id(),
        "uses sqlite".into(),
        EntrySource::Agent,
        vec![],
    )
    .unwrap();
    let b = add_entry(
        &conn,
        vol.id(),
        "likes tabs".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();

    update_entry(&conn, a.id(), "uses postgres".into()).unwrap();
    delete_entry(&conn, b.id()).unwrap();

    assert_eq!(
        contents(&list_entries(&conn, vol.id(), &[]).unwrap()),
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
    let e = add_entry(&conn, vol.id(), "same".into(), EntrySource::User, vec![]).unwrap();
    update_entry(&conn, e.id(), "same".into()).unwrap();
    assert_eq!(list_events(&conn, vol.id()).unwrap().len(), 1);
}

#[test]
fn mutating_deleted_or_missing_entries_fails() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let e = add_entry(&conn, vol.id(), "x".into(), EntrySource::User, vec![]).unwrap();
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
        add_entry(&conn, 999, "x".into(), EntrySource::User, vec![]),
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
        vec![],
    )
    .unwrap();
    add_entry(
        &conn,
        vol.id(),
        "Postgres pool size is 20; postgres runs on port 5432".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();
    let auth = add_entry(
        &conn,
        vol.id(),
        "Authentication bug is in middleware.rs".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();

    // Case-insensitive, and the entry that mentions the word most ranks first.
    let found = search(&conn, vol.id(), "POSTGRES".into(), &[]).unwrap();
    assert_eq!(found.len(), 2);
    assert!(found[0].content().starts_with("Postgres pool"));

    // Words match by prefix, in any order.
    assert_eq!(
        contents(&search(&conn, vol.id(), "middleware auth".into(), &[]).unwrap()),
        [auth.content()]
    );

    // FTS5 syntax in the query is treated as plain text, not as operators.
    assert_eq!(
        search(&conn, vol.id(), "auth* AND \"NEAR(".into(), &[])
            .unwrap()
            .len(),
        1
    );
    assert!(
        search(&conn, vol.id(), "%%".into(), &[])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn search_follows_updates_deletes_and_volumes() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let other = create_volume(&conn, "other".into()).unwrap();
    let entry = add_entry(
        &conn,
        vol.id(),
        "uses sqlite".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();
    add_entry(
        &conn,
        other.id(),
        "uses sqlite too".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();

    assert_eq!(
        search(&conn, vol.id(), "sqlite".into(), &[]).unwrap().len(),
        1
    );

    update_entry(&conn, entry.id(), "uses postgres".into()).unwrap();
    assert!(
        search(&conn, vol.id(), "sqlite".into(), &[])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        search(&conn, vol.id(), "postgres".into(), &[])
            .unwrap()
            .len(),
        1
    );

    delete_entry(&conn, entry.id()).unwrap();
    assert!(
        search(&conn, vol.id(), "postgres".into(), &[])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn search_follows_clones_and_rollbacks() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let entry = add_entry(
        &conn,
        vol.id(),
        "uses sqlite".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();
    let snap = snapshot(&conn, vol.id(), "s".into()).unwrap();

    let branch = clone_volume(&conn, snap.id(), None).unwrap();
    assert_eq!(
        search(&conn, branch.id(), "sqlite".into(), &[])
            .unwrap()
            .len(),
        1
    );

    update_entry(&conn, entry.id(), "uses postgres".into()).unwrap();
    rollback_to_snapshot(&conn, snap.id()).unwrap();
    assert_eq!(
        search(&conn, vol.id(), "sqlite".into(), &[]).unwrap().len(),
        1
    );
    assert!(
        search(&conn, vol.id(), "postgres".into(), &[])
            .unwrap()
            .is_empty()
    );
}

fn tags(list: &[&str]) -> Vec<String> {
    list.iter().map(|tag| tag.to_string()).collect()
}

#[test]
fn tags_are_normalized_and_validated() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let entry = add_entry(
        &conn,
        vol.id(),
        "x".into(),
        EntrySource::User,
        tags(&[" DB ", "auth", "db"]),
    )
    .unwrap();
    assert_eq!(entry.tags(), ["auth", "db"]);
    assert_eq!(get_entry(&conn, entry.id()).unwrap().tags(), ["auth", "db"]);

    for bad in ["", "two words", "a,b"] {
        assert!(matches!(
            add_entry(&conn, vol.id(), "x".into(), EntrySource::User, tags(&[bad])),
            Err(EngineError::Domain(core_types::DomainError::InvalidTag(_)))
        ));
    }
}

#[test]
fn list_and_search_filter_by_all_tags() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    add_entry(
        &conn,
        vol.id(),
        "postgres in prod".into(),
        EntrySource::User,
        tags(&["db", "prod"]),
    )
    .unwrap();
    add_entry(
        &conn,
        vol.id(),
        "sqlite in tests".into(),
        EntrySource::User,
        tags(&["db"]),
    )
    .unwrap();
    add_entry(
        &conn,
        vol.id(),
        "deploy on fridays".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();

    assert_eq!(list_entries(&conn, vol.id(), &[]).unwrap().len(), 3);
    assert_eq!(
        list_entries(&conn, vol.id(), &tags(&["DB"])).unwrap().len(),
        2
    );
    assert_eq!(
        contents(&list_entries(&conn, vol.id(), &tags(&["db", "prod"])).unwrap()),
        ["postgres in prod"]
    );
    assert_eq!(
        contents(&search(&conn, vol.id(), "in".into(), &tags(&["prod"])).unwrap()),
        ["postgres in prod"]
    );
}

#[test]
fn tag_entry_is_versioned_like_content() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let entry = add_entry(
        &conn,
        vol.id(),
        "uses sqlite".into(),
        EntrySource::User,
        tags(&["db"]),
    )
    .unwrap();
    let before = snapshot(&conn, vol.id(), "before".into()).unwrap();

    let tagged = tag_entry(&conn, entry.id(), tags(&["db", "decision"])).unwrap();
    assert_eq!(tagged.content(), "uses sqlite");
    assert_eq!(tagged.tags(), ["db", "decision"]);

    // Updating content keeps tags; setting the same tags again records nothing.
    update_entry(&conn, entry.id(), "uses postgres".into()).unwrap();
    tag_entry(&conn, entry.id(), tags(&["decision", "db"])).unwrap();
    let history = entry_history(&conn, vol.id(), entry.id()).unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(history[2].tags(), ["db", "decision"]);

    let diff = diff_snapshots(&conn, before.id(), None).unwrap();
    assert_eq!(diff.modified.len(), 1);
    assert_eq!(diff.modified[0].before.tags(), ["db"]);

    let branch = clone_volume(
        &conn,
        snapshot(&conn, vol.id(), "now".into()).unwrap().id(),
        None,
    )
    .unwrap();
    assert_eq!(
        list_entries(&conn, branch.id(), &[]).unwrap()[0].tags(),
        ["db", "decision"]
    );

    rollback_to_snapshot(&conn, before.id()).unwrap();
    let restored = get_entry(&conn, entry.id()).unwrap();
    assert_eq!(
        (restored.content(), restored.tags()),
        ("uses sqlite", &tags(&["db"])[..])
    );

    tag_entry(&conn, entry.id(), vec![]).unwrap();
    assert!(get_entry(&conn, entry.id()).unwrap().tags().is_empty());
}

#[test]
fn restoring_a_deleted_entry_brings_back_its_tags() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let entry = add_entry(
        &conn,
        vol.id(),
        "x".into(),
        EntrySource::User,
        tags(&["db"]),
    )
    .unwrap();
    let before = snapshot(&conn, vol.id(), "before".into()).unwrap();
    delete_entry(&conn, entry.id()).unwrap();

    rollback_to_snapshot(&conn, before.id()).unwrap();
    assert_eq!(get_entry(&conn, entry.id()).unwrap().tags(), ["db"]);
}

#[test]
fn rebuild_projection_repairs_tags() {
    let conn = db();
    let vol = create_volume(&conn, "v".into()).unwrap();
    let entry = add_entry(
        &conn,
        vol.id(),
        "x".into(),
        EntrySource::User,
        tags(&["keep"]),
    )
    .unwrap();
    conn.execute(
        "UPDATE entries SET tags = 'tampered' WHERE id = ?1",
        [entry.id()],
    )
    .unwrap();
    assert_eq!(rebuild_projection(&conn, vol.id()).unwrap(), 1);
    assert_eq!(get_entry(&conn, entry.id()).unwrap().tags(), ["keep"]);
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
    let e = add_entry(&conn, vol.id(), "v1".into(), EntrySource::User, vec![]).unwrap();
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
    let keep = add_entry(&conn, vol.id(), "keep".into(), EntrySource::User, vec![]).unwrap();
    let change = add_entry(&conn, vol.id(), "old".into(), EntrySource::User, vec![]).unwrap();
    let drop = add_entry(&conn, vol.id(), "drop".into(), EntrySource::User, vec![]).unwrap();
    let s1 = snapshot(&conn, vol.id(), "s1".into()).unwrap();

    update_entry(&conn, change.id(), "new".into()).unwrap();
    delete_entry(&conn, drop.id()).unwrap();
    add_entry(&conn, vol.id(), "added".into(), EntrySource::User, vec![]).unwrap();
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
    let a = add_entry(&conn, vol.id(), "a1".into(), EntrySource::User, vec![]).unwrap();
    let b = add_entry(&conn, vol.id(), "b".into(), EntrySource::User, vec![]).unwrap();
    let before = snapshot(&conn, vol.id(), "before".into()).unwrap();

    update_entry(&conn, a.id(), "a2".into()).unwrap();
    delete_entry(&conn, b.id()).unwrap();
    add_entry(&conn, vol.id(), "c".into(), EntrySource::User, vec![]).unwrap();
    let after = snapshot(&conn, vol.id(), "after".into()).unwrap();

    let report = rollback_to_snapshot(&conn, before.id()).unwrap();
    assert_eq!(report.from_seq, 5);
    assert_eq!(report.events_appended, 3); // update a, restore b, delete c
    assert_eq!(report.to_seq, 8);
    assert_eq!(
        contents(&list_entries(&conn, vol.id(), &[]).unwrap()),
        ["a1", "b"]
    );
    assert!(diff_snapshots(&conn, before.id(), None).unwrap().is_empty());

    // The pre-rollback state is still in history and the rollback can be undone.
    assert_eq!(list_events(&conn, vol.id()).unwrap().len(), 8);
    rollback_to_snapshot(&conn, after.id()).unwrap();
    assert_eq!(
        contents(&list_entries(&conn, vol.id(), &[]).unwrap()),
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
    add_entry(&conn, vol.id(), "x".into(), EntrySource::User, vec![]).unwrap();
    rollback_to_snapshot(&conn, empty.id()).unwrap();
    assert!(list_entries(&conn, vol.id(), &[]).unwrap().is_empty());
}

#[test]
fn clone_copies_history_and_diverges_independently() {
    let conn = db();
    let vol = create_volume(&conn, "plan".into()).unwrap();
    let a = add_entry(&conn, vol.id(), "a1".into(), EntrySource::Agent, vec![]).unwrap();
    let b = add_entry(&conn, vol.id(), "b".into(), EntrySource::User, vec![]).unwrap();
    update_entry(&conn, a.id(), "a2".into()).unwrap();
    delete_entry(&conn, b.id()).unwrap();
    let snap = snapshot(&conn, vol.id(), "s".into()).unwrap();
    add_entry(
        &conn,
        vol.id(),
        "after snapshot".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();

    let branch = clone_volume(&conn, snap.id(), None).unwrap();
    assert_eq!(branch.name(), "plan_clone");
    assert_eq!(branch.parent_snapshot_id(), Some(snap.id()));

    let entries = list_entries(&conn, branch.id(), &[]).unwrap();
    assert_eq!(contents(&entries), ["a2"]);
    assert_eq!(entries[0].source(), EntrySource::Agent);
    assert_ne!(entries[0].id(), a.id());
    assert_eq!(list_events(&conn, branch.id()).unwrap().len(), 4);

    // Changes on the branch leave the source alone and vice versa.
    update_entry(&conn, entries[0].id(), "branch only".into()).unwrap();
    assert_eq!(
        contents(&list_entries(&conn, vol.id(), &[]).unwrap()),
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
    let a = add_entry(&conn, vol.id(), "truth".into(), EntrySource::User, vec![]).unwrap();
    let b = add_entry(&conn, vol.id(), "gone".into(), EntrySource::User, vec![]).unwrap();
    delete_entry(&conn, b.id()).unwrap();

    conn.execute(
        "UPDATE entries SET content = 'tampered' WHERE id = ?1",
        [a.id()],
    )
    .unwrap();
    conn.execute("UPDATE entries SET deleted = 0 WHERE id = ?1", [b.id()])
        .unwrap();

    assert_eq!(rebuild_projection(&conn, vol.id()).unwrap(), 2);
    assert_eq!(
        contents(&list_entries(&conn, vol.id(), &[]).unwrap()),
        ["truth"]
    );
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

    let entries = list_entries(&conn, 1, &[]).unwrap();
    assert_eq!(contents(&entries), ["old fact"]);
    assert_eq!(entries[0].updated_at(), entries[0].created_at());
    assert_eq!(
        list_events(&conn, 1).unwrap()[0].content(),
        Some("old fact")
    );
    assert_eq!(rebuild_projection(&conn, 1).unwrap(), 0);

    // New features work on migrated data.
    assert_eq!(search(&conn, 1, "fact".into(), &[]).unwrap().len(), 1);
    update_entry(&conn, 1, "new fact".into()).unwrap();
    rollback_to_snapshot(&conn, 1).unwrap();
    assert_eq!(
        contents(&list_entries(&conn, 1, &[]).unwrap()),
        ["old fact"]
    );
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
                        vec![],
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

    remove_database_files(&path);
}

/// Path of a v1 database file (before the full-text index, tags and entry origins) holding one volume whose
/// only entry was updated from "uses sqlite" to "uses postgres". Built by creating a current
/// database and undoing every later migration.
fn v1_database_file(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("memd-{name}-{}.db", std::process::id()));
    remove_database_files(&path);
    let conn = open_db(&path).unwrap();
    let vol = create_volume(&conn, "legacy".into()).unwrap();
    let entry = add_entry(
        &conn,
        vol.id(),
        "uses sqlite".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();
    update_entry(&conn, entry.id(), "uses postgres".into()).unwrap();
    conn.execute_batch(
        "DROP TRIGGER entries_fts_insert;
         DROP TRIGGER entries_fts_update;
         DROP TABLE entries_fts;
         ALTER TABLE entries DROP COLUMN tags;
         ALTER TABLE events DROP COLUMN tags;
         ALTER TABLE entries DROP COLUMN origin_entry_id;
         ALTER TABLE snapshots DROP COLUMN merged_into_volume_id;
         PRAGMA user_version = 1;",
    )
    .unwrap();
    path
}

fn remove_database_files(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn migrates_v1_database_and_indexes_current_content() {
    let path = v1_database_file("v1-upgrade");
    let conn = open_db(&path).unwrap();

    assert_eq!(
        contents(&search(&conn, 1, "postgres".into(), &[]).unwrap()),
        ["uses postgres"]
    );
    assert!(search(&conn, 1, "sqlite".into(), &[]).unwrap().is_empty());
    assert!(list_entries(&conn, 1, &[]).unwrap()[0].tags().is_empty());
    tag_entry(&conn, 1, tags(&["db"])).unwrap();

    drop(conn);
    remove_database_files(&path);
}

#[test]
fn processes_starting_together_migrate_once() {
    let path = v1_database_file("v1-race");

    // Hold the write lock so every opener reads version 1 and then queues behind it.
    let blocker = Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let openers: Vec<_> = (0..3)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || open_db(&path).map(drop))
        })
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(200));
    blocker.execute_batch("COMMIT").unwrap();

    for opener in openers {
        opener.join().unwrap().unwrap();
    }
    assert_eq!(
        search(&open_db(&path).unwrap(), 1, "postgres".into(), &[])
            .unwrap()
            .len(),
        1
    );

    drop(blocker);
    remove_database_files(&path);
}

/// A "main" volume with one entry, snapshotted and branched. Returns (main, branch, snapshot).
fn main_and_branch(conn: &Connection) -> (VolumeId, VolumeId, core_types::Snapshot) {
    let main = create_volume(conn, "main".into()).unwrap();
    add_entry(
        conn,
        main.id(),
        "shared fact".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();
    let snap = snapshot(conn, main.id(), "fork".into()).unwrap();
    let branch = clone_volume(conn, snap.id(), Some("branch".into())).unwrap();
    (main.id(), branch.id(), snap)
}

fn live(conn: &Connection, volume_id: VolumeId) -> Vec<String> {
    list_entries(conn, volume_id, &[])
        .unwrap()
        .iter()
        .map(|e| e.content().to_string())
        .collect()
}

fn entry_id_with(conn: &Connection, volume_id: VolumeId, content: &str) -> core_types::EntryId {
    list_entries(conn, volume_id, &[])
        .unwrap()
        .into_iter()
        .find(|e| e.content() == content)
        .unwrap()
        .id()
}

#[test]
fn clones_match_their_source_entry_by_entry() {
    let conn = db();
    let (main, branch, _) = main_and_branch(&conn);
    assert!(diff_volumes(&conn, main, branch).unwrap().is_empty());

    let grandchild_snap = snapshot(&conn, branch, "again".into()).unwrap();
    let grandchild = clone_volume(&conn, grandchild_snap.id(), None).unwrap();
    assert!(
        diff_volumes(&conn, main, grandchild.id())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn diff_volumes_matches_entries_across_branches() {
    let conn = db();
    let (main, branch, _) = main_and_branch(&conn);
    add_entry(
        &conn,
        branch,
        "branch only".into(),
        EntrySource::Agent,
        vec![],
    )
    .unwrap();
    update_entry(
        &conn,
        entry_id_with(&conn, main, "shared fact"),
        "shared fact v2".into(),
    )
    .unwrap();

    let diff = diff_volumes(&conn, main, branch).unwrap();
    assert_eq!(contents(&diff.added), ["branch only"]);
    assert!(diff.removed.is_empty());
    assert_eq!(diff.modified.len(), 1);
    assert_eq!(diff.modified[0].before.content(), "shared fact v2");
    assert_eq!(diff.modified[0].after.content(), "shared fact");

    let reverse = diff_volumes(&conn, branch, main).unwrap();
    assert_eq!(contents(&reverse.removed), ["branch only"]);
}

#[test]
fn merge_applies_branch_changes_without_rewriting_history() {
    let conn = db();
    let main = create_volume(&conn, "main".into()).unwrap().id();
    add_entry(&conn, main, "shared fact".into(), EntrySource::User, vec![]).unwrap();
    let doomed = add_entry(&conn, main, "doomed".into(), EntrySource::User, vec![]).unwrap();
    let snap = snapshot(&conn, main, "fork".into()).unwrap();
    let branch = clone_volume(&conn, snap.id(), None).unwrap().id();

    add_entry(
        &conn,
        branch,
        "new on branch".into(),
        EntrySource::Agent,
        tags(&["idea"]),
    )
    .unwrap();
    update_entry(
        &conn,
        entry_id_with(&conn, branch, "shared fact"),
        "refined fact".into(),
    )
    .unwrap();
    delete_entry(&conn, entry_id_with(&conn, branch, "doomed")).unwrap();
    add_entry(&conn, main, "main only".into(), EntrySource::User, vec![]).unwrap();
    let main_log_before = list_events(&conn, main).unwrap().len();

    let report = merge_volumes(&conn, branch, main, false).unwrap();
    assert_eq!(contents(&report.added), ["new on branch"]);
    assert_eq!(report.removed[0].id(), doomed.id());
    assert_eq!(report.modified[0].after.content(), "refined fact");
    assert!(report.conflicts.is_empty());
    assert_eq!(report.events_appended, 3);

    // The target's own change survives, and its old history is untouched.
    assert_eq!(
        live(&conn, main),
        ["refined fact", "main only", "new on branch"]
    );
    assert_eq!(list_events(&conn, main).unwrap().len(), main_log_before + 3);
    assert_eq!(rebuild_projection(&conn, main).unwrap(), 0);
    let merged = list_entries(&conn, main, &tags(&["idea"])).unwrap();
    assert_eq!(merged[0].source(), EntrySource::Agent);
    assert_eq!(
        contents(&diff_volumes(&conn, branch, main).unwrap().added),
        ["main only"]
    );
}

#[test]
fn merge_dry_run_writes_nothing() {
    let conn = db();
    let (main, branch, _) = main_and_branch(&conn);
    add_entry(&conn, branch, "idea".into(), EntrySource::Agent, vec![]).unwrap();
    let events_before = list_events(&conn, main).unwrap().len();
    let snapshots_before = list_snapshots(&conn, branch).unwrap().len();

    let preview = merge_volumes(&conn, branch, main, true).unwrap();
    assert!(preview.dry_run);
    assert_eq!(contents(&preview.added), ["idea"]);
    assert_eq!(preview.events_appended, 1);
    assert_eq!(list_events(&conn, main).unwrap().len(), events_before);
    assert_eq!(
        list_snapshots(&conn, branch).unwrap().len(),
        snapshots_before
    );

    let real = merge_volumes(&conn, branch, main, false).unwrap();
    assert_eq!(contents(&real.added), contents(&preview.added));
}

#[test]
fn merge_skips_and_reports_conflicts() {
    let conn = db();
    let main = create_volume(&conn, "main".into()).unwrap().id();
    for content in ["both edit", "edit vs delete", "untouched"] {
        add_entry(&conn, main, content.into(), EntrySource::User, vec![]).unwrap();
    }
    let snap = snapshot(&conn, main, "fork".into()).unwrap();
    let branch = clone_volume(&conn, snap.id(), None).unwrap().id();

    update_entry(
        &conn,
        entry_id_with(&conn, main, "both edit"),
        "main's edit".into(),
    )
    .unwrap();
    update_entry(
        &conn,
        entry_id_with(&conn, branch, "both edit"),
        "branch's edit".into(),
    )
    .unwrap();
    delete_entry(&conn, entry_id_with(&conn, main, "edit vs delete")).unwrap();
    update_entry(
        &conn,
        entry_id_with(&conn, branch, "edit vs delete"),
        "kept on branch".into(),
    )
    .unwrap();
    add_entry(
        &conn,
        branch,
        "clean addition".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();

    let report = merge_volumes(&conn, branch, main, false).unwrap();
    assert_eq!(report.conflicts.len(), 2);
    let both = &report.conflicts[0];
    assert_eq!(both.base.as_ref().unwrap().content(), "both edit");
    assert_eq!(both.source.as_ref().unwrap().content(), "branch's edit");
    assert_eq!(both.target.as_ref().unwrap().content(), "main's edit");
    assert!(report.conflicts[1].target.is_none());

    // Conflicting entries are left as they were; the clean change still lands.
    assert_eq!(
        live(&conn, main),
        ["main's edit", "untouched", "clean addition"]
    );
}

#[test]
fn identical_changes_on_both_sides_are_not_conflicts() {
    let conn = db();
    let (main, branch, _) = main_and_branch(&conn);
    update_entry(
        &conn,
        entry_id_with(&conn, main, "shared fact"),
        "same edit".into(),
    )
    .unwrap();
    update_entry(
        &conn,
        entry_id_with(&conn, branch, "shared fact"),
        "same edit".into(),
    )
    .unwrap();

    let report = merge_volumes(&conn, branch, main, false).unwrap();
    assert!(report.conflicts.is_empty());
    assert_eq!(report.events_appended, 0);
}

#[test]
fn repeated_merges_from_a_long_lived_branch() {
    let conn = db();
    let (main, branch, _) = main_and_branch(&conn);
    let shared = entry_id_with(&conn, branch, "shared fact");

    update_entry(&conn, shared, "v2".into()).unwrap();
    merge_volumes(&conn, branch, main, false).unwrap();
    // Merging again with nothing new changes nothing.
    assert_eq!(
        merge_volumes(&conn, branch, main, false)
            .unwrap()
            .events_appended,
        0
    );

    // Editing the same entry again is a plain change, not a conflict.
    update_entry(&conn, shared, "v3".into()).unwrap();
    let report = merge_volumes(&conn, branch, main, false).unwrap();
    assert!(report.conflicts.is_empty());
    assert_eq!(live(&conn, main), ["v3"]);

    let note = add_entry(&conn, branch, "temporary".into(), EntrySource::User, vec![]).unwrap();
    merge_volumes(&conn, branch, main, false).unwrap();
    delete_entry(&conn, note.id()).unwrap();
    merge_volumes(&conn, branch, main, false).unwrap();
    assert_eq!(live(&conn, main), ["v3"]);
}

#[test]
fn merge_parent_into_branch_and_between_siblings() {
    let conn = db();
    let (main, branch, snap) = main_and_branch(&conn);
    let sibling = clone_volume(&conn, snap.id(), Some("sibling".into()))
        .unwrap()
        .id();

    add_entry(&conn, main, "from main".into(), EntrySource::User, vec![]).unwrap();
    merge_volumes(&conn, main, branch, false).unwrap();
    assert_eq!(live(&conn, branch), ["shared fact", "from main"]);

    add_entry(
        &conn,
        sibling,
        "from sibling".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();
    merge_volumes(&conn, sibling, branch, false).unwrap();
    assert_eq!(
        live(&conn, branch),
        ["shared fact", "from main", "from sibling"]
    );
}

#[test]
fn merge_restores_instead_of_duplicating() {
    let conn = db();
    let main = create_volume(&conn, "main".into()).unwrap().id();
    let entry = add_entry(&conn, main, "comes back".into(), EntrySource::User, vec![]).unwrap();
    let before_delete = snapshot(&conn, main, "before delete".into()).unwrap();
    delete_entry(&conn, entry.id()).unwrap();
    let after_delete = snapshot(&conn, main, "after delete".into()).unwrap();
    // The branch starts with the entry already deleted.
    let branch = clone_volume(&conn, after_delete.id(), None).unwrap().id();

    rollback_to_snapshot(&conn, before_delete.id()).unwrap();
    merge_volumes(&conn, main, branch, false).unwrap();

    assert_eq!(live(&conn, branch), ["comes back"]);
    let last_event = list_events(&conn, branch).unwrap().pop().unwrap();
    assert_eq!(last_event.event_type(), EventType::RestoreEntry);
}

#[test]
fn merging_unrelated_volumes_fails() {
    let conn = db();
    let a = create_volume(&conn, "a".into()).unwrap().id();
    let b = create_volume(&conn, "b".into()).unwrap().id();
    assert!(matches!(
        merge_volumes(&conn, a, b, false),
        Err(EngineError::VolumesNotRelated { .. })
    ));
}

#[test]
fn migration_links_existing_clones_to_their_origins() {
    let conn = db();
    let (main, branch, _) = main_and_branch(&conn);
    add_entry(
        &conn,
        branch,
        "branch only".into(),
        EntrySource::User,
        vec![],
    )
    .unwrap();
    let branch_snap = snapshot(&conn, branch, "again".into()).unwrap();
    let grandchild = clone_volume(&conn, branch_snap.id(), None).unwrap().id();

    // Turn the database back into v3, which had no entry origins.
    conn.execute_batch(
        "ALTER TABLE entries DROP COLUMN origin_entry_id;
         ALTER TABLE snapshots DROP COLUMN merged_into_volume_id;
         PRAGMA user_version = 3;",
    )
    .unwrap();
    init_db(&conn).unwrap();

    let diff = diff_volumes(&conn, main, branch).unwrap();
    assert!(diff.removed.is_empty() && diff.modified.is_empty());
    assert!(diff_volumes(&conn, branch, grandchild).unwrap().is_empty());
    assert_eq!(
        contents(&diff_volumes(&conn, main, grandchild).unwrap().added),
        ["branch only"]
    );
    let report = merge_volumes(&conn, grandchild, main, false).unwrap();
    assert_eq!(contents(&report.added), ["branch only"]);
    assert_eq!(live(&conn, main), ["shared fact", "branch only"]);
}

#[test]
fn changes_that_arrived_through_another_volume_are_not_reapplied() {
    let conn = db();
    let (main, left, snap) = main_and_branch(&conn);
    let right = clone_volume(&conn, snap.id(), Some("right".into()))
        .unwrap()
        .id();
    let shared = entry_id_with(&conn, left, "shared fact");

    // Left's changes reach right through main.
    add_entry(&conn, left, "from left".into(), EntrySource::User, vec![]).unwrap();
    update_entry(&conn, shared, "edited on left".into()).unwrap();
    merge_volumes(&conn, left, main, false).unwrap();
    merge_volumes(&conn, main, right, false).unwrap();

    // Right then deletes one and reverts the other.
    delete_entry(&conn, entry_id_with(&conn, right, "from left")).unwrap();
    let right_shared = entry_id_with(&conn, right, "edited on left");
    update_entry(&conn, right_shared, "shared fact".into()).unwrap();

    // Merging left into right directly must not undo right's later choices.
    let report = merge_volumes(&conn, left, right, false).unwrap();
    assert_eq!(report.events_appended, 0);
    assert_eq!(live(&conn, right), ["shared fact"]);
}

#[test]
fn migration_links_long_clone_chains_quickly() {
    let conn = db();
    let mut volume = create_volume(&conn, "root".into()).unwrap().id();
    for i in 0..1000 {
        add_entry(
            &conn,
            volume,
            format!("fact {i}"),
            EntrySource::User,
            vec![],
        )
        .unwrap();
    }
    for depth in 0..4 {
        let snap = snapshot(&conn, volume, format!("level {depth}")).unwrap();
        volume = clone_volume(&conn, snap.id(), None).unwrap().id();
    }
    conn.execute_batch(
        "ALTER TABLE entries DROP COLUMN origin_entry_id;
         ALTER TABLE snapshots DROP COLUMN merged_into_volume_id;
         PRAGMA user_version = 3;",
    )
    .unwrap();

    let started = std::time::Instant::now();
    init_db(&conn).unwrap();
    // The first version of this migration took minutes on a chain like this one.
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(diff_volumes(&conn, 1, volume).unwrap().is_empty());
}
