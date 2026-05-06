//! Live `PoolManager` — orchestrates a multi-disk pool's lifecycle.
//!
//! Owns:
//!
//! - Open file-backed devices for every disk in the [`crate::PoolConfig`].
//! - Format / validate per-disk [`Superblock`]s.
//! - Read / write the primary disk's [`PoolStateRoot`] (the on-disk
//!   authority for inline disk descriptors).
//! - The in-memory placement-rule list (persisted as CBOR until the
//!   `BtreeKind::PlacementRules` B+ tree lands).
//!
//! The full placement-rule evaluator and rebalancing/drain machinery from
//! DESIGN §8.4 are tagged as TODO for later phases — this module gives the
//! **disk-level** lifecycle (create / open / add / remove / status / pick).

use std::{collections::BTreeMap, path::Path, sync::Arc};

use {
    log::{trace, warn},
    mimisbrunnr_storage::{
        BLOCK_SIZE, BlockDevice, BucketDataType, FileBlockDevice, Superblock, ZoneExtent,
    },
    mimisbrunnr_types::{DiskId, DiskState, MediaType, NodeId, PlacementRule, StorageTier},
};

use crate::{
    config::{DiskConfigEntry, PoolConfig},
    disk::{DISK_PATH_INLINE_LEN, DiskDescriptorOnDisk},
    error::PoolError,
    pool::{POOL_STATE_ROOT_INLINE_DISKS, PoolStateRoot},
};

/// Fixed byte offset of the `PoolStateRoot` block on the primary disk.
///
/// The first two 4 KiB blocks hold the leading Superblock copies (offsets
/// 0 and 4 KiB). We keep a small reserved gap and place the `PoolStateRoot`
/// at offset `4 * BLOCK_SIZE` (16 KiB). Once the metadata zone is wired
/// through the engine, this becomes a `BlockRef` recorded in the active
/// `RootPointer.pool_state_root` (IMPL §2.2). For Phase 3c the offset is
/// fixed.
//
// TODO(rewrite-phase-N): wire pool_state_root via RootPointer.pool_state_root
// instead of a hard-coded byte offset.
pub const POOL_STATE_ROOT_OFFSET: u64 = (BLOCK_SIZE as u64) * 4;

/// Format-time defaults shared across freshly-created disks.
const FMT_BUCKET_SIZE_LOG2: u8 = 20; // 1 MiB buckets
const FMT_BTREE_NODE_LOG2: u8 = 18; // 256 KiB nodes
const FMT_BOOTSTRAP_BUCKETS: u32 = 64;
const FMT_WAL_OFFSET: u64 = 64 * 1024; // 64 KiB
const FMT_WAL_SIZE: u64 = 1024 * 1024; // 1 MiB
const FMT_INDEX_ZONE_OFFSET: u64 = FMT_WAL_OFFSET + FMT_WAL_SIZE; // ~1.0625 MiB
const FMT_INDEX_ZONE_SIZE: u64 = 4 * 1024 * 1024;
const FMT_METADATA_ZONE_OFFSET: u64 = FMT_INDEX_ZONE_OFFSET + FMT_INDEX_ZONE_SIZE;
const FMT_METADATA_ZONE_SIZE: u64 = 4 * 1024 * 1024;
const FMT_BLOB_ZONE_OFFSET: u64 = FMT_METADATA_ZONE_OFFSET + FMT_METADATA_ZONE_SIZE;
const FMT_FORMAT_VERSION: u16 = 1;

/// Live runtime view of one disk in the pool.
pub struct DiskRuntime {
    pub config: DiskConfigEntry,
    pub state: DiskState,
    pub used_bytes: u64,
    pub device: Arc<FileBlockDevice>,
    /// Cached active superblock copy, populated when the runtime is opened
    /// or freshly formatted. Used by inspectors (e.g. `analyze`) to render
    /// per-disk header info without re-reading the device.
    pub superblock: Superblock,
}

