use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use mimisbrunnr::{
    pool::{DiskDescriptor, MediaType, PoolConfig, PoolManager, StorageTier},
    storage::{ExtentLayout, FileBlockDevice, Superblock, ZoneLayout, ZoneType},
    wal::WriteAheadLog,
};

#[derive(Parser)]
#[command(name = "brunnr", about = "Mímisbrunnr pool management")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new pool across one or more disk files / devices.
    Create {
        /// Paths to disk files or block devices.
        #[arg(required = true)]
        disks: Vec<PathBuf>,

        /// Size of each file-backed disk in MiB. Repeatable — one per disk.
        /// If fewer sizes than disks, the last value is reused.
        #[arg(long, default_value = "256")]
        size_mib: Vec<u64>,

        /// Node ID for this machine.
        #[arg(long, default_value = "0")]
        node_id: u16,

        /// Storage tier for each disk (hot, warm, cold). Repeatable.
        #[arg(long)]
        tier: Vec<String>,
    },

    /// Show pool status.
    Status {
        /// Paths to disk files or block devices forming the pool.
        #[arg(required = true)]
        disks: Vec<PathBuf>,
    },

    /// Add a disk to an existing pool.
    AddDisk {
        /// Path to the new disk file or block device.
        disk: PathBuf,

        /// An existing disk in the pool (to read pool config from).
        #[arg(long)]
        pool_disk: PathBuf,

        /// Size in MiB (for file-backed disks).
        #[arg(long, default_value = "256")]
        size_mib: u64,

        /// Storage tier.
        #[arg(long, default_value = "warm")]
        tier: String,
    },

    /// Begin draining a disk for removal.
    RemoveDisk {
        /// Path to the disk to remove.
        disk: PathBuf,

        /// An existing disk in the pool.
        #[arg(long)]
        pool_disk: PathBuf,
    },

    /// Mount a path context as a FUSE filesystem.
    MountUnix {
        /// Mount point directory.
        mountpoint: PathBuf,

        /// Path to pool.toml.
        #[arg(long)]
        pool: PathBuf,

        /// Path context name to mount.
        #[arg(long)]
        context: String,
    },
}

fn parse_tier(s: &str) -> StorageTier {
    match s.to_lowercase().as_str() {
        "hot" => StorageTier::Hot,
        "warm" => StorageTier::Warm,
        "cold" => StorageTier::Cold,
        "glacier" => StorageTier::Glacier,
        _ => {
            eprintln!("warning: unknown tier '{s}', defaulting to warm");
            StorageTier::Warm
        }
    }
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::Create {
            disks,
            size_mib,
            node_id,
            tier,
        } => cmd_create(&disks, &size_mib, node_id, &tier),
        Commands::Status { disks } => cmd_status(&disks),
        Commands::AddDisk {
            disk,
            pool_disk,
            size_mib,
            tier,
        } => cmd_add_disk(&disk, &pool_disk, size_mib, &tier),
        Commands::RemoveDisk { disk, pool_disk } => cmd_remove_disk(&disk, &pool_disk),
        Commands::MountUnix {
            mountpoint,
            pool,
            context,
        } => cmd_mount_unix(&pool, &context, &mountpoint),
    }
}

