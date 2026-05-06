//! Command implementations for the `brunnr` CLI.
//!
//! Split out into its own module so the unit tests (which exercise the
//! disk-spec parser and the create / status / add-disk / remove-disk flows
//! against `tempfile::TempDir`) can call them directly without spawning a
//! subprocess.

use std::path::{Path, PathBuf};

use mimisbrunnr_engine::{DiskEngine, EngineStatus};
use mimisbrunnr_pool::{DiskConfigEntry, PoolConfig};
use mimisbrunnr_types::{DiskId, DiskState, MediaType, NodeId, StorageTier};

/// Application-level error type for the CLI. Library errors flow in via
/// `From` impls; the CLI itself only adds a small set of own variants for
/// argument-parsing problems.
#[derive(Debug, thiserror::Error)]
pub enum BrunnrError {
    #[error("engine: {0}")]
    Engine(#[from] mimisbrunnr_engine::EngineError),

    #[error("pool: {0}")]
    Pool(#[from] mimisbrunnr_pool::PoolError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid disk-spec '{spec}': {reason}")]
    InvalidDiskSpec { spec: String, reason: String },

    #[error("unknown tier '{0}' (expected hot|warm|cold|glacier)")]
    UnknownTier(String),

    #[error("unknown media '{0}' (expected nvme|ssd|hdd|smr|remote)")]
    UnknownMedia(String),

    #[error("disk '{0}' not found in pool")]
    DiskNotFound(String),

    #[cfg_attr(feature = "fuse", allow(dead_code))]
    #[error("the `fuse` feature is not enabled — rebuild with --features fuse")]
    FuseNotEnabled,

    #[error("mount failed: {0}")]
    Mount(String),
}

// ----------------------------------------------------------------------
// Disk-spec parsing.
// ----------------------------------------------------------------------

/// Parsed `<path>[:key=value]...` specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskSpec {
    pub path: PathBuf,
    pub tier: StorageTier,
    pub media: MediaType,
    /// `None` means "use existing file size" (capacity = auto).
    pub capacity: Option<u64>,
}

impl DiskSpec {
    /// Parse a single disk-spec string.
    ///
    /// Defaults (when the user omits a key):
    /// - `tier = hot`
    /// - `media = nvme`
    /// - `capacity = auto` (inherit from existing file size)
    pub fn parse(spec: &str) -> Result<Self, BrunnrError> {
        let mut parts = spec.split(':');
        let path = parts
            .next()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| BrunnrError::InvalidDiskSpec {
                spec: spec.to_string(),
                reason: "empty path".into(),
            })?;

        let mut tier = StorageTier::Hot;
        let mut media = MediaType::NVMe;
        let mut capacity: Option<u64> = None;

        for kv in parts {
            if kv.is_empty() {
                continue;
            }
            let (key, value) = kv.split_once('=').ok_or_else(|| BrunnrError::InvalidDiskSpec {
                spec: spec.to_string(),
                reason: format!("expected key=value, got '{kv}'"),
            })?;
            match key {
                "tier" => tier = parse_tier(value)?,
                "media" => media = parse_media(value)?,
                "capacity" => {
                    capacity = if value == "auto" {
                        None
                    } else {
                        Some(value.parse().map_err(|_| BrunnrError::InvalidDiskSpec {
                            spec: spec.to_string(),
                            reason: format!("capacity '{value}' is not a non-negative integer"),
                        })?)
                    };
                }
                other => {
                    return Err(BrunnrError::InvalidDiskSpec {
                        spec: spec.to_string(),
                        reason: format!("unknown key '{other}'"),
                    });
                }
            }
        }

        Ok(Self {
            path: PathBuf::from(path),
            tier,
            media,
            capacity,
        })
    }

    /// Resolve the capacity, consulting the filesystem when the spec said
    /// `capacity=auto`. Returns an error if the file does not exist and the
    /// spec did not provide an explicit byte count.
    pub fn resolved_capacity(&self) -> Result<u64, BrunnrError> {
        if let Some(c) = self.capacity {
            return Ok(c);
        }
        let meta = std::fs::metadata(&self.path).map_err(|e| BrunnrError::InvalidDiskSpec {
            spec: self.path.display().to_string(),
            reason: format!("capacity=auto requires the file to exist: {e}"),
        })?;
        Ok(meta.len())
    }
}