impl std::fmt::Debug for DiskRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskRuntime")
            .field("config", &self.config)
            .field("state", &self.state)
            .field("used_bytes", &self.used_bytes)
            .finish_non_exhaustive()
    }
}

/// Per-tier capacity / used totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TierBreakdown {
    pub disk_count: u32,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
}

/// Capacity / health snapshot of the pool.
#[derive(Debug, Clone)]
pub struct PoolStatus {
    pub node_id: NodeId,
    pub disk_count: u32,
    pub total_capacity: u64,
    pub total_used: u64,
    pub disks: Vec<(DiskId, DiskState, MediaType, StorageTier, u64, u64)>,
    pub by_tier: BTreeMap<StorageTier, TierBreakdown>,
    pub faulted_disks: Vec<DiskId>,
}

/// Live pool orchestrator.
#[derive(Debug)]
pub struct PoolManager {
    config: PoolConfig,
    disks: BTreeMap<DiskId, DiskRuntime>,
    placement_rules: Vec<PlacementRule>,
}

impl PoolManager {
    // --------------------------------------------------------------
    // Construction
    // --------------------------------------------------------------

    /// Create a fresh pool from a freshly-built [`PoolConfig`].
    ///
    /// For each disk:
    ///
    /// 1. Open / extend the file-backed device.
    /// 2. Write the 3-copy [`Superblock`] (inheriting `node_id` and the
    ///    listed `disk_id`).
    ///
    /// On the primary disk a fresh [`PoolStateRoot`] is initialised with
    /// every disk's descriptor and written to
    /// [`POOL_STATE_ROOT_OFFSET`].
    pub fn create(config: PoolConfig) -> Result<Self, PoolError> {
        if config.disks.is_empty() {
            return Err(PoolError::EmptyPool);
        }
        if config.disks.len() > POOL_STATE_ROOT_INLINE_DISKS {
            return Err(PoolError::DiskOverflowUnsupported {
                count: config.disks.len() as u32,
                max: POOL_STATE_ROOT_INLINE_DISKS as u32,
            });
        }

        // Reject duplicate disk ids up-front.
        let mut seen: BTreeMap<DiskId, ()> = BTreeMap::new();
        for entry in &config.disks {
            if seen.insert(entry.id, ()).is_some() {
                return Err(PoolError::DuplicateDiskId(entry.id));
            }
        }

        let mut disks: BTreeMap<DiskId, DiskRuntime> = BTreeMap::new();
        for entry in &config.disks {
            let (dev, sb) = Self::open_or_format_disk(entry, config.node_id, true)?;
            disks.insert(
                entry.id,
                DiskRuntime {
                    config: entry.clone(),
                    state: DiskState::Online,
                    used_bytes: 0,
                    device: dev,
                    superblock: sb,
                },
            );
        }

        let mut mgr = Self {
            config,
            disks,
            placement_rules: Vec::new(),
        };
        mgr.write_pool_state_root()?;
        Ok(mgr)
    }

    /// Open an existing pool described by `config`. Each listed disk is
    /// expected to already contain a valid [`Superblock`] tagged with the
    /// configured `node_id` and `disk_id`.
    pub fn open(config: PoolConfig) -> Result<Self, PoolError> {
        if config.disks.is_empty() {
            return Err(PoolError::EmptyPool);
        }

        let mut disks: BTreeMap<DiskId, DiskRuntime> = BTreeMap::new();
        for entry in &config.disks {
            let (dev, sb) = Self::open_or_format_disk(entry, config.node_id, false)?;
            disks.insert(
                entry.id,
                DiskRuntime {
                    config: entry.clone(),
                    state: DiskState::Online,
                    used_bytes: 0,
                    device: dev,
                    superblock: sb,
                },
            );
        }

        let primary_id = config
            .primary()
            .ok_or(PoolError::EmptyPool)?
            .id;
        let primary_dev = disks
            .get(&primary_id)
            .ok_or(PoolError::DiskNotFound(primary_id))?
            .device
            .clone();

        let psr = PoolStateRoot::read(primary_dev.as_ref(), POOL_STATE_ROOT_OFFSET)?;
        let mut mgr = Self {
            config,
            disks,
            placement_rules: Vec::new(),
        };
        mgr.apply_pool_state_root(&psr)?;
        Ok(mgr)
    }