fn cmd_create(disk_paths: &[PathBuf], sizes_mib: &[u64], node_id: u16, tiers: &[String]) {
    if disk_paths.is_empty() {
        eprintln!("error: at least one disk path required");
        std::process::exit(1);
    }

    let mut pool = PoolManager::new();
    let mut pool_config = PoolConfig::new(node_id);

    println!(
        "Creating Mímisbrunnr pool with {} disk(s)...",
        disk_paths.len()
    );

    for (i, path) in disk_paths.iter().enumerate() {
        // Per-disk size: use sizes_mib[i], or last value, or default 256
        let size_mib = sizes_mib
            .get(i)
            .or_else(|| sizes_mib.last())
            .copied()
            .unwrap_or(256);
        let capacity = size_mib * 1024 * 1024;

        let tier = tiers.get(i).map(|s| parse_tier(s)).unwrap_or_else(|| {
            if i == 0 {
                StorageTier::Hot
            } else {
                StorageTier::Warm
            }
        });

        // Determine media type from tier (heuristic for file-backed)
        let media = match tier {
            StorageTier::Hot => MediaType::NVMe,
            StorageTier::Warm => MediaType::Ssd,
            StorageTier::Cold => MediaType::Hdd,
            StorageTier::Glacier => MediaType::Remote,
        };

        let disk_id = i as u16;

        // Canonicalize the path for storage in config
        let abs_path = std::fs::canonicalize(path.parent().unwrap_or(path))
            .unwrap_or_else(|_| path.parent().unwrap_or(path).to_path_buf())
            .join(path.file_name().unwrap_or_default());

        // Create/open the file-backed device
        let dev = match FileBlockDevice::open(path, capacity) {
            Ok(dev) => dev,
            Err(e) => {
                eprintln!("error: failed to open {}: {e}", path.display());
                std::process::exit(1);
            }
        };

        // Compute zone layout
        let layout = match ZoneLayout::compute(capacity) {
            Some(l) => l,
            None => {
                eprintln!("error: disk too small (need at least ~68 MiB)");
                std::process::exit(1);
            }
        };

        // Write superblock
        let sb = Superblock::new(node_id, disk_id, layout);
        if let Err(e) = sb.write_to(&dev) {
            eprintln!(
                "error: failed to write superblock to {}: {e}",
                path.display()
            );
            std::process::exit(1);
        }

        // Initialize WAL
        match WriteAheadLog::create(&dev, layout.wal_offset, mimisbrunnr::storage::WAL_SIZE) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("error: failed to initialize WAL on {}: {e}", path.display());
                std::process::exit(1);
            }
        }

        // Register in pool manager and config
        let desc = DiskDescriptor::new(disk_id, capacity, media)
            .with_tier(tier)
            .with_path(abs_path.to_string_lossy());

        pool.add_disk(desc).unwrap();
        pool_config.add_disk(disk_id, abs_path.to_string_lossy(), tier.name(), capacity);

        // Show zone layout
        let el = ExtentLayout::from(&layout);
        println!(
            "  disk {disk_id}: {} ({size_mib} MiB, tier: {tier})",
            path.display(),
        );
        println!(
            "    zones: index {} KiB, meta {} KiB, blob {} KiB",
            el.zone_size(ZoneType::Index) / 1024,
            el.zone_size(ZoneType::Metadata) / 1024,
            el.zone_size(ZoneType::Blob) / 1024,
        );
    }

    // Write pool.toml alongside the first disk
    let config_dir = disk_paths[0]
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let config_path = config_dir.join("pool.toml");
    if let Err(e) = pool_config.save(&config_path) {
        eprintln!("error: failed to write pool config: {e}");
        std::process::exit(1);
    }

    println!("Pool created successfully.");
    println!(
        "  Total capacity: {} MiB",
        pool.total_capacity() / (1024 * 1024)
    );
    println!("  Disks: {}", pool.online_count());
    println!("  Config: {}", config_path.display());
}

fn cmd_status(disk_paths: &[PathBuf]) {
    if disk_paths.is_empty() {
        eprintln!("error: specify at least one disk path");
        std::process::exit(1);
    }

    println!("Mímisbrunnr Pool Status");
    println!("{}", "=".repeat(60));

    let mut total_capacity = 0u64;

    for (i, path) in disk_paths.iter().enumerate() {
        // Try to read superblock
        let dev = match FileBlockDevice::open(path, 0) {
            Ok(dev) => dev,
            Err(e) => {
                eprintln!("  disk {i}: {} — error: {e}", path.display());
                continue;
            }
        };

        match Superblock::read_from(&dev) {
            Ok(sb) => {
                let el = ExtentLayout::from(&sb.layout);
                let cap_mib = sb.layout.device_capacity / (1024 * 1024);

                println!("  Disk {} (node={}, disk={}):", i, sb.node_id, sb.disk_id);
                println!("    Path:      {}", path.display());
                println!("    Capacity:  {cap_mib} MiB");
                println!(
                    "    Index:     {} KiB ({} extent(s))",
                    el.zone_size(ZoneType::Index) / 1024,
                    el.extents(ZoneType::Index).len(),
                );
                println!(
                    "    Metadata:  {} KiB ({} extent(s))",
                    el.zone_size(ZoneType::Metadata) / 1024,
                    el.extents(ZoneType::Metadata).len(),
                );
                println!(
                    "    Blob:      {} KiB ({} extent(s))",
                    el.zone_size(ZoneType::Blob) / 1024,
                    el.extents(ZoneType::Blob).len(),
                );
                if sb.zone_map_offset != 0 {
                    println!("    Zone map:  offset {:#x}", sb.zone_map_offset);
                }
                println!("    Checkpoint: LSN {}", sb.last_checkpoint_lsn);

                total_capacity += sb.layout.device_capacity;
            }
            Err(e) => {
                eprintln!(
                    "  disk {i}: {} — not a valid Mímisbrunnr disk: {e}",
                    path.display()
                );
            }
        }
    }

    println!("{}", "-".repeat(60));
    println!(
        "  Total pool capacity: {} MiB",
        total_capacity / (1024 * 1024)
    );
}

