//! [`DiskEngine`] — persistent wrapper around [`Engine`] (DESIGN §15).
//!
//! Glues the in-memory engine to:
//!
//! - the per-disk [`Wal`] (writes every mutation as a `WalOp` entry),
//! - the primary disk's [`Superblock`] (atomic root commit at checkpoint),
//! - a [`PoolManager`] (multi-disk lifecycle).
//!
//! ## Index persistence layout (post-R1b-4)
//!
//! Within the index zone, R1b carves out **twelve** dedicated 256 KiB
//! §1.5 B+ tree regions for the migrated indices and tables, with the
//! remaining state (oplog, path contexts, scalar bookkeeping) still
//! living in a length-prefixed CBOR blob at the tail of the zone:
//!
//! ```text
//! [zone.offset + 0]                  ChunkIndex region        (256 KiB)   ← R1b-1
//! [zone.offset + 256 KiB]            KvIndex region           (256 KiB)   ← R1b-1
//! [zone.offset + 512 KiB]            ForwardIndex region      (256 KiB)   ← R1b-2
//! [zone.offset + 768 KiB]            TagIndex region          (256 KiB)   ← R1b-2
//! [zone.offset + 1024 KiB]           RangeIndex region        (256 KiB)   ← R1b-2
//! [zone.offset + 1280 KiB]           ObjectTable region       (256 KiB)   ← R1b-3
//! [zone.offset + 1536 KiB]           LocationTable region     (256 KiB)   ← R1b-3
//! [zone.offset + 1792 KiB]           Ontology region          (256 KiB)   ← R1b-3*
//! [zone.offset + 2048 KiB]           Subscriptions region     (256 KiB)   ← R1b-3*
//! [zone.offset + 2304 KiB]           BackpointerTable region  (256 KiB)   ← R1b-3
//! [zone.offset + 2560 KiB]           BucketAllocTable region  (256 KiB)   ← R1b-4
//! [zone.offset + 2816 KiB]           FreespaceLru region      (256 KiB)   ← R1b-4
//! [zone.offset + 3072 KiB]           Legacy CBOR blob         (MIXI magic + u32 len + CBOR)
//! ```
//!
//! \* Ontology and Subscriptions regions landed alongside R1b-3 since
//! their `flush_to_region` / `load_from_region` APIs were already in
//! place; they're documented here so the layout stays self-describing.
//!
//! Total dedicated region prefix after R1b-4: **3.0 MiB**. With the
//! default 3%-of-disk index zone, a 1 GiB pool (≈ 30 MiB index zone)
//! leaves ~27 MiB for the legacy blob. The 4 MiB minimum index
//! zone (`FMT_INDEX_ZONE_SIZE` in `mimisbrunnr-pool::tier`) leaves
//! exactly **1 MiB** for the legacy blob — tight but workable for the
//! few scalars plus the path-contexts CBOR + oplog + transient blob
//! `HashMap`. If a workload outgrows that, bump
//! `FMT_INDEX_ZONE_SIZE` to 8 MiB.
//!
//! Per IMPL §12.2 / §12.4 the bucket alloc table and freespace LRU
//! are actually **per-disk** B+ trees rooted at
//! `DiskDescriptorOnDisk.{buckets_root, freespace_root}`. R1b-4
//! simplifies to one pool-scoped region in the index zone; the wire
//! keys already carry `disk_id` so the per-disk split (R1d) is a
//! layout-only change with no on-disk format break. The
//! `RootPointer` does **not** gain `bucket_alloc_root` /
//! `freespace_lru_root` slots — per spec those fields live on the
//! per-disk descriptor, not on the pool-wide root pointer.
//!
//! The legacy blob now carries only:
//!
//! - `oplog` (not yet migrated; tracked under R1b-N for a later phase),
//! - `path_contexts` (per IMPL §10.3, no native region — derived from
//!   forward-index `Attr(unix-path, ...)` at engine boot, see
//!   `mimisbrunnr-unix::PathContextManager` docs),
//! - scalar bookkeeping (`next_oid_local`, `next_tag_id`,
//!   `last_applied_lsn`),
//! - the transient `blobs: HashMap<u64, Vec<u8>>` (replaced by the real
//!   blob zone once R4 lands).
//!
//! Backwards compatibility with pre-R1b-13 pools is **not** supported:
//! every region offset and the legacy-blob offset have shifted. This is
//! a one-way migration; recreating the pool is the only path forward.
//!
//! TODO(rewrite-phase-R1b-N): migrate `oplog` to its own region (or fold
//! its content into a new `BtreeKind` slot once one is allocated). After
//! that the legacy blob can be retired entirely modulo the few scalars,
//! which can move to the superblock.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use ciborium::{de::from_reader, ser::into_writer};
use log::trace;
use mimisbrunnr_index::{ChunkIndex, ForwardIndex, KvIndex, RangeIndex, TagIndex};
use mimisbrunnr_meta::{BackpointerTable, LocationTable, ObjectTable};