    fn open_or_format_disk(
        entry: &DiskConfigEntry,
        node_id: NodeId,
        format: bool,
    ) -> Result<(Arc<FileBlockDevice>, Superblock), PoolError> {
        trace!(
            "open_or_format_disk id={} path={} format={}",
            entry.id,
            entry.path.display(),
            format
        );
        let dev = if format {
            FileBlockDevice::open(&entry.path, entry.capacity_bytes)?
        } else {
            FileBlockDevice::open(&entry.path, 0)?
        };
        if format {
            Superblock::format(
                &dev,
                fs_uuid_for(node_id, entry.id),
                node_id,
                entry.id,
                entry.media_type,
                entry.tier,
                FMT_BUCKET_SIZE_LOG2,
                FMT_BTREE_NODE_LOG2,
                FMT_BOOTSTRAP_BUCKETS,
                FMT_WAL_OFFSET,
                FMT_WAL_SIZE,
                ZoneExtent {
                    offset: FMT_INDEX_ZONE_OFFSET,
                    length: FMT_INDEX_ZONE_SIZE,
                    flags: 0,
                    _pad: 0,
                },
                ZoneExtent {
                    offset: FMT_METADATA_ZONE_OFFSET,
                    length: FMT_METADATA_ZONE_SIZE,
                    flags: 0,
                    _pad: 0,
                },
                ZoneExtent {
                    offset: FMT_BLOB_ZONE_OFFSET,
                    length: entry.capacity_bytes.saturating_sub(FMT_BLOB_ZONE_OFFSET),
                    flags: 0,
                    _pad: 0,
                },
                FMT_FORMAT_VERSION,
            )?;
        }
        // Read the active superblock copy back (whether we just formatted or
        // are re-opening). On the re-open path also validate node/disk IDs.
        let sb = Superblock::open(&dev)?;
        if !format {
            let sb_node = sb.node_id();
            if sb_node != node_id {
                return Err(PoolError::NodeIdMismatch {
                    disk: entry.id,
                    pool: node_id,
                    disk_says: sb_node,
                });
            }
            let sb_disk = sb.disk_id();
            if sb_disk != entry.id {
                return Err(PoolError::DiskIdMismatch {
                    expected: entry.id,
                    actual: sb_disk,
                });
            }
        }
        Ok((Arc::new(dev), sb))
    }

    // --------------------------------------------------------------
    // Disk lifecycle
    // --------------------------------------------------------------

    /// Add a disk to the pool. Formats its superblock, registers it in
    /// the [`PoolStateRoot`], persists the updated root.
    pub fn add_disk(&mut self, entry: DiskConfigEntry) -> Result<DiskId, PoolError> {
        if self.disks.contains_key(&entry.id) {
            return Err(PoolError::DuplicateDiskId(entry.id));
        }
        if self.disks.len() + 1 > POOL_STATE_ROOT_INLINE_DISKS {
            return Err(PoolError::DiskOverflowUnsupported {
                count: (self.disks.len() + 1) as u32,
                max: POOL_STATE_ROOT_INLINE_DISKS as u32,
            });
        }
        let (dev, sb) = Self::open_or_format_disk(&entry, self.config.node_id, true)?;
        let id = entry.id;
        self.config.disks.push(entry.clone());
        self.disks.insert(
            id,
            DiskRuntime {
                config: entry,
                state: DiskState::Online,
                used_bytes: 0,
                device: dev,
                superblock: sb,
            },
        );
        self.write_pool_state_root()?;
        Ok(id)
    }

