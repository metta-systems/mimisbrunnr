//! [`DiskEngine`] — persistent wrapper around [`Engine`] (DESIGN §15).
//!
//! Glues the in-memory engine to:
//!
//! - the per-disk [`Wal`] (writes every mutation as a `WalOp` entry),
//! - the primary disk's [`Superblock`] (atomic root commit at checkpoint),
//! - a [`PoolManager`] (multi-disk lifecycle).
//!
//! Index persistence in this phase is a **single CBOR blob** in the index
//! zone, length-prefixed by a 4-byte little-endian header. The B+ tree
//! machinery lands in a later phase.
// TODO(rewrite-phase-N): replace CBOR blob with B+ trees per IMPL §1.5/§7/§8/§9.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use ciborium::{de::from_reader, ser::into_writer};
use log::trace;
use mimisbrunnr_index::{ChunkIndex, ForwardIndex, KvIndex, RangeIndex, TagIndex};
use mimisbrunnr_meta::{LocationTable, ObjectTable};
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
}

// ---------- Index-zone CBOR blob shape ----------

#[derive(Debug, Serialize, Deserialize)]
struct IndexBlob {
    forward_index_bytes: Vec<u8>,
    tag_index_bytes: Vec<u8>,
    kv_index_bytes: Vec<u8>,
    range_index_bytes: Vec<u8>,
    chunk_index_bytes: Vec<u8>,
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

        // 1. Restore index state from the index-zone CBOR blob (if any).
        let index_blob = read_index_blob(primary_dev.as_ref(), &superblock)?;
        if let Some(blob) = index_blob {
            apply_index_blob(&mut engine, blob);
        }

        let mut de = Self {
            engine,
            blobs: HashMap::new(),
            pool,
            primary_device: primary_dev,
            superblock,
            wal,
            config,
            config_path: config_path.to_path_buf(),
        };

        // 2. Re-load the blob map (we stuffed it into the index blob too).
        if let Some(blob) = read_index_blob(de.primary_device.as_ref(), &de.superblock)? {
            de.blobs = blob.blobs.clone();
        }