pub(crate) fn parse_tier(s: &str) -> Result<StorageTier, BrunnrError> {
    match s.to_ascii_lowercase().as_str() {
        "hot" => Ok(StorageTier::Hot),
        "warm" => Ok(StorageTier::Warm),
        "cold" => Ok(StorageTier::Cold),
        "glacier" => Ok(StorageTier::Glacier),
        _ => Err(BrunnrError::UnknownTier(s.to_string())),
    }
}

pub(crate) fn parse_media(s: &str) -> Result<MediaType, BrunnrError> {
    match s.to_ascii_lowercase().as_str() {
        "nvme" => Ok(MediaType::NVMe),
        "ssd" => Ok(MediaType::Ssd),
        "hdd" => Ok(MediaType::Hdd),
        "smr" | "smrhdd" | "smr-hdd" => Ok(MediaType::SmrHdd),
        "remote" => Ok(MediaType::Remote),
        _ => Err(BrunnrError::UnknownMedia(s.to_string())),
    }
}

// ----------------------------------------------------------------------
// `brunnr create`
// ----------------------------------------------------------------------

pub fn cmd_create(
    config_path: &Path,
    disk_specs: &[String],
    node_id: NodeId,
) -> Result<(), BrunnrError> {
    let mut entries: Vec<DiskConfigEntry> = Vec::with_capacity(disk_specs.len());
    for (i, raw) in disk_specs.iter().enumerate() {
        let spec = DiskSpec::parse(raw)?;
        // capacity=auto: when the file does not yet exist we default to a
        // generous 256 MiB so `brunnr create /tmp/disk0.img` Just Works on a
        // fresh system. If the caller explicitly typed `capacity=auto` for a
        // missing file, surface that as an error via `resolved_capacity`.
        let capacity = match spec.capacity {
            Some(c) => c,
            None => {
                if spec.path.exists() {
                    spec.resolved_capacity()?
                } else {
                    DEFAULT_NEW_DISK_BYTES
                }
            }
        };
        entries.push(DiskConfigEntry {
            id: i as DiskId,
            path: absolute_path(&spec.path),
            media_type: spec.media,
            tier: spec.tier,
            capacity_bytes: capacity,
        });
    }

    let config = PoolConfig {
        node_id,
        disks: entries,
    };

    let de = DiskEngine::create(config.clone(), config_path.to_path_buf())?;
    let assigned = de.config.disks.clone();
    drop(de);

    println!(
        "Pool created at {} (node_id={node_id}, {} disk(s)).",
        config_path.display(),
        assigned.len()
    );
    for d in &assigned {
        println!(
            "  disk {} → {} ({} bytes, tier={tier:?}, media={media:?})",
            d.id,
            d.path.display(),
            d.capacity_bytes,
            tier = d.tier,
            media = d.media_type,
        );
    }
    Ok(())
}

// 256 MiB — used when `capacity=auto` is requested but the backing file does
// not yet exist.
const DEFAULT_NEW_DISK_BYTES: u64 = 256 * 1024 * 1024;

// ----------------------------------------------------------------------
// `brunnr status`
// ----------------------------------------------------------------------

pub fn cmd_status(config_path: &Path) -> Result<(), BrunnrError> {
    let de = DiskEngine::open(config_path)?;
    print_status(&de.status());
    Ok(())
}

fn print_status(s: &EngineStatus) {
    println!("Mímisbrunnr Pool Status");
    println!("{}", "=".repeat(60));
    println!("  node_id:           {}", s.pool.node_id);
    println!("  disks:             {}", s.pool.disk_count);
    println!(
        "  total capacity:    {} bytes ({} MiB)",
        s.pool.total_capacity,
        s.pool.total_capacity / (1024 * 1024)
    );
    println!(
        "  total used:        {} bytes",
        s.pool.total_used
    );
    if !s.pool.faulted_disks.is_empty() {
        println!("  faulted disks:     {:?}", s.pool.faulted_disks);
    }
    println!("  objects:           {}", s.object_count);
    println!("  tags:              {}", s.tag_count);
    println!("  oplog entries:     {}", s.oplog_len);
    println!("  WAL next LSN:      {}", s.wal_next_lsn);
    println!(
        "  last checkpoint:   LSN {}",
        s.last_checkpoint_lsn
    );

    println!();
    println!("Disks:");
    println!(
        "  {:>4} {:<10} {:<10} {:<6} {:>14} {:>14}",
        "id", "state", "media", "tier", "capacity", "used"
    );
    for (id, state, media, tier, cap, used) in &s.pool.disks {
        println!(
            "  {:>4} {:<10} {:<10} {:<6} {:>14} {:>14}",
            id,
            format!("{state:?}"),
            format!("{media:?}"),
            format!("{tier:?}"),
            cap,
            used
        );
    }

    println!();
    println!("Tier breakdown:");
    for (tier, tb) in &s.pool.by_tier {
        println!(
            "  {:<8} disks={:<3} capacity={:>14}  used={:>14}",
            format!("{tier:?}"),
            tb.disk_count,
            tb.capacity_bytes,
            tb.used_bytes,
        );
    }
}

