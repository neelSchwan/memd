//! Diffing and merging between a volume and its branches.
//!
//! Entries are matched across volumes by origin (see `entry_origins`), so an entry and its copy in
//! a branch are treated as the same entry. A volume never holds two entries with the same origin.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use core_types::{
    Entry, EntryId, EntryModification, EventType, MergeConflict, MergeReport, SeqNumber,
    SnapshotId, VolumeDiff, VolumeId,
};
use rusqlite::{Connection, OptionalExtension};

use crate::{
    EngineError, EntryData, Result, append_event, compare_states, entry_data, entry_origins,
    get_snapshot, get_volume, head_seq, insert_entry_row, join_tags, load_events, now,
    project_event, state_map_at, write_tx,
};

/// Live entries of a volume as of `seq`, keyed by origin.
fn live_by_origin(
    conn: &Connection,
    volume_id: VolumeId,
    seq: SeqNumber,
) -> Result<BTreeMap<EntryId, Entry>> {
    let origins = entry_origins(conn, volume_id)?;
    Ok(state_map_at(conn, volume_id, seq)?
        .into_iter()
        .map(|(id, entry)| (origins.get(&id).copied().unwrap_or(id), entry))
        .collect())
}

fn current_by_origin(conn: &Connection, volume_id: VolumeId) -> Result<BTreeMap<EntryId, Entry>> {
    live_by_origin(conn, volume_id, head_seq(conn, volume_id)?)
}

/// Compares the current live entries of two volumes, such as a branch and the volume it was
/// branched from. Works for any two volumes; unrelated volumes simply share no entries.
pub fn diff_volumes(conn: &Connection, from: VolumeId, to: VolumeId) -> Result<VolumeDiff> {
    get_volume(conn, from)?;
    get_volume(conn, to)?;
    let (added, removed, modified) =
        compare_states(current_by_origin(conn, from)?, current_by_origin(conn, to)?);
    Ok(VolumeDiff {
        from_volume_id: from,
        to_volume_id: to,
        added,
        removed,
        modified,
    })
}

/// A volume followed by the volumes it was branched from, nearest first, each paired with the
/// seq where the chain left it: `[(volume, head), (parent, fork seq), (grandparent, ...), ...]`.
fn lineage(conn: &Connection, volume_id: VolumeId) -> Result<Vec<(VolumeId, SeqNumber)>> {
    let mut chain = vec![(volume_id, head_seq(conn, volume_id)?)];
    let mut parent = get_volume(conn, volume_id)?.parent_snapshot_id();
    while let Some(snapshot_id) = parent {
        let snap = get_snapshot(conn, snapshot_id)?;
        chain.push((snap.volume_id(), snap.event_seq_pointer()));
        parent = get_volume(conn, snap.volume_id())?.parent_snapshot_id();
    }
    Ok(chain)
}

/// The state both volumes last agreed on.
struct MergeBase {
    volume_id: VolumeId,
    seq: SeqNumber,
    /// When that state was current. Target history after this moment is the target's own;
    /// anything earlier is history both sides share.
    since: DateTime<Utc>,
}

/// Finds the most recent merge between the two volumes, or else the point where their histories
/// split.
fn merge_base(conn: &Connection, source: VolumeId, target: VolumeId) -> Result<MergeBase> {
    // A merge leaves a snapshot on its source at the seq the target took in.
    let last_merge: Option<SnapshotId> = conn
        .query_row(
            "SELECT id FROM snapshots
                WHERE (volume_id = ?1 AND merged_into_volume_id = ?2)
                   OR (volume_id = ?2 AND merged_into_volume_id = ?1)
                ORDER BY id DESC LIMIT 1",
            (source, target),
            |row| row.get(0),
        )
        .optional()?;
    if let Some(snapshot_id) = last_merge {
        let snap = get_snapshot(conn, snapshot_id)?;
        return Ok(MergeBase {
            volume_id: snap.volume_id(),
            seq: snap.event_seq_pointer(),
            since: snap.created_at(),
        });
    }

    let target_lineage = lineage(conn, target)?;
    for (volume_id, source_seq) in lineage(conn, source)? {
        if let Some((_, target_seq)) = target_lineage.iter().find(|(v, _)| *v == volume_id) {
            let seq = source_seq.min(*target_seq);
            // Clones copy events with their original timestamps, so this splits the target's
            // history into the part it shares with the source and the part after.
            let since = match load_events(conn, volume_id, Some(seq))?.last() {
                Some(event) => event.created_at(),
                None => DateTime::<Utc>::MIN_UTC,
            };
            return Ok(MergeBase {
                volume_id,
                seq,
                since,
            });
        }
    }
    Err(EngineError::VolumesNotRelated { source, target })
}