        // 3. Replay WAL entries past the last applied LSN.
        de.replay_wal()?;
        Ok(de)
    }

    // -------------------------------------------------------------------
    // Persistence: index zone (CBOR blob) and atomic root commit.
    // -------------------------------------------------------------------

    /// Save the engine's index state to the index zone as a length-prefixed
    /// CBOR blob.
    pub fn save_index_state(&mut self) -> Result<(), EngineError> {
        let blob = build_index_blob(&self.engine, &self.blobs)?;
        let mut payload = Vec::new();
        into_writer(&blob, &mut payload)?;

        let zone_offset = { self.superblock.index_zone.offset };
        let zone_length = { self.superblock.index_zone.length };

        // Frame: [magic u32 le | length u32 le | cbor bytes ...]
        let total = 4 + 4 + payload.len() as u64;
        if total > zone_length {
            return Err(EngineError::NotImplemented(
                "index zone too small for CBOR blob — TODO(rewrite-phase-N): spill via §1.5 B+ trees",
            ));
        }
        let mut framed = Vec::with_capacity(total as usize);
        framed.extend_from_slice(&INDEX_BLOB_MAGIC.to_le_bytes());
        framed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        framed.extend_from_slice(&payload);
        self.primary_device.write_at(zone_offset, &framed)?;
        self.primary_device.sync()?;
        Ok(())
    }

    /// Reload the index state from disk (overwrites in-memory state).
    pub fn load_index_state(&mut self) -> Result<(), EngineError> {
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
        let id = self.pool.add_disk(entry)?;
        // Keep the on-disk pool config in sync.
        self.config = self.pool.config().clone();
        self.config.save_toml(&self.config_path)?;
        Ok(id)
    }

    pub fn remove_disk(&mut self, id: DiskId) -> Result<(), EngineError> {
        self.pool.remove_disk(id)?;
        self.config = self.pool.config().clone();
        self.config.save_toml(&self.config_path)?;
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
        let oid = self.engine.create_object();
        self.append_wal(&OpKind::CreateObject { oid })?;
        Ok(oid)
    }

    pub fn delete_object(&mut self, oid: ObjectId) -> Result<(), EngineError> {
        self.engine.delete_object(oid)?;
        self.append_wal(&OpKind::DeleteObject { oid })?;
        Ok(())
    }

    pub fn add_tag(&mut self, oid: ObjectId, tag: TagId) -> Result<(), EngineError> {
        self.engine.add_tag(oid, tag)?;
        self.append_wal(&OpKind::AddTag { oid, tag })?;
        Ok(())
    }

    pub fn remove_tag(&mut self, oid: ObjectId, tag: TagId) -> Result<(), EngineError> {
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
        let res = self.engine.write_blob(oid, plaintext)?;
        self.blobs.insert(oid.local_seq(), res.data.clone());
        self.append_wal(&OpKind::WriteBlob {
            oid,
            content_hash: res.content_hash,
            size: res.original_size,
        })?;
        Ok(res)
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
        self.engine.subscribe(name, query, interest, retention)
    }

    /// Install an ontology module; no WAL entry. The module ends up in the
    /// index-zone CBOR blob at the next `commit`.
    pub fn install_ontology_module(
        &mut self,
        module: OntologyModule,
    ) -> Result<InstallResult, EngineError> {
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
        let mut bytes = Vec::new();
        loc.serialize_into(&mut bytes);
        locations.push(LocationCbor { oid: *oid, bytes });
    }
    Ok(IndexBlob {
        forward_index_bytes: engine.forward_index.serialise()?,
        tag_index_bytes: engine.tag_index.serialise()?,
        kv_index_bytes: engine.kv_index.serialise()?,
        range_index_bytes: engine.range_index.serialise()?,
        chunk_index_bytes: engine.chunk_index.serialise()?,
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
    if zone_length < 8 {
        return Ok(None);
    }
    let mut header = [0u8; 8];
    device.read_at(zone_offset, &mut header)?;
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
    if payload_len + 8 > zone_length {
        return Ok(None);
    }
    let mut payload = vec![0u8; payload_len as usize];
    device.read_at(zone_offset + 8, &mut payload)?;
    let blob: IndexBlob = from_reader(payload.as_slice())?;
    Ok(Some(blob))
}

fn apply_index_blob(engine: &mut Engine, blob: IndexBlob) {
    if let Ok(fwd) = ForwardIndex::deserialise(&blob.forward_index_bytes) {
        engine.forward_index = fwd;
    }
    if let Ok(t) = TagIndex::deserialise(&blob.tag_index_bytes) {
        engine.tag_index = t;
    }
    if let Ok(kv) = KvIndex::deserialise(&blob.kv_index_bytes) {
        engine.kv_index = kv;
    }
    if let Ok(r) = RangeIndex::deserialise(&blob.range_index_bytes) {
        engine.range_index = r;
    }
    if let Ok(c) = ChunkIndex::deserialise(&blob.chunk_index_bytes) {
        engine.chunk_index = c;
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
        // We can't poke private fields — use replay_create_object on a
        // sentinel local to bump the counter forward.
        if next_local > engine.next_oid_local() {
            // Allocate a "ghost" oid one less than next_local to advance the
            // counter to next_local exactly. This is a deliberate
            // workaround for not having a setter on `next_oid_local`.
            let ghost_local = next_local.saturating_sub(1);
            let ghost = ObjectId::from_parts(engine.node_id, ghost_local);
            let _ = engine.replay_create_object(ghost, 0, 0);
        }
    }
    engine.last_applied_lsn = engine.last_applied_lsn.max(blob.last_applied_lsn);
    // next_tag_id: rolled forward via `register_tag` callers; persisted via
    // ontology so we don't need to do anything else here. The blob's
    // `next_tag_id` field exists for future use.
    let _ = blob.next_tag_id;
}