/// Per-index slot offsets within the index zone. R1b-4 wires twelve
/// dedicated 256 KiB §1.5 B+ tree regions, pushing the legacy CBOR blob
/// to offset 3.0 MiB.
pub(crate) const CHUNK_INDEX_REGION_OFFSET: u64 = 0;
/// See [`CHUNK_INDEX_REGION_OFFSET`].
pub(crate) const KV_INDEX_REGION_OFFSET: u64 = 256 * 1024;
/// See [`CHUNK_INDEX_REGION_OFFSET`].
pub(crate) const FORWARD_INDEX_REGION_OFFSET: u64 = 512 * 1024;
/// See [`CHUNK_INDEX_REGION_OFFSET`].
pub(crate) const TAG_INDEX_REGION_OFFSET: u64 = 768 * 1024;
/// See [`CHUNK_INDEX_REGION_OFFSET`].
pub(crate) const RANGE_INDEX_REGION_OFFSET: u64 = 1024 * 1024;
/// See [`CHUNK_INDEX_REGION_OFFSET`]. Added by R1b-3.
pub(crate) const OBJECT_TABLE_REGION_OFFSET: u64 = 1280 * 1024;
/// See [`CHUNK_INDEX_REGION_OFFSET`]. Added by R1b-3.
pub(crate) const LOCATION_TABLE_REGION_OFFSET: u64 = 1536 * 1024;
/// See [`CHUNK_INDEX_REGION_OFFSET`]. Added alongside R1b-3.
pub(crate) const ONTOLOGY_REGION_OFFSET: u64 = 1792 * 1024;
/// See [`CHUNK_INDEX_REGION_OFFSET`]. Added alongside R1b-3.
pub(crate) const SUBSCRIPTIONS_REGION_OFFSET: u64 = 2048 * 1024;
/// See [`CHUNK_INDEX_REGION_OFFSET`]. Added by R1b-3.
pub(crate) const BACKPOINTER_TABLE_REGION_OFFSET: u64 = 2304 * 1024;
/// See [`CHUNK_INDEX_REGION_OFFSET`]. Added by R1b-4.
pub(crate) const BUCKET_ALLOC_REGION_OFFSET: u64 = 2560 * 1024;
/// See [`CHUNK_INDEX_REGION_OFFSET`]. Added by R1b-4.
pub(crate) const FREESPACE_LRU_REGION_OFFSET: u64 = 2816 * 1024;
/// See [`CHUNK_INDEX_REGION_OFFSET`]. Shifted from 2.5 MiB to 3.0 MiB
/// by R1b-4.
pub(crate) const LEGACY_CBOR_BLOB_OFFSET: u64 = 3072 * 1024;
/// TagIndex per-tag `TagBitmapPage` chains (R1c-A3.3) live in this area.
/// Sits after the legacy CBOR blob (which is sized for up to ~1 MiB of
/// path-context / oplog / subscriptions metadata); each 4 KiB slot here
/// holds one bitmap-page chain link. Sized to [`TAG_BITMAP_AREA_PAGES`]
/// pages (4 MiB) — comfortably more than smoke-test workloads need.
pub(crate) const TAG_BITMAP_AREA_OFFSET: u64 = 4 * 1024 * 1024;
/// Number of 4 KiB `TagBitmapPage` slots in the tag bitmap area.
pub(crate) const TAG_BITMAP_AREA_PAGES: usize = 1024;
/// ForwardIndex per-object `ForwardOverflowRegion` chains (R1c-A3.2)
/// live in this area. Sits after the tag bitmap area. Each slot holds
/// one 256 KiB `ForwardOverflowRegion`, chained via the trailing
/// `next_page: BlockRef` slot. Sized to
/// [`FORWARD_OVERFLOW_AREA_REGIONS`] regions (8 MiB).
pub(crate) const FORWARD_OVERFLOW_AREA_OFFSET: u64 = 8 * 1024 * 1024;
/// Number of 256 KiB slots in the forward-overflow area.
pub(crate) const FORWARD_OVERFLOW_AREA_REGIONS: usize = 32;
/// 256 KiB region size — matches `Superblock.btree_node_size_log2 = 18`.
#[allow(dead_code)] // referenced in size assertions / future dynamic layout work.
pub(crate) const REGION_SIZE: u64 = 256 * 1024;

/// Convert a [`BlockRef`] to its disk-absolute byte offset.
///
/// `BlockRef.block_no` is in 4 KiB units (IMPL §2.3 / §12.3). Used by the
/// R1c-D1 minimum-viable wiring that populates `RootPointer.*_root`
/// slots from the existing fixed-offset region layout.
fn block_ref_offset(block_ref: &BlockRef) -> u64 {
    let block_no = { block_ref.block_no };
    block_no as u64 * mimisbrunnr_storage::BLOCK_SIZE as u64
}

/// Build a [`BlockRef`] for the tree currently living at
/// `zone_offset + tree_offset_in_zone`. R1c-D1 uses this to seed initial
/// `RootPointer.*_root` slots at format/create time; subsequent commits
/// don't reallocate (COW reallocation is Tier 3 D3).
///
/// `disk_id` is the primary-disk id; `generation` is `1` for fresh
/// pools and never bumps under R1c-D1 (the buckets aren't reused).
fn root_ref_for(disk_id: u16, zone_offset: u64, tree_offset_in_zone: u64) -> BlockRef {
    let absolute = zone_offset + tree_offset_in_zone;
    let block_no = (absolute / mimisbrunnr_storage::BLOCK_SIZE as u64) as u32;
    BlockRef {
        disk_id,
        _pad: 0,
        block_no,
        generation: 1,
    }
}

/// Populate every `RootPointer.*_root` slot the engine knows about
/// (R1c-D1). Called once at format time and again on every `commit()`
/// (the slots stay stable across commits under D1; D3 will rotate them
/// per commit).
fn seed_root_pointer(root: &mut mimisbrunnr_storage::RootPointer, disk_id: u16, zone_offset: u64) {
    root.chunk_index_root = root_ref_for(disk_id, zone_offset, CHUNK_INDEX_REGION_OFFSET);
    root.kv_index_root = root_ref_for(disk_id, zone_offset, KV_INDEX_REGION_OFFSET);
    root.forward_index_root = root_ref_for(disk_id, zone_offset, FORWARD_INDEX_REGION_OFFSET);
    root.tag_index_root = root_ref_for(disk_id, zone_offset, TAG_INDEX_REGION_OFFSET);
    root.range_index_root = root_ref_for(disk_id, zone_offset, RANGE_INDEX_REGION_OFFSET);
    root.object_table_root = root_ref_for(disk_id, zone_offset, OBJECT_TABLE_REGION_OFFSET);
    root.location_table_root = root_ref_for(disk_id, zone_offset, LOCATION_TABLE_REGION_OFFSET);
    root.ontology_root = root_ref_for(disk_id, zone_offset, ONTOLOGY_REGION_OFFSET);
    root.subscriptions_root = root_ref_for(disk_id, zone_offset, SUBSCRIPTIONS_REGION_OFFSET);
    root.backpointer_root = root_ref_for(disk_id, zone_offset, BACKPOINTER_TABLE_REGION_OFFSET);
    // disks_overflow / placement_rules / cluster_peers / reconcile_*
    // / value_spill / *_history / snapshot_chain stay BlockRef::ZERO
    // until later phases populate them.
    let _ = (
        root.disks_overflow_root,
        root.placement_rules_root,
        root.cluster_peers_root,
    );
}
use mimisbrunnr_ontology::{InstallResult, OntologyModule, OntologyState};
use mimisbrunnr_pool::{DiskConfigEntry, PoolConfig, PoolManager, PoolStatus};
use mimisbrunnr_storage::{
    BlockDevice, BlockRef, BucketAllocTable, FileBlockDevice, FreespaceLru, Superblock,
};
use mimisbrunnr_types::{
    ChangeInterest, DiskId, ObjectId, Query, SubscriptionId, TagId, Value,
};
use mimisbrunnr_unix::PathContextManager;
use mimisbrunnr_wal::{Checkpoint, Wal, WalOpKind};
use mimisbrunnr_watch::{Retention, SubscriptionEngine};
use serde::{Deserialize, Serialize};