    /// Begin removing a disk: marks the runtime state as `Draining` and
    /// persists the updated `PoolStateRoot`.
    ///
    /// **Phase 3c scope:** no actual data evacuation. Subsequent phases
    /// will scan the location table and migrate matching extents.
    //
    // TODO(rewrite-phase-N): evacuate per DESIGN §8.4 (location-table scan,
    // placement-rule re-evaluation, migration, then `state = Removed`).
    pub fn remove_disk(&mut self, disk_id: DiskId) -> Result<(), PoolError> {
        let runtime = self
            .disks
            .get_mut(&disk_id)
            .ok_or(PoolError::DiskNotFound(disk_id))?;
        runtime.state = DiskState::Draining;
        warn!(
            "PoolManager::remove_disk(id={disk_id}) — Phase 3c marks Draining \
             but does not migrate data"
        );
        self.write_pool_state_root()?;
        Ok(())
    }

    // --------------------------------------------------------------
    // Status / queries
    // --------------------------------------------------------------

    /// Snapshot of pool capacity / health.
    pub fn status(&self) -> PoolStatus {
        let mut disks = Vec::with_capacity(self.disks.len());
        let mut by_tier: BTreeMap<StorageTier, TierBreakdown> = BTreeMap::new();
        let mut faulted = Vec::new();
        let mut total_capacity = 0u64;
        let mut total_used = 0u64;
        for (id, rt) in &self.disks {
            disks.push((
                *id,
                rt.state,
                rt.config.media_type,
                rt.config.tier,
                rt.config.capacity_bytes,
                rt.used_bytes,
            ));
            total_capacity = total_capacity.saturating_add(rt.config.capacity_bytes);
            total_used = total_used.saturating_add(rt.used_bytes);
            let tb = by_tier.entry(rt.config.tier).or_default();
            tb.disk_count += 1;
            tb.capacity_bytes = tb.capacity_bytes.saturating_add(rt.config.capacity_bytes);
            tb.used_bytes = tb.used_bytes.saturating_add(rt.used_bytes);
            if rt.state == DiskState::Faulted {
                faulted.push(*id);
            }
        }
        PoolStatus {
            node_id: self.config.node_id,
            disk_count: self.disks.len() as u32,
            total_capacity,
            total_used,
            disks,
            by_tier,
            faulted_disks: faulted,
        }
    }

    /// All registered placement rules.
    pub fn placement_rules(&self) -> &[PlacementRule] {
        &self.placement_rules
    }

    /// Append a placement rule.
    pub fn add_placement_rule(&mut self, rule: PlacementRule) -> Result<(), PoolError> {
        self.placement_rules.push(rule);
        Ok(())
    }

    /// Pool's TOML-serialisable config view.
    pub fn config(&self) -> &PoolConfig {
        &self.config
    }

    /// Borrow the runtime view of a disk.
    pub fn runtime(&self, disk_id: DiskId) -> Option<&DiskRuntime> {
        self.disks.get(&disk_id)
    }

    /// Borrow the cached superblock for `disk_id`. Returns `None` for an
    /// unknown disk. The cached copy reflects the active superblock as of
    /// pool open / disk add.
    pub fn disk_superblock(&self, disk_id: DiskId) -> Option<&Superblock> {
        self.disks.get(&disk_id).map(|rt| &rt.superblock)
    }

    /// Pick a disk for new data of `data_type` in tier `tier_pref`.
    ///
    /// Phase 3c strategy: simple capacity-weighted choice among Online
    /// disks in the requested tier — pick the one with the most free
    /// capacity. Returns `None` if no disk in that tier is online.
    //
    // TODO(rewrite-phase-N): full placement-rule evaluator per DESIGN
    // §8.3 (Pin > Prefer > Replicate > AutoTier > defaults).
    pub fn pick_disk(
        &self,
        tier_pref: StorageTier,
        _data_type: BucketDataType,
    ) -> Option<DiskId> {
        let mut best: Option<(DiskId, u64)> = None;
        for (id, rt) in &self.disks {
            if rt.state != DiskState::Online {
                continue;
            }
            if rt.config.tier != tier_pref {
                continue;
            }
            let free = rt.config.capacity_bytes.saturating_sub(rt.used_bytes);
            match best {
                Some((_, best_free)) if free <= best_free => {}
                _ => best = Some((*id, free)),
            }
        }
        best.map(|(id, _)| id)
    }

