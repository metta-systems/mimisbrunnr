//! [`DiskEngine`] — persistent wrapper around [`Engine`] (DESIGN §15).
//!
//! Glues the in-memory engine to:
//!
//! - the per-disk [`Wal`] (writes every mutation as a `WalOp` entry),
//! - the primary disk's [`Superblock`] (atomic root commit at checkpoint),
//! - a [`PoolManager`] (multi-disk lifecycle).
//!
//! ## Index persistence layout (post-R1b-1)
//!
//! Within the index zone, R1b-1 carves out two dedicated 256 KiB §1.5 B+
//! tree regions for the migrated indices, with the remaining indices still
//! living in a length-prefixed CBOR blob:
//!
//! ```text
//! [zone.offset + 0]                 ChunkIndex region    (256 KiB)
//! [zone.offset + 256 KiB]           KvIndex region       (256 KiB)
//! [zone.offset + 512 KiB]           Legacy CBOR blob     (MIXI magic + u32 len + CBOR)
//! ```
//!
//! Backwards compatibility with pre-R1b pools is **not** supported: the
//! legacy blob has moved 512 KiB into the zone, so older pools whose CBOR
//! blob sits at zone offset 0 will fail to load. This is a one-way
//! migration; recreating the pool is the only path forward.
//! TODO(rewrite-phase-R1b-2..N): migrate the remaining indices (forward,
//! tag, range, ontology, subscriptions, path contexts, oplog, object
//! table, location table) to per-index B+ tree regions and drop the CBOR
//! blob entirely.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use ciborium::{de::from_reader, ser::into_writer};
use log::trace;
use mimisbrunnr_index::{ChunkIndex, ForwardIndex, KvIndex, RangeIndex, TagIndex};
use mimisbrunnr_meta::{LocationTable, ObjectTable};