/// One change a merge makes to its target.
enum Write<'a> {
    /// Copy over an entry the target never had.
    New { origin: EntryId, entry: &'a Entry },
    /// Update or restore an existing target entry to the source's version, or delete it (None).
    Existing {
        event_type: EventType,
        entry_id: EntryId,
        entry: Option<&'a Entry>,
    },
}

fn write_change(conn: &Connection, target: VolumeId, write: &Write, at: &str) -> Result<()> {
    match *write {
        Write::New { origin, entry } => {
            let tags = join_tags(entry.tags());
            let data = EntryData {
                content: entry.content(),
                tags: &tags,
            };
            let entry_id = insert_entry_row(conn, target, data, entry.source(), at, Some(origin))?;
            let source = Some(entry.source());
            append_event(
                conn,
                target,
                EventType::PutEntry,
                entry_id,
                Some(data),
                source,
                at,
            )?;
        }
        Write::Existing {
            event_type,
            entry_id,
            entry,
        } => {
            let tags = entry.map(|entry| join_tags(entry.tags()));
            let data = entry.zip(tags.as_deref()).map(|(entry, tags)| EntryData {
                content: entry.content(),
                tags,
            });
            append_event(conn, target, event_type, entry_id, data, None, at)?;
            project_event(conn, event_type, entry_id, data, at)?;
        }
    }
    Ok(())
}

/// The versions (content and tags) entries have had, keyed by entry id.
type Versions = HashMap<EntryId, Vec<(String, Vec<String>)>>;

/// Every version each entry of a volume has had after `since`.
fn versions_since(
    conn: &Connection,
    volume_id: VolumeId,
    since: DateTime<Utc>,
) -> Result<Versions> {
    let mut versions = Versions::new();
    let events = load_events(conn, volume_id, None)?;
    for event in events.iter().filter(|event| event.created_at() > since) {
        if let Some(content) = event.content() {
            versions
                .entry(event.entry_id())
                .or_default()
                .push((content.to_string(), event.tags().to_vec()));
        }
    }
    Ok(versions)
}

/// What a merge changes: the report's entry lists, plus the writes that make those changes.
struct MergePlan<'a> {
    added: Vec<Entry>,
    removed: Vec<Entry>,
    modified: Vec<EntryModification>,
    conflicts: Vec<MergeConflict>,
    writes: Vec<Write<'a>>,
}