use crate::{
    Engine, EngineError, OpKind, OpLog,
    engine::BlobWriteResult,
    wal_proj::{project_op, replay_wal_op},
};

/// Magic prefix for the index-zone CBOR blob; lets us tell a fresh
/// (all-zero) zone from a corrupted payload.
const INDEX_BLOB_MAGIC: u32 = 0x4D49_5849; // "MIXI"

/// Snapshot status reported by [`DiskEngine::status`].
#[derive(Debug, Clone)]
pub struct EngineStatus {
    pub pool: PoolStatus,
    pub object_count: usize,
    pub tag_count: usize,
    pub kv_entries: usize,
    pub range_entries: usize,
    pub chunk_count: usize,
    pub forward_objects: usize,
    pub oplog_len: usize,
    pub wal_next_lsn: u64,
    pub last_checkpoint_lsn: u64,
}

/// Persistent engine.
pub struct DiskEngine {
    pub engine: Engine,
    /// In-memory blob store keyed by `oid_local` (the bottom 48 bits of the
    /// `ObjectId`). A real implementation would push these to the blob zone;
    /// for Phase 6 we keep them in RAM and round-trip the metadata.
    pub blobs: HashMap<u64, Vec<u8>>,
    pub pool: PoolManager,
    pub primary_device: Arc<FileBlockDevice>,
    pub superblock: Superblock,
    pub wal: Wal,
    pub config: PoolConfig,
    pub config_path: PathBuf,
    /// `true` when constructed via [`DiskEngine::open_read_only`]. Every
    /// mutation method (`create_object`, `add_tag`, `commit`, …) returns
    /// [`EngineError::ReadOnly`] in this mode.
    read_only: bool,
}

// ---------- Index-zone CBOR blob shape ----------

#[derive(Debug, Serialize, Deserialize)]
struct IndexBlob {
    // R1b-13: forward / tag / range / chunk / kv indices, the
    // object_table / location_table tables, and the ontology /
    // subscriptions modules have all migrated to dedicated 256 KiB §1.5
    // B+ tree regions; their bytes are no longer carried here.
    path_contexts_bytes: Vec<u8>,
    oplog_bytes: Vec<u8>,
    next_oid_local: u64,
    next_tag_id: u32,
    last_applied_lsn: u64,
    blobs: HashMap<u64, Vec<u8>>,
}

// R1b-13: removed `ObjectRecordCbor` and `LocationCbor` helpers — the
// object_table and location_table now persist in their dedicated B+
// tree regions, so the legacy blob no longer carries per-object /
// per-location bytes.

impl DiskEngine {
    // -------------------------------------------------------------------
    // Construction.
    // -------------------------------------------------------------------

    /// Format a new pool and initialise the engine on top of it.
    pub fn create(config: PoolConfig, config_path: PathBuf) -> Result<Self, EngineError> {
        trace!("DiskEngine::create config_path={}", config_path.display());
        // 1. Create + format every disk in the pool.
        let pool = PoolManager::create(config.clone())?;
        // 2. Open primary disk's runtime view.
        let primary_id = config
            .primary()
            .ok_or(EngineError::Pool(mimisbrunnr_pool::PoolError::EmptyPool))?
            .id;
        let primary_dev = pool
            .runtime(primary_id)
            .ok_or(EngineError::Pool(
                mimisbrunnr_pool::PoolError::DiskNotFound(primary_id),
            ))?
            .device
            .clone();

        // 3. Read the superblock written by `PoolManager::create`.
        let mut superblock = Superblock::open(primary_dev.as_ref())?;
        let wal_offset = { superblock.wal_offset };
        let wal_size = { superblock.wal_size };

        // 4. Format the WAL ring.
        let wal = Wal::format(primary_dev.as_ref(), wal_offset, wal_size)?;

        // 5. Build empty engine.
        let engine = Engine::new(config.node_id);

        // 6. Populate the initial `RootPointer` (R1c-D1).
        //
        // Per IMPL §2.2, every B+ tree root is anchored as a `BlockRef`
        // in the active `RootPointer`. R1c-D1 ships the *minimum-viable*
        // population: each tree's slot is seeded with the `BlockRef`
        // that points at its existing fixed offset in the index zone.
        // Subsequent commits don't re-allocate (COW reallocation is
        // Tier 3 D3); the BlockRef stays stable across the pool's
        // lifetime under D1.
        //
        // The `disks_overflow_root`, `placement_rules_root`, and
        // `cluster_peers_root` stay `BlockRef::ZERO` — those trees
        // either don't yet exist (cluster_peers, deferred to R12) or
        // are populated only when the pool exceeds 12 disks
        // (disks_overflow). The reconcile, snapshot, value_spill, and
        // *_history slots are R6/R7 territory.
        let zone_offset = { superblock.index_zone.offset };
        let active = *superblock.active_root_pointer();
        let mut new_root = active;
        new_root.seq = { active.seq }.saturating_add(1);
        new_root.lsn = wal.next_lsn();
        seed_root_pointer(&mut new_root, primary_id, zone_offset);
        superblock.commit_root(primary_dev.as_ref(), new_root)?;

        // 7. Persist the pool config to TOML.
        config.save_toml(&config_path)?;

        Ok(Self {
            engine,
            blobs: HashMap::new(),
            pool,
            primary_device: primary_dev,
            superblock,
            wal,
            config,
            config_path,
            read_only: false,
        })
    }