    // --------------------------------------------------------------
    // Persistence
    // --------------------------------------------------------------

    /// Serialise the current state to a fresh [`PoolStateRoot`] and write
    /// it on the primary disk.
    pub fn commit(&mut self) -> Result<(), PoolError> {
        self.write_pool_state_root()
    }

    /// Persist the manager's authoritative [`PoolConfig`] to `path` as TOML.
    ///
    /// `add_disk` and `remove_disk` already update the manager's internal
    /// `PoolConfig`; this is the single helper callers (binaries, the engine
    /// `DiskEngine`) reach for after a mutation. Replaces the older
    /// re-read-then-save dance.
    pub fn save_config(&self, path: &Path) -> Result<(), PoolError> {
        self.config.save_toml(path)
    }

    fn write_pool_state_root(&mut self) -> Result<(), PoolError> {
        let primary_id = self
            .config
            .primary()
            .ok_or(PoolError::EmptyPool)?
            .id;

        let mut psr = PoolStateRoot::new_blank();
        psr.cluster_node_count = 1; // single-node clusters in this phase
        let mut wire: Vec<DiskDescriptorOnDisk> = Vec::with_capacity(self.disks.len());
        for rt in self.disks.values() {
            let path_str = rt.config.path.to_string_lossy();
            if path_str.len() > DISK_PATH_INLINE_LEN {
                return Err(PoolError::DiskPathTooLong {
                    len: path_str.len(),
                    max: DISK_PATH_INLINE_LEN,
                });
            }
            let mut d = DiskDescriptorOnDisk::new(
                rt.config.id,
                rt.config.media_type,
                rt.config.tier,
                rt.state,
                rt.config.capacity_bytes,
                &path_str,
            )?;
            d.used_bytes = rt.used_bytes;
            wire.push(d);
        }
        psr.set_inline_disks(&wire)?;

        let primary_dev = self
            .disks
            .get(&primary_id)
            .ok_or(PoolError::DiskNotFound(primary_id))?
            .device
            .clone();
        psr.write(primary_dev.as_ref(), POOL_STATE_ROOT_OFFSET)?;
        primary_dev.sync()?;
        Ok(())
    }

    fn apply_pool_state_root(&mut self, psr: &PoolStateRoot) -> Result<(), PoolError> {
        for d in psr.inline_iter() {
            let id = d.disk_id_typed();
            let state = d.state_typed()?;
            let used = { d.used_bytes };
            if let Some(rt) = self.disks.get_mut(&id) {
                rt.state = state;
                rt.used_bytes = used;
            }
        }
        Ok(())
    }
}

