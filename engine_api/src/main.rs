use clap::{Parser, Subcommand};
use core_types::{Entry, EntryModification, EntrySource, Event, MemoryDiff, MergeReport};
use engine_api::{
    add_entry, clone_volume, create_volume, delete_entry, diff_snapshots, diff_volumes,
    entries_at_snapshot, entry_history, list_entries, list_events, list_snapshots, list_volumes,
    merge_volumes, open_db, rebuild_projection, rollback_to_snapshot, search, snapshot, tag_entry,
    update_entry,
};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "memd", about = "Versioned memory store for AI agents")]
struct Cli {
    /// Path to the SQLite database
    #[arg(long, env = "MEMD_DB", default_value = "memd.db", global = true)]
    db: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage memory volumes
    Volume {
        #[command(subcommand)]
        command: VolumeCommands,
    },

    /// Manage memory entries
    Entry {
        #[command(subcommand)]
        command: EntryCommands,
    },

    /// Manage snapshots
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommands,
    },

    /// Clone a volume from a snapshot
    Clone {
        snapshot_id: i64,
        /// Name for the new volume (defaults to <source>_clone)
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum VolumeCommands {
    /// Create a new memory volume with the given name
    Create { name: String },

    /// List all existing volumes
    List,

    /// Show a volume's full event log
    Log { volume_id: i64 },

    /// Rebuild a volume's entries from its event log
    Rebuild { volume_id: i64 },

    /// Compare the current entries of two volumes, e.g. a branch and its parent
    Diff {
        from_volume_id: i64,
        to_volume_id: i64,
    },

    /// Apply the changes made on one volume to a related volume
    Merge {
        source_volume_id: i64,
        target_volume_id: i64,
        /// Show what would change without writing anything
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum EntryCommands {
    /// Add a new entry to a volume
    Add {
        volume_id: i64,
        content: String,
        #[arg(long, default_value = "user")]
        source: EntrySource,
        /// Tag the entry (repeat for several tags)
        #[arg(long = "tag")]
        tags: Vec<String>,
    },

    /// Replace the content of an entry
    Update { entry_id: i64, content: String },

    /// Replace all tags of an entry (no tags removes them all)
    Tag { entry_id: i64, tags: Vec<String> },

    /// Delete an entry (it stays in history)
    Delete { entry_id: i64 },

    /// List all entries in a volume
    List {
        volume_id: i64,
        /// Only entries with this tag (repeat to require several)
        #[arg(long = "tag")]
        tags: Vec<String>,
    },

    /// Search entries in a volume by content
    Search {
        volume_id: i64,
        query: String,
        /// Only entries with this tag (repeat to require several)
        #[arg(long = "tag")]
        tags: Vec<String>,
    },

    /// Show every change made to an entry
    History { volume_id: i64, entry_id: i64 },
}

#[derive(Subcommand)]
enum SnapshotCommands {
    /// Snapshot the current state of a volume
    Create { volume_id: i64, label: String },

    /// List all snapshots for a volume
    List { volume_id: i64 },

    /// Show the entries a volume had when the snapshot was taken
    Show { snapshot_id: i64 },

    /// Compare a snapshot with another snapshot, or with the current state
    Diff {
        from_snapshot_id: i64,
        to_snapshot_id: Option<i64>,
    },

    /// Roll a volume back to a snapshot (appends compensating events)
    Rollback { snapshot_id: i64 },
}

/// Formats content followed by its tags, e.g. `use postgres #db #prod`.
fn with_tags(content: &str, tags: &[String]) -> String {
    tags.iter()
        .fold(content.to_string(), |text, tag| format!("{text} #{tag}"))
}

fn print_entries(entries: &[Entry]) {
    for e in entries {
        println!("[{}] {}", e.id(), with_tags(e.content(), e.tags()));
    }
}

fn print_events(events: &[Event]) {
    for ev in events {
        let content = ev
            .content()
            .map(|c| format!(" {}", with_tags(c, ev.tags())))
            .unwrap_or_default();
        println!(
            "#{} {} {} entry={}{}",
            ev.seq(),
            ev.created_at().to_rfc3339(),
            ev.event_type().as_str(),
            ev.entry_id(),
            content
        );
    }
}

fn print_diff(diff: &MemoryDiff) {
    println!("seq {} -> {}", diff.from_seq, diff.to_seq);
    print_changes(&diff.added, &diff.removed, &diff.modified);
}

fn print_merge(report: &MergeReport) {
    print_changes(&report.added, &report.removed, &report.modified);
    let describe = |entry: &Option<Entry>| match entry {
        Some(e) => with_tags(e.content(), e.tags()),
        None => "(deleted)".to_string(),
    };
    for conflict in &report.conflicts {
        println!("! conflict, left as is: {}", describe(&conflict.target));
        println!("  source has: {}", describe(&conflict.source));
    }
    let verb = if report.dry_run {
        "Would merge"
    } else {
        "Merged"
    };
    println!(
        "{verb} volume {} into {}: {} events, {} conflicts skipped (base: volume {} seq {})",
        report.source_volume_id,
        report.target_volume_id,
        report.events_appended,
        report.conflicts.len(),
        report.base_volume_id,
        report.base_seq
    );
}

fn print_changes(added: &[Entry], removed: &[Entry], modified: &[EntryModification]) {
    if added.is_empty() && removed.is_empty() && modified.is_empty() {
        println!("(no changes)");
    }
    for e in added {
        println!("+ [{}] {}", e.id(), with_tags(e.content(), e.tags()));
    }
    for e in removed {
        println!("- [{}] {}", e.id(), with_tags(e.content(), e.tags()));
    }
    for m in modified {
        let (before, after) = (&m.before, &m.after);
        println!(
            "~ [{}] {}",
            before.id(),
            with_tags(before.content(), before.tags())
        );
        println!("  -> {}", with_tags(after.content(), after.tags()));
    }
}

fn run(cli: Cli) -> engine_api::Result<()> {
    let conn = open_db(&cli.db)?;

    match cli.command {
        Commands::Volume { command } => match command {
            VolumeCommands::Create { name } => {
                let vol = create_volume(&conn, name)?;
                println!("Created Volume: {} (id: {})", vol.name(), vol.id());
            }
            VolumeCommands::List => {
                for v in list_volumes(&conn)? {
                    match v.parent_snapshot_id() {
                        Some(parent) => {
                            println!("{}: {} (from snapshot {})", v.id(), v.name(), parent)
                        }
                        None => println!("{}: {}", v.id(), v.name()),
                    }
                }
            }
            VolumeCommands::Log { volume_id } => print_events(&list_events(&conn, volume_id)?),
            VolumeCommands::Rebuild { volume_id } => {
                let corrected = rebuild_projection(&conn, volume_id)?;
                println!("Rebuilt volume {volume_id}: {corrected} entries corrected");
            }
            VolumeCommands::Diff {
                from_volume_id,
                to_volume_id,
            } => {
                let diff = diff_volumes(&conn, from_volume_id, to_volume_id)?;
                print_changes(&diff.added, &diff.removed, &diff.modified);
            }
            VolumeCommands::Merge {
                source_volume_id,
                target_volume_id,
                dry_run,
            } => {
                let report = merge_volumes(&conn, source_volume_id, target_volume_id, dry_run)?;
                print_merge(&report);
            }
        },
        Commands::Entry { command } => match command {
            EntryCommands::Add {
                volume_id,
                content,
                source,
                tags,
            } => {
                let entry = add_entry(&conn, volume_id, content, source, tags)?;
                println!("Added entry (id: {})", entry.id());
            }
            EntryCommands::Update { entry_id, content } => {
                let entry = update_entry(&conn, entry_id, content)?;
                println!("Updated entry (id: {})", entry.id());
            }
            EntryCommands::Tag { entry_id, tags } => {
                let entry = tag_entry(&conn, entry_id, tags)?;
                println!("Tagged entry (id: {})", entry.id());
            }
            EntryCommands::Delete { entry_id } => {
                let entry = delete_entry(&conn, entry_id)?;
                println!("Deleted entry (id: {})", entry.id());
            }
            EntryCommands::List { volume_id, tags } => {
                print_entries(&list_entries(&conn, volume_id, &tags)?)
            }
            EntryCommands::Search {
                volume_id,
                query,
                tags,
            } => print_entries(&search(&conn, volume_id, query, &tags)?),
            EntryCommands::History {
                volume_id,
                entry_id,
            } => print_events(&entry_history(&conn, volume_id, entry_id)?),
        },
        Commands::Snapshot { command } => match command {
            SnapshotCommands::Create { volume_id, label } => {
                let snap = snapshot(&conn, volume_id, label)?;
                println!("Snapshot created (id: {})", snap.id());
            }
            SnapshotCommands::List { volume_id } => {
                for s in list_snapshots(&conn, volume_id)? {
                    println!("{}: {} (seq: {})", s.id(), s.label(), s.event_seq_pointer());
                }
            }
            SnapshotCommands::Show { snapshot_id } => {
                print_entries(&entries_at_snapshot(&conn, snapshot_id)?)
            }
            SnapshotCommands::Diff {
                from_snapshot_id,
                to_snapshot_id,
            } => print_diff(&diff_snapshots(&conn, from_snapshot_id, to_snapshot_id)?),
            SnapshotCommands::Rollback { snapshot_id } => {
                let report = rollback_to_snapshot(&conn, snapshot_id)?;
                println!(
                    "Rolled volume {} back to snapshot {}: {} events appended (seq {} -> {})",
                    report.volume_id,
                    report.snapshot_id,
                    report.events_appended,
                    report.from_seq,
                    report.to_seq
                );
            }
        },
        Commands::Clone { snapshot_id, name } => {
            let vol = clone_volume(&conn, snapshot_id, name)?;
            println!("Cloned volume: {} (id: {})", vol.name(), vol.id());
        }
    }

    Ok(())
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
