use clap::{Parser, Subcommand};
use core_types::{Entry, EntrySource, Event, MemoryDiff};
use engine_api::{
    add_entry, clone_volume, create_volume, delete_entry, diff_snapshots, entries_at_snapshot,
    entry_history, list_entries, list_events, list_snapshots, list_volumes, open_db,
    rebuild_projection, rollback_to_snapshot, search, snapshot, update_entry,
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
}

#[derive(Subcommand)]
enum EntryCommands {
    /// Add a new entry to a volume
    Add {
        volume_id: i64,
        content: String,
        #[arg(long, default_value = "user")]
        source: EntrySource,
    },

    /// Replace the content of an entry
    Update { entry_id: i64, content: String },

    /// Delete an entry (it stays in history)
    Delete { entry_id: i64 },

    /// List all entries in a volume
    List { volume_id: i64 },

    /// Search entries in a volume by content
    Search { volume_id: i64, query: String },

    /// Show every change made to an entry
    History { entry_id: i64 },
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

fn print_entries(entries: &[Entry]) {
    for e in entries {
        println!("[{}] {}", e.id(), e.content());
    }
}

fn print_events(events: &[Event]) {
    for ev in events {
        let content = ev.content().map(|c| format!(" {c}")).unwrap_or_default();
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
    if diff.is_empty() {
        println!("(no changes)");
    }
    for e in &diff.added {
        println!("+ [{}] {}", e.id(), e.content());
    }
    for e in &diff.removed {
        println!("- [{}] {}", e.id(), e.content());
    }
    for m in &diff.modified {
        println!("~ [{}] {}", m.before.id(), m.before.content());
        println!("  -> {}", m.after.content());
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
        },
        Commands::Entry { command } => match command {
            EntryCommands::Add {
                volume_id,
                content,
                source,
            } => {
                let entry = add_entry(&conn, volume_id, content, source)?;
                println!("Added entry (id: {})", entry.id());
            }
            EntryCommands::Update { entry_id, content } => {
                let entry = update_entry(&conn, entry_id, content)?;
                println!("Updated entry (id: {})", entry.id());
            }
            EntryCommands::Delete { entry_id } => {
                let entry = delete_entry(&conn, entry_id)?;
                println!("Deleted entry (id: {})", entry.id());
            }
            EntryCommands::List { volume_id } => print_entries(&list_entries(&conn, volume_id)?),
            EntryCommands::Search { volume_id, query } => {
                print_entries(&search(&conn, volume_id, query)?)
            }
            EntryCommands::History { entry_id } => print_events(&entry_history(&conn, entry_id)?),
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
