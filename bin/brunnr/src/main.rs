//! `brunnr` — Mímisbrunnr pool management CLI (DESIGN Appendix A).
//!
//! Subcommands:
//!
//! - `create <disk-spec>...` — format a fresh pool.
//! - `status` — show pool health, per-disk capacity / used / state, tier
//!   breakdown.
//! - `add-disk <path>` / `remove-disk <id-or-path>` — disk lifecycle.
//! - `mount <mountpoint>` / `mount-unix --context <ctx> <mountpoint>` —
//!   FUSE mounts (feature-gated behind `--features fuse`).
//! - `unmount <mountpoint>` — platform-specific unmount.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod commands;

#[derive(Parser)]
#[command(name = "brunnr", about = "Mímisbrunnr pool management")]
struct Cli {
    /// Path to the pool configuration TOML. Defaults to `./pool.toml`.
    #[arg(long, global = true, default_value = "pool.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a fresh pool. Each disk-spec is
    /// `<path>[:tier=hot|warm|cold|glacier][:media=nvme|ssd|hdd|smr|remote][:capacity=N|auto]`.
    Create {
        /// Disk specifications.
        #[arg(required = true)]
        disks: Vec<String>,

        /// Node id for this machine.
        #[arg(long, default_value = "0")]
        node_id: u16,
    },

    /// Show pool status.
    Status,

    /// Add a disk to the existing pool.
    AddDisk {
        /// Path to the disk file or block device.
        path: PathBuf,

        /// Storage tier: hot | warm | cold | glacier.
        #[arg(long, default_value = "warm")]
        tier: String,

        /// Media type: nvme | ssd | hdd | smr | remote.
        #[arg(long, default_value = "ssd")]
        media: String,

        /// Capacity in bytes. Omit to inherit from the existing file size.
        #[arg(long)]
        capacity: Option<u64>,
    },

    /// Begin draining a disk (does not yet evacuate data).
    RemoveDisk {
        /// Disk id (decimal) or path.
        target: String,
    },

    /// Mount the full /tags + /ctx filesystem (feature = "fuse").
    Mount {
        /// Mount point.
        mountpoint: PathBuf,
    },

    /// Mount a single path-context subtree (feature = "fuse").
    ///
    /// **Phase 7b note:** this currently mounts the *entire* filesystem;
    /// the requested context will appear under
    /// `<mountpoint>/ctx/<context>`. Subtree-only mounts are a TODO.
    MountUnix {
        /// Mount point.
        mountpoint: PathBuf,

        /// Path-context name to expose.
        #[arg(long)]
        context: String,
    },

    /// Unmount a mountpoint (best-effort, platform-specific).
    Unmount {
        /// Mount point.
        mountpoint: PathBuf,
    },
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Create { disks, node_id } => {
            commands::cmd_create(&cli.config, &disks, node_id)
        }
        Commands::Status => commands::cmd_status(&cli.config),
        Commands::AddDisk {
            path,
            tier,
            media,
            capacity,
        } => commands::cmd_add_disk(&cli.config, &path, &tier, &media, capacity),
        Commands::RemoveDisk { target } => commands::cmd_remove_disk(&cli.config, &target),
        Commands::Mount { mountpoint } => commands::cmd_mount(&cli.config, &mountpoint, None),
        Commands::MountUnix {
            mountpoint,
            context,
        } => commands::cmd_mount(&cli.config, &mountpoint, Some(&context)),
        Commands::Unmount { mountpoint } => commands::cmd_unmount(&mountpoint),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