    /// Open an existing pool, replay the WAL, restore index state.
    pub fn open(config_path: &Path) -> Result<Self, EngineError> {
        trace!("DiskEngine::open config_path={}", config_path.display());
        let config = PoolConfig::load_toml(config_path)?;
        let pool = PoolManager::open(config.clone())?;
        let primary_id = config
            .primary()
            .ok_or(EngineError::Pool(mimisbrunnr_pool::PoolError::EmptyPool))?
            .id;
        let primary_dev = pool
            .runtime(primary_id)
            .ok_or(EngineError::Pool(
                mimisbrunnr_pool::PoolError::DiskNotFound(primary_id),
            ))?
            .device
            .clone();

        let superblock = Superblock::open(primary_dev.as_ref())?;
        let wal_offset = { superblock.wal_offset };
        let wal_size = { superblock.wal_size };
        let wal = Wal::open(primary_dev.as_ref(), wal_offset, wal_size)?;

        let mut engine = Engine::new(config.node_id);

        let zone_offset = { superblock.index_zone.offset };

        // 1a. Migrated indices and tables: dedicated 256 KiB §1.5 B+
        // tree regions at fixed slot offsets.
        load_all_regions(
            &mut engine,
            primary_dev.as_ref(),
            zone_offset,
            superblock.active_root_pointer(),
            config.node_id,
        )?;

        // 1b. Restore everything else from the legacy CBOR blob (if any).
        let index_blob = read_index_blob(primary_dev.as_ref(), &superblock)?;
        let mut blobs: HashMap<u64, Vec<u8>> = HashMap::new();
        if let Some(blob) = index_blob {
            blobs = blob.blobs.clone();
            apply_index_blob(&mut engine, blob);
        }

        let mut de = Self {
            engine,
            blobs,
            pool,
            primary_device: primary_dev,
            superblock,
            wal,
            config,
            config_path: config_path.to_path_buf(),
            read_only: false,
        };

        // 2. Replay WAL entries past the last applied LSN.
        de.replay_wal()?;
        Ok(de)
    }

    /// Open the pool for **read-only** inspection.
    ///
    /// The primary device is opened via
    /// [`FileBlockDevice::open_read_only`] and the WAL ring via
    /// [`Wal::open_read_only`], so any stray write attempts fail at the device
    /// or WAL layer with the corresponding `ReadOnly` error.
    ///
    /// Compared to [`DiskEngine::open`], this constructor:
    ///
    /// - never writes back to the index zone (no `save_index_state`);
    /// - never persists `pool.toml`;
    /// - replays the WAL **only against the in-memory engine** — that replay
    ///   doesn't touch the disk, so a read-only WAL is fine. WAL entries that
    ///   would otherwise still be present after the in-memory replay remain
    ///   on disk; we don't trim them;
    /// - sets `read_only = true`, so every mutation method returns
    ///   [`EngineError::ReadOnly`].
    pub fn open_read_only(config_path: &Path) -> Result<Self, EngineError> {
        trace!(
            "DiskEngine::open_read_only config_path={}",
            config_path.display()
        );
        let config = PoolConfig::load_toml(config_path)?;
        // The PoolManager uses RW devices internally — use it for reads of
        // the pool state root, but build our own read-only primary device for
        // the engine-side WAL/superblock/index work.
        let pool = PoolManager::open(config.clone())?;
        let primary_id = config
            .primary()
            .ok_or(EngineError::Pool(mimisbrunnr_pool::PoolError::EmptyPool))?
            .id;
        let primary_entry = config
            .disks
            .iter()
            .find(|d| d.id == primary_id)
            .ok_or(EngineError::Pool(
                mimisbrunnr_pool::PoolError::DiskNotFound(primary_id),
            ))?;
        let primary_dev: Arc<FileBlockDevice> =
            Arc::new(FileBlockDevice::open_read_only(&primary_entry.path)?);

        let superblock = Superblock::open(primary_dev.as_ref())?;
        let wal_offset = { superblock.wal_offset };
        let wal_size = { superblock.wal_size };
        let wal = Wal::open_read_only(primary_dev.as_ref(), wal_offset, wal_size)?;

        let mut engine = Engine::new(config.node_id);

        let zone_offset = { superblock.index_zone.offset };

        // Migrated indices and tables live in dedicated regions at
        // fixed slot offsets.
        load_all_regions(
            &mut engine,
            primary_dev.as_ref(),
            zone_offset,
            superblock.active_root_pointer(),
            config.node_id,
        )?;

        // Restore everything else from the legacy CBOR blob (if any).
        let index_blob = read_index_blob(primary_dev.as_ref(), &superblock)?;
        let mut blobs: HashMap<u64, Vec<u8>> = HashMap::new();
        if let Some(blob) = index_blob {
            blobs = blob.blobs.clone();
            apply_index_blob(&mut engine, blob);
        }

        let mut de = Self {
            engine,
            blobs,
            pool,
            primary_device: primary_dev,
            superblock,
            wal,
            config,
            config_path: config_path.to_path_buf(),
            read_only: true,
        };

        // Replay WAL entries past last_applied_lsn — purely in-memory; the
        // WAL is read-only so no on-disk side effects occur.
        de.replay_wal()?;
        Ok(de)
    }

    /// `true` if this engine was opened read-only.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    // -------------------------------------------------------------------
    // Persistence: index zone (CBOR blob) and atomic root commit.
    // -------------------------------------------------------------------

