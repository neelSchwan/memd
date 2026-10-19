//! Diffing and merging between a volume and its branches.
//!
//! Entries are matched across volumes by origin (see `entry_origins`), so an entry and its copy in
//! a branch are treated as the same entry. A volume never holds two entries with the same origin.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use core_types::{
    Entry, EntryId, EntryModification, EventType, MergeConflict, MergeReport, SeqNumber,
    VolumeDiff, VolumeId,
};
use rusqlite::{Connection, OptionalExtension};

use crate::{
    EngineError, EntryData, Result, append_event, entry_data, entry_origins, get_snapshot,
    get_volume, head_seq, insert_entry_row, join_tags, now, project_event, state_map_at, write_tx,
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
        crate::compare_states(current_by_origin(conn, from)?, current_by_origin(conn, to)?);
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

/// The state both volumes last agreed on, as (volume, seq): the most recent merge between them,
/// or else the point where their histories split.
fn merge_base(
    conn: &Connection,
    source: VolumeId,
    target: VolumeId,
) -> Result<(VolumeId, SeqNumber)> {
    // A merge leaves a snapshot on its source at the seq the target took in.
    let last_merge = conn
        .query_row(
            "SELECT volume_id, event_seq_pointer FROM snapshots
                WHERE (volume_id = ?1 AND merged_into_volume_id = ?2)
                   OR (volume_id = ?2 AND merged_into_volume_id = ?1)
                ORDER BY id DESC LIMIT 1",
            (source, target),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some(base) = last_merge {
        return Ok(base);
    }

    let target_lineage = lineage(conn, target)?;
    for (volume_id, source_seq) in lineage(conn, source)? {
        if let Some((_, target_seq)) = target_lineage.iter().find(|(v, _)| *v == volume_id) {
            return Ok((volume_id, source_seq.min(*target_seq)));
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

/// Applies the changes `source` made since its common base with `target` to `target`.
///
/// Every entry is compared three ways: base, source and target. A change made only on the source
/// is applied to the target by appending events, so history is never rewritten. A change made
/// only on the target is kept. An entry changed differently on both sides is a conflict: it is
/// left alone and reported. After a merge, a reported conflict counts as settled in favor of the
/// target; to take the source's version, update the target's entry.
///
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
    let (base_volume_id, base_seq) = merge_base(&tx, source, target)?;

    let base = live_by_origin(&tx, base_volume_id, base_seq)?;
    let theirs = current_by_origin(&tx, source)?;
    let ours = current_by_origin(&tx, target)?;
    // Deleted target entries are included so they get restored rather than duplicated.
    let target_ids: HashMap<EntryId, EntryId> = entry_origins(&tx, target)?
        .into_iter()
        .map(|(id, origin)| (origin, id))
        .collect();

    let mut report = MergeReport {
        source_volume_id: source,
        target_volume_id: target,
        base_volume_id,
        base_seq,
        dry_run,
        added: Vec::new(),
        removed: Vec::new(),
        modified: Vec::new(),
        conflicts: Vec::new(),
        events_appended: 0,
    };
    let mut writes = Vec::new();
    let all_origins: BTreeSet<EntryId> = base
        .keys()
        .chain(theirs.keys())
        .chain(ours.keys())
        .copied()
        .collect();
    for origin in all_origins {
        let base_entry = base.get(&origin);
        let source_entry = theirs.get(&origin);
        let target_entry = ours.get(&origin);
        let source_data = entry_data(source_entry);
        if source_data == entry_data(target_entry) || source_data == entry_data(base_entry) {
            continue;
        }
        if entry_data(target_entry) != entry_data(base_entry) {
            report.conflicts.push(MergeConflict {
                base: base_entry.cloned(),
                source: source_entry.cloned(),
                target: target_entry.cloned(),
            });
            continue;
        }

        // Only the source changed this entry since the base: bring its version over.
        match (source_entry, target_entry) {
            (None, Some(old)) => {
                writes.push(Write::Existing {
                    event_type: EventType::DeleteEntry,
                    entry_id: old.id(),
                    entry: None,
                });
                report.removed.push(old.clone());
            }
            (Some(new), Some(old)) => {
                writes.push(Write::Existing {
                    event_type: EventType::UpdateEntry,
                    entry_id: old.id(),
                    entry: Some(new),
                });
                report.modified.push(EntryModification {
                    before: old.clone(),
                    after: new.clone(),
                });
            }
            (Some(new), None) => {
                writes.push(match target_ids.get(&origin) {
                    Some(&entry_id) => Write::Existing {
                        event_type: EventType::RestoreEntry,
                        entry_id,
                        entry: Some(new),
                    },
                    None => Write::New { origin, entry: new },
                });
                report.added.push(new.clone());
            }
            (None, None) => unreachable!("absent on both sides compares equal above"),
        }
    }
    report.events_appended = writes.len();

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