// ----------------------------------------------------------------------
// `brunnr add-disk`
// ----------------------------------------------------------------------

pub fn cmd_add_disk(
    config_path: &Path,
    path: &Path,
    tier_str: &str,
    media_str: &str,
    capacity: Option<u64>,
) -> Result<(), BrunnrError> {
    let mut de = DiskEngine::open(config_path)?;
    let next_id: DiskId = de
        .config
        .disks
        .iter()
        .map(|d| d.id)
        .max()
        .map(|m| m + 1)
        .unwrap_or(0);

    let capacity_bytes = match capacity {
        Some(c) => c,
        None => {
            if path.exists() {
                std::fs::metadata(path)?.len()
            } else {
                DEFAULT_NEW_DISK_BYTES
            }
        }
    };

    let entry = DiskConfigEntry {
        id: next_id,
        path: absolute_path(path),
        media_type: parse_media(media_str)?,
        tier: parse_tier(tier_str)?,
        capacity_bytes,
    };
    let id = de.add_disk(entry)?;
    println!(
        "Added disk {id} → {} ({} bytes).",
        path.display(),
        capacity_bytes
    );
    Ok(())
}

// ----------------------------------------------------------------------
// `brunnr remove-disk`
// ----------------------------------------------------------------------

pub fn cmd_remove_disk(config_path: &Path, target: &str) -> Result<(), BrunnrError> {
    let mut de = DiskEngine::open(config_path)?;

    let disk_id = match target.parse::<DiskId>() {
        Ok(id) if de.config.find_disk(id).is_some() => id,
        _ => {
            // Fall back to a path match.
            let target_path = absolute_path(Path::new(target));
            de.config
                .disks
                .iter()
                .find(|d| d.path == target_path)
                .map(|d| d.id)
                .ok_or_else(|| BrunnrError::DiskNotFound(target.to_string()))?
        }
    };

    de.remove_disk(disk_id)?;
    println!(
        "Disk {disk_id} marked Draining. Data is not yet evacuated \
         (TODO: phase-N evacuation per DESIGN §8.4).",
    );
    // Sanity-check the runtime state we just wrote.
    let s = de.status();
    if let Some((_, state, _, _, _, _)) = s.pool.disks.iter().find(|(id, ..)| *id == disk_id)
        && *state == DiskState::Draining
    {
        println!("Confirmed: disk {disk_id} is now Draining.");
    }
    Ok(())
}

// ----------------------------------------------------------------------
// `brunnr mount` / `mount-unix`
// ----------------------------------------------------------------------