    /// Save the engine's index state to the index zone:
    ///
    /// 1. Flush the migrated indices ([`ChunkIndex`], [`KvIndex`],
    ///    [`ForwardIndex`], [`TagIndex`], [`RangeIndex`]) to their
    ///    dedicated 256 KiB §1.5 B+ tree regions at fixed slot offsets.
    /// 2. Write the remaining indices as a length-prefixed CBOR blob at
    ///    [`LEGACY_CBOR_BLOB_OFFSET`] within the zone.
    pub fn save_index_state(&mut self) -> Result<(), EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }

        let zone_offset = { self.superblock.index_zone.offset };
        let zone_length = { self.superblock.index_zone.length };

        // Sanity-check zone is large enough for the new layout. R1c-A3.2
        // adds the forward-overflow area after the tag bitmap area, so
        // the zone needs space for both.
        let forward_overflow_area_end = FORWARD_OVERFLOW_AREA_OFFSET
            + (FORWARD_OVERFLOW_AREA_REGIONS as u64) * 256 * 1024;
        if zone_length < forward_overflow_area_end {
            return Err(EngineError::NotImplemented(
                "index zone too small for R1c-A3.2 layout (needs ≥ 16 MiB for directory regions + tag bitmap area + forward overflow area)",
            ));
        }

        // 1. Flush migrated indices to the offsets named by their
        //    `RootPointer.*_root` BlockRef slots (R1c-D1). The slot
        //    values are seeded at format time and don't change across
        //    commits under D1; D3 will rotate them per commit.
        let active = *self.superblock.active_root_pointer();
        let dev = self.primary_device.as_ref();
        self.engine
            .chunk_index
            .flush_to_region(dev, block_ref_offset(&active.chunk_index_root))
            .map_err(EngineError::from)?;
        self.engine
            .kv_index
            .flush_to_region(dev, block_ref_offset(&active.kv_index_root))
            .map_err(EngineError::from)?;
        self.engine
            .forward_index
            .flush_to_region(
                dev,
                block_ref_offset(&active.forward_index_root),
                zone_offset + FORWARD_OVERFLOW_AREA_OFFSET,
                FORWARD_OVERFLOW_AREA_REGIONS,
            )
            .map_err(EngineError::from)?;
        // TagIndex needs a bitmap-area reservation in addition to the
        // directory region. The bitmap area is fixed at
        // `zone_offset + TAG_BITMAP_AREA_OFFSET` (R1c-A3.3 layout).
        self.engine
            .tag_index
            .flush_to_region(
                dev,
                block_ref_offset(&active.tag_index_root),
                zone_offset + TAG_BITMAP_AREA_OFFSET,
                TAG_BITMAP_AREA_PAGES,
            )
            .map_err(EngineError::from)?;
        self.engine
            .range_index
            .flush_to_region(dev, block_ref_offset(&active.range_index_root))
            .map_err(EngineError::from)?;
        self.engine
            .object_table
            .flush_to_region(dev, block_ref_offset(&active.object_table_root))
            .map_err(EngineError::from)?;
        self.engine
            .location_table
            .flush_to_region(dev, block_ref_offset(&active.location_table_root))
            .map_err(EngineError::from)?;
        self.engine
            .ontology
            .flush_to_region(dev, block_ref_offset(&active.ontology_root))
            .map_err(EngineError::from)?;
        self.engine
            .subscriptions
            .flush_to_region(dev, block_ref_offset(&active.subscriptions_root))
            .map_err(EngineError::from)?;
        self.engine
            .backpointer_table
            .flush_to_region(dev, block_ref_offset(&active.backpointer_root))
            .map_err(EngineError::from)?;
        // R1b-4: bucket alloc + freespace LRU. Pool-scoped for now;
        // wire `disk_id = 0` until R1d splits per-disk per IMPL §12.2.
        self.engine
            .bucket_alloc
            .flush_to_region(
                self.primary_device.as_ref(),
                zone_offset + BUCKET_ALLOC_REGION_OFFSET,
                0,
            )
            .map_err(EngineError::from)?;
        self.engine
            .freespace_lru
            .flush_to_region(
                self.primary_device.as_ref(),
                zone_offset + FREESPACE_LRU_REGION_OFFSET,
            )
            .map_err(EngineError::from)?;

        // 2. Build + write the legacy CBOR blob (everything else).
        let blob = build_index_blob(&self.engine, &self.blobs)?;
        let mut payload = Vec::new();
        into_writer(&blob, &mut payload)?;

        // Frame: [magic u32 le | length u32 le | cbor bytes ...]
        let total = 4 + 4 + payload.len() as u64;
        let blob_room = zone_length - LEGACY_CBOR_BLOB_OFFSET;
        if total > blob_room {
            return Err(EngineError::NotImplemented(
                "legacy index blob too large for the index zone — TODO(rewrite-phase-R1b-N): \
                 migrate the remaining indices off the CBOR blob",
            ));
        }
        let mut framed = Vec::with_capacity(total as usize);
        framed.extend_from_slice(&INDEX_BLOB_MAGIC.to_le_bytes());
        framed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        framed.extend_from_slice(&payload);
        self.primary_device
            .write_at(zone_offset + LEGACY_CBOR_BLOB_OFFSET, &framed)?;
        self.primary_device.sync()?;
        Ok(())
    }

    /// Reload the index state from disk (overwrites in-memory state).
    pub fn load_index_state(&mut self) -> Result<(), EngineError> {
        let zone_offset = { self.superblock.index_zone.offset };
        let root = *self.superblock.active_root_pointer();
        let node_id = self.config.node_id;

        // Migrated indices and tables first.
        load_all_regions(
            &mut self.engine,
            self.primary_device.as_ref(),
            zone_offset,
            &root,
            node_id,
        )?;

        // Legacy CBOR blob (oplog + path_contexts + scalars + transient
        // blob map). Object/location/ontology/subscriptions/backpointer
        // fields already loaded from their dedicated regions; the
        // legacy-blob applier no longer touches them.
        if let Some(blob) =
            read_index_blob(self.primary_device.as_ref(), &self.superblock)?
        {
            self.blobs = blob.blobs.clone();
            apply_index_blob(&mut self.engine, blob);
        }
        Ok(())
    }

    /// Replay every WAL entry past `last_applied_lsn` against the engine.
    fn replay_wal(&mut self) -> Result<(), EngineError> {
        let start = self.engine.last_applied_lsn.saturating_add(1);
        let entries: Vec<_> = self
            .wal
            .iter_from(self.primary_device.as_ref(), start)
            .collect();
        for entry in entries {
            let entry = entry?;
            let kind = WalOpKind::from_u8(entry.header.op_kind)?;
            let lsn = { entry.header.lsn };
            // Engine-only ops; checkpoint and others are handled separately.
            if matches!(kind, WalOpKind::Checkpoint) {
                continue;
            }
            // Skip ops the engine doesn't model — `replay_wal_op` handles
            // both engine-known and engine-unknown variants.
            if let Err(EngineError::LsnAlreadyApplied(_)) =
                replay_wal_op(&mut self.engine, kind, &entry.payload, lsn)
            {
                continue;
            }
        }
        Ok(())
    }

    /// Atomic root-commit (DESIGN §15 MVP cadence):
    ///
    /// 1. Save index state to the index zone.
    /// 2. Append a `Checkpoint` WAL entry recording the new root.
    /// 3. Trim the WAL ring up to the new checkpoint LSN.
    /// 4. Flip the active root in all 3 superblock copies.
    /// 5. fsync.
    pub fn commit(&mut self) -> Result<(), EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        // 1. Save index state.
        self.save_index_state()?;

        // 2. Build a fresh root pointer carrying the LSN we're about to
        //    checkpoint at. Most fields stay `BlockRef::ZERO` placeholders
        //    until the §1.5 B+ tree machinery lands.
        // TODO(rewrite-phase-N): populate every B+ tree root in RootPointer
        // (object_table_root, location_table_root, …). For now they are
        // BlockRef::ZERO sentinels.
        //
        // Note (R1b-4): `bucket_alloc_root` / `freespace_lru_root` are
        // **not** RootPointer fields — per IMPL §12.2 / §12.4 those
        // roots live on `DiskDescriptorOnDisk.{buckets_root,
        // freespace_root}`, not on the pool-wide root pointer. The
        // R1b-4 pool-scoped simplification keeps both regions in the
        // index zone instead, so the per-disk root fields stay
        // zeroed until R1d splits the trees per-disk.
        let next_lsn = self.wal.next_lsn();
        let mut new_root = *self.superblock.active_root_pointer();
        new_root.seq = { new_root.seq }.saturating_add(1);
        new_root.lsn = next_lsn;
        // R1c-D1: keep `RootPointer.*_root` slots populated. Trees stay
        // at their existing fixed offsets across commits; the slots are
        // re-seeded each commit so a pool created pre-R1c-D1 gets its
        // RootPointer healed on the next commit.
        let zone_offset = { self.superblock.index_zone.offset };
        let primary_id = { self.superblock.disk_id };
        seed_root_pointer(&mut new_root, primary_id, zone_offset);
        new_root.recompute_crc();

        // 3. Append checkpoint WAL entry.
        let cp = Checkpoint::from_root(&new_root, 0);
        let lsn = self.wal.append(
            self.primary_device.as_ref(),
            WalOpKind::Checkpoint,
            &cp,
            self.engine.clock.now(),
        )?;

        // 4. Trim the WAL up to and including `lsn`.
        self.wal.checkpoint(self.primary_device.as_ref(), lsn)?;

        // 5. Flip the active root.
        // The root we wrote referenced `next_lsn`, but the actual checkpoint
        // entry got LSN `lsn`. Update the in-memory copy and commit.
        new_root.lsn = lsn;
        new_root.recompute_crc();
        self.superblock
            .commit_root(self.primary_device.as_ref(), new_root)?;

        // Track replay watermark.
        self.engine.last_applied_lsn = self.engine.last_applied_lsn.max(lsn);
        Ok(())
    }

    // -------------------------------------------------------------------
    // Disk lifecycle proxies.
    // -------------------------------------------------------------------

    pub fn add_disk(&mut self, entry: DiskConfigEntry) -> Result<DiskId, EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        let id = self.pool.add_disk(entry)?;
        // Keep the in-engine `config` mirror in sync, then let the pool
        // manager persist its authoritative copy.
        self.config = self.pool.config().clone();
        self.pool.save_config(&self.config_path)?;
        Ok(id)
    }

    pub fn remove_disk(&mut self, id: DiskId) -> Result<(), EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        self.pool.remove_disk(id)?;
        self.config = self.pool.config().clone();
        self.pool.save_config(&self.config_path)?;
        Ok(())
    }

    pub fn status(&self) -> EngineStatus {
        EngineStatus {
            pool: self.pool.status(),
            object_count: self.engine.object_count(),
            tag_count: self.engine.tag_index.tag_count(),
            kv_entries: self.engine.kv_index.entry_count(),
            range_entries: self.engine.range_index.entry_count(),
            chunk_count: self.engine.chunk_index.chunk_count(),
            forward_objects: self.engine.forward_index.object_count(),
            oplog_len: self.engine.oplog.len(),
            wal_next_lsn: self.wal.next_lsn(),
            last_checkpoint_lsn: self.wal.last_checkpoint_lsn(),
        }
    }

    // -------------------------------------------------------------------
    // Public mutation surface — wraps `Engine::*`, projects to a WAL op,
    // appends, and stamps the engine oplog with the assigned LSN.
    // -------------------------------------------------------------------

    pub fn create_object(&mut self) -> Result<ObjectId, EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        let oid = self.engine.create_object();
        self.append_wal(&OpKind::CreateObject { oid })?;
        Ok(oid)
    }

    pub fn delete_object(&mut self, oid: ObjectId) -> Result<(), EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        self.engine.delete_object(oid)?;
        self.append_wal(&OpKind::DeleteObject { oid })?;
        Ok(())
    }

    pub fn add_tag(&mut self, oid: ObjectId, tag: TagId) -> Result<(), EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        self.engine.add_tag(oid, tag)?;
        self.append_wal(&OpKind::AddTag { oid, tag })?;
        Ok(())
    }

    pub fn remove_tag(&mut self, oid: ObjectId, tag: TagId) -> Result<(), EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        self.engine.remove_tag(oid, tag)?;
        self.append_wal(&OpKind::RemoveTag { oid, tag })?;
        Ok(())
    }

    pub fn set_attr(
        &mut self,
        oid: ObjectId,
        key: TagId,
        value: Value,
    ) -> Result<(), EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        self.engine.set_attr(oid, key, value.clone())?;
        self.append_wal(&OpKind::SetAttr { oid, key, value })?;
        Ok(())
    }

    pub fn remove_attr(
        &mut self,
        oid: ObjectId,
        key: TagId,
        value_hash: u64,
    ) -> Result<(), EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        self.engine.remove_attr(oid, key, value_hash)?;
        self.append_wal(&OpKind::RemoveAttr {
            oid,
            key,
            value_hash,
        })?;
        Ok(())
    }

    pub fn add_relation(
        &mut self,
        oid: ObjectId,
        predicate: TagId,
        target: ObjectId,
    ) -> Result<(), EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        self.engine.add_relation(oid, predicate, target)?;
        self.append_wal(&OpKind::AddRelation {
            oid,
            predicate,
            target,
        })?;
        Ok(())
    }

    pub fn remove_relation(
        &mut self,
        oid: ObjectId,
        predicate: TagId,
        target: ObjectId,
    ) -> Result<(), EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        self.engine.remove_relation(oid, predicate, target)?;
        self.append_wal(&OpKind::RemoveRelation {
            oid,
            predicate,
            target,
        })?;
        Ok(())
    }

    /// Run the transform pipeline, persist the bytes into the in-memory
    /// `blobs` map (Phase 6 placeholder) and append a `WriteBlob` WAL op.
    /// TODO(rewrite-phase-N): write to the blob zone via the placement engine
    /// rather than an in-RAM HashMap.
    pub fn write_blob(
        &mut self,
        oid: ObjectId,
        plaintext: &[u8],
    ) -> Result<BlobWriteResult, EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        let res = self.engine.write_blob(oid, plaintext)?;
        self.blobs.insert(oid.local_seq(), res.data.clone());
        self.append_wal(&OpKind::WriteBlob {
            oid,
            content_hash: res.content_hash,
            size: res.original_size,
        })?;
        Ok(res)
    }

    /// Read previously-written blob bytes for `oid`. Returns
    /// `Ok(Some(bytes))` if a blob is present, `Ok(None)` if `oid` has
    /// never been written. The bytes returned are the **post-transform**
    /// payload (whatever `write_blob` stored).
    ///
    /// TODO(rewrite-phase-R4): read from the blob zone via `ObjectLocation`
    /// (chunked / replicated) and run the inverse transform pipeline.
    pub fn read_blob(&self, oid: ObjectId) -> Result<Option<Vec<u8>>, EngineError> {
        Ok(self.blobs.get(&oid.local_seq()).cloned())
    }

    /// Subscribe through the in-memory engine; no WAL entry. Subscriptions
    /// are persisted opaquely as part of the index-zone CBOR blob.
    pub fn subscribe(
        &mut self,
        name: String,
        query: Query,
        interest: ChangeInterest,
        retention: Retention,
    ) -> Result<SubscriptionId, EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        self.engine.subscribe(name, query, interest, retention)
    }

    /// Install an ontology module; no WAL entry. The module ends up in the
    /// index-zone CBOR blob at the next `commit`.
    pub fn install_ontology_module(
        &mut self,
        module: OntologyModule,
    ) -> Result<InstallResult, EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }
        self.engine.install_ontology_module(module)
    }

    // -------------------------------------------------------------------
    // Snapshot + reconcile stubs.
    // -------------------------------------------------------------------

    /// Snapshot creation — DESIGN §11 / IMPL §11. Stub for Phase 6.
    pub fn snapshot_create(
        &mut self,
        _label: Option<String>,
    ) -> Result<u32, EngineError> {
        Err(EngineError::NotImplemented(
            "snapshots — IMPL §11 (snapshot tree, ancestor bitmap, sidecar history btrees)",
        ))
    }

    /// Reconcile work-queue driver — DESIGN §17 / IMPL §17. Stub for Phase 6.
    pub fn reconcile_step(&mut self, _max_items: usize) -> Result<usize, EngineError> {
        // TODO(rewrite-phase-N): drive the reconcile work-queue.
        Ok(0)
    }

    // -------------------------------------------------------------------
    // Internals.
    // -------------------------------------------------------------------

    fn append_wal(&mut self, op: &OpKind) -> Result<u64, EngineError> {
        let wal_op = project_op(op);
        let (kind, payload) = wal_op.encode()?;
        let lsn = self.wal.append_raw(
            self.primary_device.as_ref(),
            kind,
            &payload,
            self.engine.clock.now(),
            0,
        )?;
        Ok(lsn)
    }
}