/// Derive a stable 16-byte fs UUID from `(node_id, disk_id)`. Real
/// deployments would use a random UUID at format time and persist it; for
/// Phase 3c this deterministic derivation is sufficient and re-formatting
/// the same disk produces the same UUID.
fn fs_uuid_for(node_id: NodeId, disk_id: DiskId) -> [u8; 16] {
    let mut u = [0u8; 16];
    u[..2].copy_from_slice(&node_id.to_le_bytes());
    u[2..4].copy_from_slice(&disk_id.to_le_bytes());
    u[4..8].copy_from_slice(b"MIMI");
    u[8..12].copy_from_slice(b"POOL");
    u
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::path::PathBuf,
        tempfile::TempDir,
    };

    fn entry(id: DiskId, path: PathBuf, tier: StorageTier, media: MediaType, cap: u64) -> DiskConfigEntry {
        DiskConfigEntry {
            id,
            path,
            media_type: media,
            tier,
            capacity_bytes: cap,
        }
    }

    fn small_pool(tmp: &TempDir) -> PoolConfig {
        let p0 = tmp.path().join("disk0.img");
        let p1 = tmp.path().join("disk1.img");
        PoolConfig {
            node_id: 1,
            disks: vec![
                entry(0, p0, StorageTier::Hot, MediaType::NVMe, 32 * 1024 * 1024),
                entry(1, p1, StorageTier::Cold, MediaType::Hdd, 32 * 1024 * 1024),
            ],
        }
    }

    #[test]
    fn create_then_open_round_trip() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mgr = PoolManager::create(cfg.clone()).unwrap();
        let s = mgr.status();
        assert_eq!(s.node_id, 1);
        assert_eq!(s.disk_count, 2);
        drop(mgr);

        let opened = PoolManager::open(cfg).unwrap();
        let s = opened.status();
        assert_eq!(s.disk_count, 2);
        // both disks should be Online after re-open.
        for (_, state, _, _, _, _) in &s.disks {
            assert_eq!(*state, DiskState::Online);
        }
    }

    #[test]
    fn empty_config_rejected() {
        let cfg = PoolConfig::new(1);
        assert!(matches!(
            PoolManager::create(cfg),
            Err(PoolError::EmptyPool)
        ));
    }

    #[test]
    fn duplicate_disk_id_rejected() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("dupe.img");
        let cfg = PoolConfig {
            node_id: 1,
            disks: vec![
                entry(0, p.clone(), StorageTier::Hot, MediaType::Ssd, 8 * 1024 * 1024),
                entry(0, p, StorageTier::Hot, MediaType::Ssd, 8 * 1024 * 1024),
            ],
        };
        assert!(matches!(
            PoolManager::create(cfg),
            Err(PoolError::DuplicateDiskId(0))
        ));
    }

    #[test]
    fn add_disk_persists() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mut mgr = PoolManager::create(cfg.clone()).unwrap();
        let new_path = tmp.path().join("disk2.img");
        let new_entry = entry(2, new_path, StorageTier::Warm, MediaType::Ssd, 16 * 1024 * 1024);
        mgr.add_disk(new_entry).unwrap();
        assert_eq!(mgr.status().disk_count, 3);

        // Reopen and confirm the new disk is recorded.
        let opened_cfg = mgr.config().clone();
        drop(mgr);
        let opened = PoolManager::open(opened_cfg).unwrap();
        assert_eq!(opened.status().disk_count, 3);
        assert!(opened.runtime(2).is_some());
    }

    #[test]
    fn remove_disk_marks_draining() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mut mgr = PoolManager::create(cfg).unwrap();
        mgr.remove_disk(1).unwrap();
        let s = mgr.status();
        let (_, state, _, _, _, _) = s.disks.iter().find(|(id, ..)| *id == 1).unwrap();
        assert_eq!(*state, DiskState::Draining);
    }

    #[test]
    fn remove_unknown_disk_errors() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mut mgr = PoolManager::create(cfg).unwrap();
        let err = mgr.remove_disk(99).unwrap_err();
        assert!(matches!(err, PoolError::DiskNotFound(99)));
    }

    #[test]
    fn pick_disk_respects_tier() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mgr = PoolManager::create(cfg).unwrap();
        // disk 0 is Hot; disk 1 is Cold.
        let hot = mgr.pick_disk(StorageTier::Hot, BucketDataType::Blob);
        let cold = mgr.pick_disk(StorageTier::Cold, BucketDataType::Blob);
        assert_eq!(hot, Some(0));
        assert_eq!(cold, Some(1));
        // Warm has no disk in this pool.
        assert_eq!(mgr.pick_disk(StorageTier::Warm, BucketDataType::Blob), None);
    }

    #[test]
    fn pick_disk_skips_draining() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mut mgr = PoolManager::create(cfg).unwrap();
        // Draining disk 0 (Hot) — pick_disk(Hot) should return None.
        mgr.remove_disk(0).unwrap();
        assert_eq!(mgr.pick_disk(StorageTier::Hot, BucketDataType::Blob), None);
    }

    #[test]
    fn placement_rule_round_trip() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mut mgr = PoolManager::create(cfg).unwrap();
        let rule = PlacementRule::Pin {
            query: mimisbrunnr_types::Query::HasTag(mimisbrunnr_types::TagId::new(7)),
            tier: StorageTier::Hot,
        };
        mgr.add_placement_rule(rule.clone()).unwrap();
        assert_eq!(mgr.placement_rules().len(), 1);

        // Encode + decode via crate::placement helpers — proves CBOR
        // shape stays stable across save/restore.
        let bytes = crate::placement::encode_placement_rules(mgr.placement_rules()).unwrap();
        let back = crate::placement::decode_placement_rules(&bytes).unwrap();
        assert_eq!(back, vec![rule]);
    }

    #[test]
    fn status_tier_breakdown() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mgr = PoolManager::create(cfg).unwrap();
        let s = mgr.status();
        let hot = s.by_tier.get(&StorageTier::Hot).unwrap();
        assert_eq!(hot.disk_count, 1);
        let cold = s.by_tier.get(&StorageTier::Cold).unwrap();
        assert_eq!(cold.disk_count, 1);
    }

    #[test]
    fn open_with_wrong_node_id_errors() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let _mgr = PoolManager::create(cfg.clone()).unwrap();

        let mut bad = cfg;
        bad.node_id = 99;
        let err = PoolManager::open(bad).unwrap_err();
        assert!(matches!(err, PoolError::NodeIdMismatch { .. }));
    }

    #[test]
    fn save_config_persists_post_add_disk() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mut mgr = PoolManager::create(cfg).unwrap();
        let new_path = tmp.path().join("dynamic.img");
        let new_entry = entry(2, new_path, StorageTier::Warm, MediaType::Ssd, 16 * 1024 * 1024);
        mgr.add_disk(new_entry).unwrap();

        let toml_path = tmp.path().join("pool.toml");
        mgr.save_config(&toml_path).unwrap();

        // Reload via PoolConfig::load_toml — verifies the manager-owned
        // save path produces a re-readable file with the new disk.
        let reloaded = PoolConfig::load_toml(&toml_path).unwrap();
        assert_eq!(reloaded.disks.len(), 3);
        assert!(reloaded.disks.iter().any(|d| d.id == 2));
    }

    #[test]
    fn disk_superblock_returns_some_for_each_disk() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mgr = PoolManager::create(cfg).unwrap();
        let sb0 = mgr.disk_superblock(0).expect("primary disk superblock");
        assert_eq!(sb0.node_id(), 1);
        assert_eq!(sb0.disk_id(), 0);
        let sb1 = mgr.disk_superblock(1).expect("secondary disk superblock");
        assert_eq!(sb1.node_id(), 1);
        assert_eq!(sb1.disk_id(), 1);
    }

    #[test]
    fn disk_superblock_unknown_returns_none() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mgr = PoolManager::create(cfg).unwrap();
        assert!(mgr.disk_superblock(99).is_none());
    }

    #[test]
    fn disk_superblock_after_add_disk_is_present() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mut mgr = PoolManager::create(cfg).unwrap();
        let new_path = tmp.path().join("disk2.img");
        let new_entry = entry(2, new_path, StorageTier::Warm, MediaType::Ssd, 16 * 1024 * 1024);
        mgr.add_disk(new_entry).unwrap();
        let sb2 = mgr.disk_superblock(2).expect("freshly added disk superblock");
        assert_eq!(sb2.disk_id(), 2);
    }

    #[test]
    fn commit_is_callable_post_create() {
        let tmp = TempDir::new().unwrap();
        let cfg = small_pool(&tmp);
        let mut mgr = PoolManager::create(cfg).unwrap();
        mgr.commit().unwrap();
    }
}