#[cfg(feature = "fuse")]
pub fn cmd_mount(
    config_path: &Path,
    mountpoint: &Path,
    context: Option<&str>,
) -> Result<(), BrunnrError> {
    use mimisbrunnr_fuse::{MimisbrunnrFs, TagVfs};

    if !mountpoint.exists() {
        std::fs::create_dir_all(mountpoint)?;
    }

    let de = DiskEngine::open(config_path)?;

    // The `fuser::Filesystem` trait is `: 'static`; `TagVfs<'a>` borrows from
    // the engine's mirrors. We resolve the lifetime by leaking the engine so
    // that any reference into it has a `'static` lifetime for the duration of
    // the mount session. The mount blocks until unmounted, after which the
    // process exits and the OS reclaims the allocation. This is the simplest
    // sound option; the alternative (parking everything in `Arc`s and adding
    // an `Arc`-friendly TagVfs constructor) is a TODO for a later phase.
    let leaked: &'static DiskEngine = Box::leak(Box::new(de));

    let tag_vfs: TagVfs<'static> = TagVfs::new(
        &leaked.engine.tag_index,
        &leaked.engine.kv_index,
        &leaked.engine.forward_index,
        &leaked.engine.ontology,
        &leaked.engine.path_contexts,
    );

    if let Some(ctx) = context {
        // The 5b `MimisbrunnrFs` always exposes both `/tags/` and `/ctx/`.
        // For `mount-unix` we still mount the full tree; the requested
        // context shows up under `<mountpoint>/ctx/<context>`.
        // TODO(rewrite-phase-N): implement subtree-only mount that re-roots
        // at the named path-context.
        if leaked
            .engine
            .ontology
            .names
            .get(ctx)
            .and_then(|tid| leaked.engine.path_contexts.projections.get(tid))
            .is_none()
        {
            return Err(BrunnrError::Mount(format!(
                "context '{ctx}' is not registered in this pool"
            )));
        }
        println!(
            "Mounting full filesystem at {} — your context is at {}/ctx/{ctx}",
            mountpoint.display(),
            mountpoint.display()
        );
    } else {
        println!("Mounting filesystem at {}", mountpoint.display());
    }

    // The Phase 6 `DiskEngine` keeps blobs in an in-memory HashMap keyed by
    // `oid_local`. Wrap that lookup in a closure the FUSE adapter can call.
    let content: Box<
        dyn Fn(mimisbrunnr_types::ObjectId) -> Option<Vec<u8>> + Send + Sync + 'static,
    > = Box::new(move |oid: mimisbrunnr_types::ObjectId| {
        leaked.blobs.get(&oid.local_seq()).cloned()
    });
    let fs: MimisbrunnrFs<'static> = MimisbrunnrFs::new(tag_vfs, content);

    let mount_config = fuser::Config::default();
    fuser::mount2(fs, mountpoint, &mount_config)
        .map_err(|e| BrunnrError::Mount(e.to_string()))?;
    Ok(())
}

#[cfg(not(feature = "fuse"))]
pub fn cmd_mount(
    _config_path: &Path,
    _mountpoint: &Path,
    _context: Option<&str>,
) -> Result<(), BrunnrError> {
    Err(BrunnrError::FuseNotEnabled)
}

// ----------------------------------------------------------------------
// `brunnr unmount`
// ----------------------------------------------------------------------

pub fn cmd_unmount(mountpoint: &Path) -> Result<(), BrunnrError> {
    use std::process::Command;

    let mp = mountpoint.to_string_lossy().to_string();
    let result = if cfg!(target_os = "macos") {
        Command::new("diskutil").args(["unmount", &mp]).status()
    } else if cfg!(target_os = "linux") {
        Command::new("fusermount").args(["-u", &mp]).status()
    } else {
        Command::new("umount").arg(&mp).status()
    };

    match result {
        Ok(status) if status.success() => {
            println!("Unmounted {mp}.");
            Ok(())
        }
        Ok(status) => Err(BrunnrError::Mount(format!(
            "unmount of {mp} exited with status {status}"
        ))),
        Err(e) => Err(BrunnrError::Mount(format!("failed to spawn unmount: {e}"))),
    }
}

// ----------------------------------------------------------------------
// Helpers.
// ----------------------------------------------------------------------

fn absolute_path(p: &Path) -> PathBuf {
    if p.is_absolute() {
        return p.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(p),
        Err(_) => p.to_path_buf(),
    }
}