fn cmd_add_disk(disk_path: &Path, pool_disk: &Path, size_mib: u64, tier_str: &str) {
    // Read existing pool's superblock to get node_id
    let existing = match FileBlockDevice::open(pool_disk, 0) {
        Ok(dev) => match Superblock::read_from(&dev) {
            Ok(sb) => sb,
            Err(e) => {
                eprintln!("error: cannot read pool from {}: {e}", pool_disk.display());
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("error: cannot open {}: {e}", pool_disk.display());
            std::process::exit(1);
        }
    };

    let capacity = size_mib * 1024 * 1024;
    let tier = parse_tier(tier_str);
    let disk_id = existing.disk_id + 10; // Simple ID assignment

    let dev = match FileBlockDevice::open(disk_path, capacity) {
        Ok(dev) => dev,
        Err(e) => {
            eprintln!("error: failed to open {}: {e}", disk_path.display());
            std::process::exit(1);
        }
    };

    let layout = match ZoneLayout::compute(capacity) {
        Some(l) => l,
        None => {
            eprintln!("error: disk too small");
            std::process::exit(1);
        }
    };

    let sb = Superblock::new(existing.node_id, disk_id, layout);
    if let Err(e) = sb.write_to(&dev) {
        eprintln!("error: failed to write superblock: {e}");
        std::process::exit(1);
    }

    if let Err(e) = WriteAheadLog::create(&dev, layout.wal_offset, mimisbrunnr::storage::WAL_SIZE) {
        eprintln!("error: failed to initialize WAL: {e}");
        std::process::exit(1);
    }

    println!(
        "Added disk {disk_id}: {} ({size_mib} MiB, tier: {tier})",
        disk_path.display()
    );
}

fn cmd_remove_disk(disk_path: &Path, _pool_disk: &Path) {
    println!("Marking {} for drain...", disk_path.display());
    println!("Note: In a running system, this would begin background migration.");
    println!("Disk will continue serving reads until all data is migrated.");
}

fn cmd_mount_unix(pool_path: &Path, context_name: &str, mountpoint: &Path) {
    use {
        mimisbrunnr::engine::DiskEngine,
        mimisbrunnr_fuse::{MimisbrunnrFs, TagVfs, VfsTree},
    };

    let pool_toml = if pool_path.is_dir() {
        pool_path.to_path_buf().join("pool.toml")
    } else {
        pool_path.to_path_buf()
    };

    let disk_engine = match DiskEngine::open(&pool_toml) {
        Ok(de) => de,
        Err(e) => {
            eprintln!("error: failed to open pool: {e}");
            std::process::exit(1);
        }
    };

    // Get the path context
    let projection = match disk_engine.context_mgr.get_context(context_name) {
        Ok(proj) => proj.clone(),
        Err(e) => {
            eprintln!("error: {e}");
            let contexts = disk_engine.context_mgr.list_contexts();
            if contexts.is_empty() {
                eprintln!("  No path contexts exist. Import a directory first:");
                eprintln!(
                    "    mimir --pool {} project import <dir> --context <name>",
                    pool_toml.display()
                );
            } else {
                eprintln!("  Available contexts: {}", contexts.join(", "));
            }
            std::process::exit(1);
        }
    };

    // Build TagVfs from engine's actual indexes (so /tags/ is populated)
    let engine = disk_engine.engine();
    let mut tag_vfs = TagVfs::new(
        engine.tag_index.clone(),
        engine.kv_index.clone(),
        engine.forward_index.clone(),
        engine.dag.clone(),
    );

    // Add path context with the correct name
    let tree = VfsTree::from_projection(&projection);
    tag_vfs.add_context(context_name.to_string(), tree);

    // Populate blob data for all objects
    let fs = MimisbrunnrFs::from_tag_vfs(tag_vfs);
    for entry in &projection.entries {
        if let Some(oid) = entry.object
            && let Some(blob_data) = disk_engine.get_blob(oid.raw_value())
        {
            fs.set_blob(oid.raw_value(), blob_data.to_vec());
        }
    }

    // Create mountpoint if it doesn't exist
    if !mountpoint.exists()
        && let Err(e) = std::fs::create_dir_all(mountpoint)
    {
        eprintln!("error: failed to create mountpoint: {e}");
        std::process::exit(1);
    }

    println!(
        "Mounting at {} (tag filesystem + context '{}')",
        mountpoint.display(),
        context_name,
    );
    println!(
        "  {} tag(s), {} file(s) in context projection",
        engine.dag.all_tags().len(),
        projection.files().count(),
    );
    println!("Press Ctrl+C to unmount.");

    let mut config = fuser::Config::default();
    config.mount_options = vec![
        fuser::MountOption::RO,
        fuser::MountOption::FSName("mimisbrunnr".to_string()),
        fuser::MountOption::AutoUnmount,
    ];
    config.acl = fuser::SessionACL::All;

    if let Err(e) = fuser::mount2(fs, mountpoint, &config) {
        eprintln!("error: FUSE mount failed: {e}");
        std::process::exit(1);
    }
}
