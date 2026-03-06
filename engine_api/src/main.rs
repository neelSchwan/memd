use clap::{Parser, Subcommand};
use core_types::EntrySource;
use engine_api::{
    add_entry, clone_volume, create_volume, init_db, list_entries, list_snapshots, list_volumes,
    search, snapshot,
};
use rusqlite::{Connection, Result};

#[derive(Parser)]
#[command(name = "memd")]
struct Cli {
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
    Clone { snapshot_id: i64 },
}

#[derive(Subcommand)]
enum VolumeCommands {
    /// Create a new memory volume with the given name
    Create { name: String },

    /// List all existing volumes
    List,
}

#[derive(Subcommand)]
enum EntryCommands {
    /// Add a new entry to a volume
    Add {
        volume_id: i64,
        content: String,
        #[arg(long, default_value = "user")]
        source: String,
    },

    /// List all entries in a volume
    List { volume_id: i64 },

    /// Search entries in a volume by content
    Search { volume_id: i64, query: String },
}

#[derive(Subcommand)]
enum SnapshotCommands {
    /// Snapshot the current state of a volume
    Create { volume_id: i64, label: String },

    /// List all snapshots for a volume
    List { volume_id: i64 },
}

fn parse_source(source: &str) -> EntrySource {
    match source {
        "agent" => EntrySource::Agent,
        "tool" => EntrySource::Tool,
        _ => EntrySource::User,
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let conn = Connection::open("memd.db")?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    init_db(&conn)?;

    match cli.command {
        Commands::Volume { command } => match command {
            VolumeCommands::Create { name } => {
                let vol = create_volume(&conn, name)?;
                println!("Created Volume: {} (id: {})", vol.name(), vol.id());
            }
            VolumeCommands::List => {
                let volumes = list_volumes(&conn)?;
                for v in volumes {
                    println!("{}: {}", v.id(), v.name());
                }
            }
        },
        Commands::Entry { command } => match command {
            EntryCommands::Add {
                volume_id,
                content,
                source,
            } => {
                let src = parse_source(&source);
                let entry = add_entry(&conn, volume_id, content, src)?;
                println!("Added entry (id: {})", entry.id());
            }
            EntryCommands::List { volume_id } => {
                let entries = list_entries(&conn, volume_id)?;
                for e in entries {
                    println!("[{}] {}", e.id(), e.content());
                }
            }
            EntryCommands::Search { volume_id, query } => {
                let entries = search(&conn, volume_id, query)?;
                for e in entries {
                    println!("[{}] {}", e.id(), e.content());
                }
            }
        },
        Commands::Snapshot { command } => match command {
            SnapshotCommands::Create { volume_id, label } => {
                let snap = snapshot(&conn, volume_id, label)?;
                println!("Snapshot created (id: {})", snap.id());
            }
            SnapshotCommands::List { volume_id } => {
                let snapshots = list_snapshots(&conn, volume_id)?;
                for s in snapshots {
                    println!("{}: {} (seq: {})", s.id(), s.label(), s.event_seq_pointer());
                }
            }
        },
        Commands::Clone { snapshot_id } => {
            let vol = clone_volume(&conn, snapshot_id)?;
            println!("Cloned volume: {} (id: {})", vol.name(), vol.id());
        }
    }

    Ok(())
}