// ----------------------------------------------------------------------
// Index-zone CBOR helpers (free functions to avoid borrow conflicts).
// ----------------------------------------------------------------------

/// Load every R1b-migrated index / table from its dedicated 256 KiB
/// §1.5 B+ tree region. Used by [`DiskEngine::open`],
/// [`DiskEngine::open_read_only`] and [`DiskEngine::load_index_state`].
/// Load every R1b dedicated B+ tree region.
///
/// Migrated indices and tables: read from the offsets named by the
/// active `RootPointer.*_root` slots (R1c-D1). A `BlockRef::ZERO` slot
/// indicates a tree that was never initialised under D1+ → return
/// `default()` (empty). Pre-D1 pools — created when `RootPointer.*_root`
/// were always zeroed — therefore start blank under D1; recreate the
/// pool to load existing data.
///
/// The bootstrap roots (`bucket_alloc`, `freespace_lru`) stay at fixed
/// offsets in the index zone; per IMPL §12.2 they're meant to live on
/// per-disk roots (`DiskDescriptorOnDisk.{buckets_root, freespace_root}`)
/// — the per-disk split is Tier 3 E1 / E2.
///
/// `node_id` is needed by [`LocationTable::load_from_region`] (R1c-A2)
/// to reconstruct the full `ObjectId.to_u64()` keys from the leaf's
/// `oid_local` slot index. Single-node pools pass their own node id;
/// multi-node cluster recovery (R12) needs cross-reference against
/// `ObjectTable`.
fn load_all_regions<D: BlockDevice>(
    engine: &mut Engine,
    device: &D,
    zone_offset: u64,
    root: &mimisbrunnr_storage::RootPointer,
    node_id: u16,
) -> Result<(), EngineError> {
    engine.chunk_index = if is_zero(&root.chunk_index_root) {
        ChunkIndex::default()
    } else {
        ChunkIndex::load_from_region(device, block_ref_offset(&root.chunk_index_root))?
    };
    engine.kv_index = if is_zero(&root.kv_index_root) {
        KvIndex::default()
    } else {
        KvIndex::load_from_region(device, block_ref_offset(&root.kv_index_root))?
    };
    engine.forward_index = if is_zero(&root.forward_index_root) {
        ForwardIndex::default()
    } else {
        ForwardIndex::load_from_region(device, block_ref_offset(&root.forward_index_root))?
    };
    engine.tag_index = if is_zero(&root.tag_index_root) {
        TagIndex::default()
    } else {
        TagIndex::load_from_region(device, block_ref_offset(&root.tag_index_root))?
    };
    engine.range_index = if is_zero(&root.range_index_root) {
        RangeIndex::default()
    } else {
        RangeIndex::load_from_region(device, block_ref_offset(&root.range_index_root))?
    };
    engine.object_table = if is_zero(&root.object_table_root) {
        ObjectTable::default()
    } else {
        ObjectTable::load_from_region(device, block_ref_offset(&root.object_table_root))?
    };
    engine.location_table = if is_zero(&root.location_table_root) {
        LocationTable::default()
    } else {
        LocationTable::load_from_region(
            device,
            block_ref_offset(&root.location_table_root),
            node_id,
        )?
    };
    engine.ontology = if is_zero(&root.ontology_root) {
        OntologyState::default()
    } else {
        OntologyState::load_from_region(device, block_ref_offset(&root.ontology_root))?
    };
    engine.subscriptions = if is_zero(&root.subscriptions_root) {
        SubscriptionEngine::default()
    } else {
        SubscriptionEngine::load_from_region(device, block_ref_offset(&root.subscriptions_root))?
    };
    engine.backpointer_table = if is_zero(&root.backpointer_root) {
        BackpointerTable::default()
    } else {
        BackpointerTable::load_from_region(device, block_ref_offset(&root.backpointer_root))?
    };
    // Bootstrap roots stay at fixed offsets (Tier 3 E1/E2 splits per-disk).
    engine.bucket_alloc =
        BucketAllocTable::load_from_region(device, zone_offset + BUCKET_ALLOC_REGION_OFFSET)
            .map_err(EngineError::from)?;
    engine.freespace_lru =
        FreespaceLru::load_from_region(device, zone_offset + FREESPACE_LRU_REGION_OFFSET)
            .map_err(EngineError::from)?;
    Ok(())
}