/// Decides, entry by entry, what merging the source into the target changes. All maps are keyed
/// by origin except `target_versions` (the target's versions since the base), which is keyed by
/// the target's entry ids.
fn plan_merge<'a>(
    base: &BTreeMap<EntryId, Entry>,
    source_entries: &'a BTreeMap<EntryId, Entry>,
    target_entries: &BTreeMap<EntryId, Entry>,
    target_ids: &HashMap<EntryId, EntryId>,
    target_versions: &Versions,
) -> MergePlan<'a> {
    let mut plan = MergePlan {
        added: Vec::new(),
        removed: Vec::new(),
        modified: Vec::new(),
        conflicts: Vec::new(),
        writes: Vec::new(),
    };
    let all_origins: BTreeSet<EntryId> = base
        .keys()
        .chain(source_entries.keys())
        .chain(target_entries.keys())
        .copied()
        .collect();

    for origin in all_origins {
        let base_entry = base.get(&origin);
        let source_entry = source_entries.get(&origin);
        let target_entry = target_entries.get(&origin);
        let source_data = entry_data(source_entry);
        if source_data == entry_data(target_entry) || source_data == entry_data(base_entry) {
            continue;
        }
        // Since the base, the target already held this exact version, for example through a merge
        // via another volume, and has moved on. Its later choice wins.
        let target_had_it = target_ids
            .get(&origin)
            .and_then(|entry_id| target_versions.get(entry_id))
            .is_some_and(|versions| {
                versions
                    .iter()
                    .any(|(content, tags)| Some((content.as_str(), tags.as_slice())) == source_data)
            });
        if target_had_it {
            continue;
        }
        if entry_data(target_entry) != entry_data(base_entry) {
            plan.conflicts.push(MergeConflict {
                base: base_entry.cloned(),
                source: source_entry.cloned(),
                target: target_entry.cloned(),
            });
            continue;
        }

        // Only the source changed this entry since the base: bring its version over.
        match (source_entry, target_entry) {
            (Some(new), Some(old)) => {
                plan.writes.push(Write::Existing {
                    event_type: EventType::UpdateEntry,
                    entry_id: old.id(),
                    entry: Some(new),
                });
                plan.modified.push(EntryModification {
                    before: old.clone(),
                    after: new.clone(),
                });
            }
            (Some(new), None) => {
                plan.writes.push(match target_ids.get(&origin) {
                    Some(&entry_id) => Write::Existing {
                        event_type: EventType::RestoreEntry,
                        entry_id,
                        entry: Some(new),
                    },
                    None => Write::New { origin, entry: new },
                });
                plan.added.push(new.clone());
            }
            (None, Some(old)) => {
                plan.writes.push(Write::Existing {
                    event_type: EventType::DeleteEntry,
                    entry_id: old.id(),
                    entry: None,
                });
                plan.removed.push(old.clone());
            }
            // Absent on both sides compares equal, so it was skipped above.
            (None, None) => {}
        }
    }
    plan
}

/// Applies the changes `source` made since its common base with `target` to `target`.
///
/// Every entry is compared three ways: base, source and target. A change made only on the source
/// is applied to the target by appending events, so history is never rewritten. A change made
/// only on the target is kept, and so is a target that already held the source's version and has
/// changed it since. An entry changed differently on both sides is a conflict: it is left alone
/// and reported. After a merge, a reported conflict counts as settled in favor of the target; to
/// take the source's version, update the target's entry (or add it again if the target deleted
/// it).
///
/// A real merge that changed anything or found conflicts also leaves a "merged into <target>"
/// snapshot on the source, which marks the base for the next merge between the two volumes.
/// With `dry_run`, nothing is written and the report shows what a real merge would do.
pub fn merge_volumes(
    conn: &Connection,
    source: VolumeId,
    target: VolumeId,
    dry_run: bool,
) -> Result<MergeReport> {
    let tx = write_tx(conn)?;
    get_volume(&tx, source)?;
    let target_volume = get_volume(&tx, target)?;
    let merge_base = merge_base(&tx, source, target)?;

    let base = live_by_origin(&tx, merge_base.volume_id, merge_base.seq)?;
    let source_entries = current_by_origin(&tx, source)?;
    let target_entries = current_by_origin(&tx, target)?;
    // Deleted target entries are included so they get restored rather than duplicated.
    let target_ids: HashMap<EntryId, EntryId> = entry_origins(&tx, target)?
        .into_iter()
        .map(|(id, origin)| (origin, id))
        .collect();
    let target_versions = versions_since(&tx, target, merge_base.since)?;

    let MergePlan {
        added,
        removed,
        modified,
        conflicts,
        writes,
    } = plan_merge(
        &base,
        &source_entries,
        &target_entries,
        &target_ids,
        &target_versions,
    );
    let report = MergeReport {
        source_volume_id: source,
        target_volume_id: target,
        base_volume_id: merge_base.volume_id,
        base_seq: merge_base.seq,
        dry_run,
        added,
        removed,
        modified,
        conflicts,
        events_appended: writes.len(),
    };

    if dry_run || (writes.is_empty() && report.conflicts.is_empty()) {
        return Ok(report);
    }
    let at = now();
    for write in &writes {
        write_change(&tx, target, write, &at)?;
    }
    // Bookmark how far the target has taken in the source, as the base for the next merge.
    tx.execute(
        "INSERT INTO snapshots (volume_id, event_seq_pointer, created_at, label, merged_into_volume_id)
            VALUES (?1, ?2, ?3, ?4, ?5)",
        (
            source,
            head_seq(&tx, source)?,
            &at,
            format!("merged into {}", target_volume.name()),
            target,
        ),
    )?;
    tx.commit()?;
    Ok(report)
}