/// Per-index slot offsets within the index zone. R1b-1 places two
/// 256 KiB §1.5 B+ tree regions at the start of the zone (one per
/// migrated index) and pushes the legacy CBOR blob to immediately after
/// them.
const CHUNK_INDEX_REGION_OFFSET: u64 = 0;
/// See [`CHUNK_INDEX_REGION_OFFSET`].
const KV_INDEX_REGION_OFFSET: u64 = 256 * 1024;
/// See [`CHUNK_INDEX_REGION_OFFSET`].
const LEGACY_CBOR_BLOB_OFFSET: u64 = 512 * 1024;
/// 256 KiB region size — matches `Superblock.btree_node_size_log2 = 18`.
#[allow(dead_code)] // referenced in size assertions / future dynamic layout work.
const REGION_SIZE: u64 = 256 * 1024;
use mimisbrunnr_ontology::{InstallResult, OntologyModule, OntologyState};
use mimisbrunnr_pool::{DiskConfigEntry, PoolConfig, PoolManager, PoolStatus};
use mimisbrunnr_storage::{BlockDevice, FileBlockDevice, Superblock};
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
    forward_index_bytes: Vec<u8>,
    tag_index_bytes: Vec<u8>,
    range_index_bytes: Vec<u8>,
    ontology_bytes: Vec<u8>,
    subscriptions_bytes: Vec<u8>,
    path_contexts_bytes: Vec<u8>,
    oplog_bytes: Vec<u8>,
    /// Object table — serialised as a flat list of every record, since
    /// `ObjectTable` itself doesn't derive Serialize.
    objects: Vec<ObjectRecordCbor>,
    /// Location table.
    locations: Vec<LocationCbor>,
    next_oid_local: u64,
    next_tag_id: u32,
    last_applied_lsn: u64,
    blobs: HashMap<u64, Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ObjectRecordCbor {
    oid: u64,
    /// Raw 128-byte ObjectRecord image.
    bytes: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LocationCbor {
    oid: u64,
    /// Variable-length serialised ObjectLocation. `ObjectLocation::serialise`
    /// gives bytes; we reuse the same parser on read.
    bytes: Vec<u8>,
}

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
        let superblock = Superblock::open(primary_dev.as_ref())?;
        let wal_offset = { superblock.wal_offset };
        let wal_size = { superblock.wal_size };

        // 4. Format the WAL ring.
        let wal = Wal::format(primary_dev.as_ref(), wal_offset, wal_size)?;

        // 5. Build empty engine.
        let engine = Engine::new(config.node_id);

        // 6. Persist the pool config to TOML.
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

        // 1a. Migrated indices: ChunkIndex / KvIndex live in dedicated
        // 256 KiB §1.5 B+ tree regions at fixed slot offsets.
        engine.chunk_index = ChunkIndex::load_from_region(
            primary_dev.as_ref(),
            zone_offset + CHUNK_INDEX_REGION_OFFSET,
        )
        .map_err(EngineError::from)?;
        engine.kv_index = KvIndex::load_from_region(
            primary_dev.as_ref(),
            zone_offset + KV_INDEX_REGION_OFFSET,
        )
        .map_err(EngineError::from)?;

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

        // Migrated indices live in dedicated regions at fixed slot offsets.
        engine.chunk_index = ChunkIndex::load_from_region(
            primary_dev.as_ref(),
            zone_offset + CHUNK_INDEX_REGION_OFFSET,
        )
        .map_err(EngineError::from)?;
        engine.kv_index = KvIndex::load_from_region(
            primary_dev.as_ref(),
            zone_offset + KV_INDEX_REGION_OFFSET,
        )
        .map_err(EngineError::from)?;

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
    /// 1. Flush the migrated indices ([`ChunkIndex`], [`KvIndex`]) to their
    ///    dedicated 256 KiB §1.5 B+ tree regions at fixed slot offsets.
    /// 2. Write the remaining indices as a length-prefixed CBOR blob at
    ///    [`LEGACY_CBOR_BLOB_OFFSET`] within the zone.
    pub fn save_index_state(&mut self) -> Result<(), EngineError> {
        if self.read_only {
            return Err(EngineError::ReadOnly);
        }

        let zone_offset = { self.superblock.index_zone.offset };
        let zone_length = { self.superblock.index_zone.length };

        // Sanity-check zone is large enough for the new layout.
        if zone_length < LEGACY_CBOR_BLOB_OFFSET {
            return Err(EngineError::NotImplemented(
                "index zone too small for R1b-1 layout (needs ≥ 512 KiB before the legacy blob)",
            ));
        }

        // 1. Flush migrated indices to their fixed-offset regions.
        self.engine
            .chunk_index
            .flush_to_region(self.primary_device.as_ref(), zone_offset + CHUNK_INDEX_REGION_OFFSET)
            .map_err(EngineError::from)?;
        self.engine
            .kv_index
            .flush_to_region(self.primary_device.as_ref(), zone_offset + KV_INDEX_REGION_OFFSET)
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

        // Migrated indices first.
        self.engine.chunk_index = ChunkIndex::load_from_region(
            self.primary_device.as_ref(),
            zone_offset + CHUNK_INDEX_REGION_OFFSET,
        )
        .map_err(EngineError::from)?;
        self.engine.kv_index = KvIndex::load_from_region(
            self.primary_device.as_ref(),
            zone_offset + KV_INDEX_REGION_OFFSET,
        )
        .map_err(EngineError::from)?;

        // Legacy CBOR blob.
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
        let next_lsn = self.wal.next_lsn();
        let mut new_root = *self.superblock.active_root_pointer();
        new_root.seq = { new_root.seq }.saturating_add(1);
        new_root.lsn = next_lsn;
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

fn build_index_blob(
    engine: &Engine,
    blobs: &HashMap<u64, Vec<u8>>,
) -> Result<IndexBlob, EngineError> {
    let mut objects = Vec::with_capacity(engine.object_table.len());
    for (oid, rec) in engine.object_table.iter() {
        objects.push(ObjectRecordCbor {
            oid: *oid,
            bytes: rec.to_bytes().to_vec(),
        });
    }
    let mut locations = Vec::with_capacity(engine.location_table.len());
    for (oid, loc) in engine.location_table.iter() {
        // ObjectLocation has its own framing; serialise via its own helper.
        locations.push(LocationCbor {
            oid: *oid,
            bytes: loc.serialize(),
        });
    }
    // Forward index now uses direct ciborium derive (no crate-local helper).
    let mut forward_index_bytes = Vec::new();
    into_writer(&engine.forward_index, &mut forward_index_bytes)?;
    Ok(IndexBlob {
        forward_index_bytes,
        tag_index_bytes: engine.tag_index.serialise()?,
        range_index_bytes: engine.range_index.serialise()?,
        ontology_bytes: engine.ontology.serialise()?,
        subscriptions_bytes: engine.subscriptions.serialise()?,
        path_contexts_bytes: engine.path_contexts.serialise()?,
        oplog_bytes: engine.oplog.serialise()?,
        objects,
        locations,
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
    // Forward index uses direct ciborium derive (no helper).
    if let Ok(fwd) = from_reader::<ForwardIndex, _>(blob.forward_index_bytes.as_slice()) {
        engine.forward_index = fwd;
    }
    if let Ok(t) = TagIndex::deserialise(&blob.tag_index_bytes) {
        engine.tag_index = t;
    }
    if let Ok(r) = RangeIndex::deserialise(&blob.range_index_bytes) {
        engine.range_index = r;
    }
    if let Ok(ont) = OntologyState::deserialise(&blob.ontology_bytes) {
        engine.ontology = ont;
    }
    if let Ok(s) = SubscriptionEngine::deserialise(&blob.subscriptions_bytes) {
        engine.subscriptions = s;
    }
    if let Ok(p) = PathContextManager::deserialise(&blob.path_contexts_bytes) {
        engine.path_contexts = p;
    }
    if let Ok(ol) = OpLog::deserialise(&blob.oplog_bytes) {
        engine.oplog = ol;
    }
    // Object table.
    let mut object_table = ObjectTable::new();
    for o in blob.objects {
        if o.bytes.len() == mimisbrunnr_meta::OBJECT_RECORD_SIZE {
            let mut buf = [0u8; mimisbrunnr_meta::OBJECT_RECORD_SIZE];
            buf.copy_from_slice(&o.bytes);
            let rec = *mimisbrunnr_meta::ObjectRecord::ref_from_bytes(&buf);
            object_table.insert(rec);
        }
    }
    engine.object_table = object_table;
    // Location table.
    let mut location_table = LocationTable::new();
    for l in blob.locations {
        if let Ok((loc, _)) = mimisbrunnr_meta::ObjectLocation::parse(&l.bytes) {
            location_table.insert(l.oid, loc);
        }
    }
    engine.location_table = location_table;
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
    // next_tag_id: rolled forward via `register_tag` callers; persisted via
    // ontology so we don't need to do anything else here. The blob's
    // `next_tag_id` field exists for future use.
    let _ = blob.next_tag_id;
}