// ----------------------------------------------------------------------
// Tests.
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::TempDir;

    fn create_empty_file(path: &Path, size_bytes: u64) {
        let f = File::create(path).unwrap();
        f.set_len(size_bytes).unwrap();
    }

    // ----- Disk-spec parser -----

    #[test]
    fn disk_spec_path_only_uses_defaults() {
        let s = DiskSpec::parse("/tmp/x").unwrap();
        assert_eq!(s.path, PathBuf::from("/tmp/x"));
        assert_eq!(s.tier, StorageTier::Hot);
        assert_eq!(s.media, MediaType::NVMe);
        assert_eq!(s.capacity, None);
    }

    #[test]
    fn disk_spec_full_form_parses() {
        let s = DiskSpec::parse("/tmp/x:tier=cold:media=hdd:capacity=1073741824").unwrap();
        assert_eq!(s.path, PathBuf::from("/tmp/x"));
        assert_eq!(s.tier, StorageTier::Cold);
        assert_eq!(s.media, MediaType::Hdd);
        assert_eq!(s.capacity, Some(1_073_741_824));
    }

    #[test]
    fn disk_spec_capacity_auto_means_inherit() {
        let s = DiskSpec::parse("/tmp/x:capacity=auto").unwrap();
        assert_eq!(s.capacity, None);
    }

    #[test]
    fn disk_spec_unknown_key_rejected() {
        let err = DiskSpec::parse("/tmp/x:foo=bar").unwrap_err();
        match err {
            BrunnrError::InvalidDiskSpec { reason, .. } => {
                assert!(reason.contains("unknown key"));
            }
            other => panic!("expected InvalidDiskSpec, got {other:?}"),
        }
    }

    #[test]
    fn disk_spec_bad_tier_rejected() {
        let err = DiskSpec::parse("/tmp/x:tier=lukewarm").unwrap_err();
        assert!(matches!(err, BrunnrError::UnknownTier(_)));
    }

    #[test]
    fn disk_spec_bad_media_rejected() {
        let err = DiskSpec::parse("/tmp/x:media=floppy").unwrap_err();
        assert!(matches!(err, BrunnrError::UnknownMedia(_)));
    }

    #[test]
    fn disk_spec_no_value_rejected() {
        let err = DiskSpec::parse("/tmp/x:tier").unwrap_err();
        assert!(matches!(err, BrunnrError::InvalidDiskSpec { .. }));
    }

    #[test]
    fn disk_spec_resolved_capacity_uses_file_size() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("d.img");
        create_empty_file(&p, 4 * 1024 * 1024);
        let s = DiskSpec::parse(&format!("{}:capacity=auto", p.display())).unwrap();
        assert_eq!(s.resolved_capacity().unwrap(), 4 * 1024 * 1024);
    }

    // ----- create / status -----

    #[test]
    fn create_two_disk_pool_round_trip() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("pool.toml");
        let p0 = tmp.path().join("d0.img");
        let p1 = tmp.path().join("d1.img");
        create_empty_file(&p0, 32 * 1024 * 1024);
        create_empty_file(&p1, 32 * 1024 * 1024);

        let specs = vec![
            format!("{}:tier=hot:media=nvme:capacity=auto", p0.display()),
            format!("{}:tier=cold:media=hdd:capacity=auto", p1.display()),
        ];
        cmd_create(&cfg, &specs, 7).unwrap();

        // pool.toml exists.
        assert!(cfg.exists());
        // both disk files exist with the requested size.
        assert!(p0.exists() && p1.exists());

        // re-open succeeds and reports two disks.
        let de = DiskEngine::open(&cfg).unwrap();
        let s = de.status();
        assert_eq!(s.pool.node_id, 7);
        assert_eq!(s.pool.disk_count, 2);
    }

    #[test]
    fn status_reports_disk_counts_and_tier_breakdown() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("pool.toml");
        let p0 = tmp.path().join("d0.img");
        let p1 = tmp.path().join("d1.img");
        create_empty_file(&p0, 32 * 1024 * 1024);
        create_empty_file(&p1, 32 * 1024 * 1024);

        let specs = vec![
            format!("{}:tier=hot:media=nvme:capacity=auto", p0.display()),
            format!("{}:tier=cold:media=hdd:capacity=auto", p1.display()),
        ];
        cmd_create(&cfg, &specs, 1).unwrap();

        let de = DiskEngine::open(&cfg).unwrap();
        let s = de.status();
        assert_eq!(s.pool.disk_count, 2);
        assert_eq!(
            s.pool.by_tier.get(&StorageTier::Hot).unwrap().disk_count,
            1
        );
        assert_eq!(
            s.pool.by_tier.get(&StorageTier::Cold).unwrap().disk_count,
            1
        );
    }

    // ----- add-disk / remove-disk -----

    #[test]
    fn add_disk_increments_count_and_persists_config() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("pool.toml");
        let p0 = tmp.path().join("d0.img");
        create_empty_file(&p0, 32 * 1024 * 1024);
        cmd_create(
            &cfg,
            &[format!("{}:capacity=auto", p0.display())],
            0,
        )
        .unwrap();

        let p1 = tmp.path().join("d1.img");
        create_empty_file(&p1, 32 * 1024 * 1024);
        cmd_add_disk(&cfg, &p1, "warm", "ssd", None).unwrap();

        // pool.toml updated.
        let cfg_data = PoolConfig::load_toml(&cfg).unwrap();
        assert_eq!(cfg_data.disks.len(), 2);
        assert_eq!(cfg_data.disks[1].tier, StorageTier::Warm);
        assert_eq!(cfg_data.disks[1].media_type, MediaType::Ssd);

        // re-open sees both disks.
        let de = DiskEngine::open(&cfg).unwrap();
        assert_eq!(de.status().pool.disk_count, 2);
    }

    #[test]
    fn remove_disk_marks_draining() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("pool.toml");
        let p0 = tmp.path().join("d0.img");
        let p1 = tmp.path().join("d1.img");
        create_empty_file(&p0, 32 * 1024 * 1024);
        create_empty_file(&p1, 32 * 1024 * 1024);
        cmd_create(
            &cfg,
            &[
                format!("{}:capacity=auto", p0.display()),
                format!("{}:capacity=auto", p1.display()),
            ],
            0,
        )
        .unwrap();

        cmd_remove_disk(&cfg, "1").unwrap();

        let de = DiskEngine::open(&cfg).unwrap();
        let s = de.status();
        let (_, state, _, _, _, _) = s.pool.disks.iter().find(|(id, ..)| *id == 1).unwrap();
        assert_eq!(*state, DiskState::Draining);
    }

    #[test]
    fn remove_disk_by_path() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("pool.toml");
        let p0 = tmp.path().join("d0.img");
        let p1 = tmp.path().join("d1.img");
        create_empty_file(&p0, 32 * 1024 * 1024);
        create_empty_file(&p1, 32 * 1024 * 1024);
        cmd_create(
            &cfg,
            &[
                format!("{}:capacity=auto", p0.display()),
                format!("{}:capacity=auto", p1.display()),
            ],
            0,
        )
        .unwrap();

        cmd_remove_disk(&cfg, p1.to_str().unwrap()).unwrap();

        let de = DiskEngine::open(&cfg).unwrap();
        let s = de.status();
        let (_, state, _, _, _, _) = s.pool.disks.iter().find(|(id, ..)| *id == 1).unwrap();
        assert_eq!(*state, DiskState::Draining);
    }

    #[test]
    fn remove_disk_unknown_target_errors() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("pool.toml");
        let p0 = tmp.path().join("d0.img");
        create_empty_file(&p0, 32 * 1024 * 1024);
        cmd_create(
            &cfg,
            &[format!("{}:capacity=auto", p0.display())],
            0,
        )
        .unwrap();

        let err = cmd_remove_disk(&cfg, "/no/such/path").unwrap_err();
        match err {
            BrunnrError::DiskNotFound(_) | BrunnrError::Pool(_) => {}
            other => panic!("expected DiskNotFound/Pool error, got {other:?}"),
        }
    }

    // ----- mount --
    //
    // Mount tests must NOT actually mount (they would require root / FUSE
    // permissions). We exercise the steps up to but not including
    // `fuser::mount2` only when the `fuse` feature is on.

    #[cfg(feature = "fuse")]
    #[test]
    fn fuse_filesystem_constructible_from_disk_engine() {
        use mimisbrunnr_fuse::{MimisbrunnrFs, TagVfs};

        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("pool.toml");
        let p0 = tmp.path().join("d0.img");
        create_empty_file(&p0, 32 * 1024 * 1024);
        cmd_create(
            &cfg,
            &[format!("{}:capacity=auto", p0.display())],
            0,
        )
        .unwrap();

        let de = DiskEngine::open(&cfg).unwrap();
        // We don't leak in tests — just take borrowed references with a
        // short-lived 'a lifetime; that's enough to prove the FUSE
        // filesystem can be assembled from the engine without going through
        // `fuser::mount2`.
        let tag_vfs = TagVfs::new(
            &de.engine.tag_index,
            &de.engine.kv_index,
            &de.engine.forward_index,
            &de.engine.ontology,
            &de.engine.path_contexts,
        );
        let provider: Box<
            dyn Fn(mimisbrunnr_types::ObjectId) -> Option<Vec<u8>> + Send + Sync,
        > = Box::new(|_| None);
        let _fs = MimisbrunnrFs::new(tag_vfs, provider);
    }

    #[cfg(not(feature = "fuse"))]
    #[test]
    fn mount_without_fuse_feature_errors() {
        let tmp = TempDir::new().unwrap();
        let err = cmd_mount(tmp.path(), tmp.path(), None).unwrap_err();
        assert!(matches!(err, BrunnrError::FuseNotEnabled));
    }
}