/// `true` when the `BlockRef` is the all-zero sentinel (pre-D1 pool or
/// uninitialised slot).
fn is_zero(block_ref: &BlockRef) -> bool {
    let block_no = { block_ref.block_no };
    let generation = { block_ref.generation };
    block_no == 0 && generation == 0
}

fn build_index_blob(
    engine: &Engine,
    blobs: &HashMap<u64, Vec<u8>>,
) -> Result<IndexBlob, EngineError> {
    // R1b-13: object_table, location_table, ontology, subscriptions, and
    // every R1b-1..R1b-4 index have moved to dedicated 256 KiB §1.5 B+
    // tree regions; they are no longer carried in the legacy blob.
    Ok(IndexBlob {
        path_contexts_bytes: engine.path_contexts.serialise()?,
        oplog_bytes: engine.oplog.serialise()?,
        next_oid_local: engine.next_oid_local(),
        next_tag_id: index_blob_next_tag_id(engine),
        last_applied_lsn: engine.last_applied_lsn,
        blobs: blobs.clone(),
    })
}

/// Pull `next_tag_id` out of the engine without exposing the field publicly
/// (it's deliberately private). We can recover it from the highest registered
/// tag plus 1.
fn index_blob_next_tag_id(engine: &Engine) -> u32 {
    engine
        .ontology
        .tags
        .keys()
        .map(|t| t.raw())
        .max()
        .map(|m| m + 1)
        .unwrap_or(1)
}

fn read_index_blob(
    device: &dyn BlockDevice,
    superblock: &Superblock,
) -> Result<Option<IndexBlob>, EngineError> {
    let zone_offset = { superblock.index_zone.offset };
    let zone_length = { superblock.index_zone.length };
    if zone_length < LEGACY_CBOR_BLOB_OFFSET + 8 {
        return Ok(None);
    }
    let blob_offset = zone_offset + LEGACY_CBOR_BLOB_OFFSET;
    let blob_room = zone_length - LEGACY_CBOR_BLOB_OFFSET;
    let mut header = [0u8; 8];
    device.read_at(blob_offset, &mut header)?;
    let magic = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    if magic == 0 {
        // Fresh zone — no payload yet.
        return Ok(None);
    }
    if magic != INDEX_BLOB_MAGIC {
        // Treat as corruption — Phase 6: skip rather than fail boot. The WAL
        // replay will rebuild what it can.
        // TODO(rewrite-phase-N): surface this to the operator instead.
        return Ok(None);
    }
    let payload_len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as u64;
    if payload_len + 8 > blob_room {
        return Ok(None);
    }
    let mut payload = vec![0u8; payload_len as usize];
    device.read_at(blob_offset + 8, &mut payload)?;
    let blob: IndexBlob = from_reader(payload.as_slice())?;
    Ok(Some(blob))
}

fn apply_index_blob(engine: &mut Engine, blob: IndexBlob) {
    // R1b-13: forward / tag / range / chunk / kv indices, plus
    // object_table / location_table / ontology / subscriptions, all live
    // in dedicated B+ tree regions; the engine fields are populated by
    // the per-region loaders in `load_index_state` before this function
    // runs. The legacy blob carries only path_contexts, oplog, and a
    // few scalars.
    if let Ok(p) = PathContextManager::deserialise(&blob.path_contexts_bytes) {
        engine.path_contexts = p;
    }
    if let Ok(ol) = OpLog::deserialise(&blob.oplog_bytes) {
        engine.oplog = ol;
    }
    // Counters.
    {
        let mut next_local = blob.next_oid_local;
        // Defensive: can't be smaller than the engine's own counter.
        if engine.next_oid_local() > next_local {
            next_local = engine.next_oid_local();
        }
        // Use the public `ensure_oid_exists` API to nudge the counter.
        if next_local > engine.next_oid_local() {
            // Materialise a "ghost" oid one less than next_local; this
            // advances `next_oid_local` to exactly `next_local`.
            let ghost_local = next_local.saturating_sub(1);
            let ghost = ObjectId::from_parts(engine.node_id, ghost_local);
            let _ = engine.ensure_oid_exists(ghost);
        }
    }
    engine.last_applied_lsn = engine.last_applied_lsn.max(blob.last_applied_lsn);
    // next_tag_id: rolled forward via `register_tag` callers; persisted
    // via the ontology region so we don't need to do anything else
    // here. The blob's `next_tag_id` field exists for future use.
    let _ = blob.next_tag_id;
}

