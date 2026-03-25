use std::path::Path;

use {
    mimisbrunnr_meta::ObjectTable,
    mimisbrunnr_pool::{PlacementRule, PoolConfig, parse_compression_algo},
    mimisbrunnr_query::QueryParser,
    mimisbrunnr_storage::{
        BlockDevice, ExtentLayout, FileBlockDevice, Superblock, ZoneExtent, ZoneMap, ZoneType,
        BLOCK_SIZE,
    },
};

use crate::{engine::Engine, error::EngineError};
use log::trace;

/// Round up `value` to the next multiple of `align`.
fn align_up(value: u64, align: u64) -> u64 {
    value.div_ceil(align) * align
}

/// Persistent state serialized to the index zone.
///
/// Stores the forward index entries and ontology tag definitions as JSON
/// at the start of the index zone. This is a simple approach for correctness;
/// a production system would use a more compact binary format.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct IndexState {
    /// Forward index: object_id_raw → vec of (assertion_json, origin)
    forward: Vec<ForwardRecord>,
    /// Tag definitions: id → (name, semantics_json, implies)
    tags: Vec<TagRecord>,
    /// Implications: (from, to)
    implications: Vec<(u32, u32)>,
    /// Path contexts: context_name → entries
    #[serde(default)]
    path_contexts: Vec<PathContextRecord>,
    /// Blob data stored inline (for small blobs; production would use blob zone)
    #[serde(default)]
    blobs: Vec<BlobRecord>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PathContextRecord {
    #[serde(default)]
    name: Option<String>,
    entries: Vec<ProjectionEntryRecord>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ProjectionEntryRecord {
    object_id: Option<u64>,
    path: String,
    entry_type: EntryTypeRecord,
}

#[derive(serde::Serialize, serde::Deserialize)]
enum EntryTypeRecord {
    File { mode: u32, uid: u32, gid: u32 },
    Symlink { target: String },
    Directory { mode: u32 },
}

#[derive(serde::Serialize, serde::Deserialize)]
struct BlobRecord {
    object_id: u64,
    data: Vec<u8>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ForwardRecord {
    object_id: u64,
    tag_ids_direct: Vec<u32>,
    tag_ids_materialized: Vec<u32>,
    attrs: Vec<AttrRecord>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct AttrRecord {
    key: u32,
    value: ValueRecord,
}

#[derive(serde::Serialize, serde::Deserialize)]
enum ValueRecord {
    Text(String),
    Int(i64),
    Float(f64),
    Timestamp(i64),
    Blob(Vec<u8>),
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TagRecord {
    id: u32,
    name: String,
    semantics: String,
}

/// Disk-backed engine that persists state across invocations.
///
/// Opens pool disks, loads the object table from the metadata zone and
/// the index state from the index zone. Changes are flushed back on `flush()`.
pub struct DiskEngine {
    /// The in-memory engine with all indexes loaded.
    pub engine: Engine,
    /// Path context manager (persisted).
    pub context_mgr: mimisbrunnr_types::PathContextManager,
    /// Blob data store: object_id_raw → data.
    pub blobs: std::collections::HashMap<u64, Vec<u8>>,
    /// Primary disk device (holds index + metadata zones).
    primary_device: FileBlockDevice,
    /// Superblock from the primary disk.
    superblock: Superblock,
    /// Pool config (so we know about all disks).
    #[allow(dead_code)]
    config: PoolConfig,
    /// Path to the pool config file.
    #[allow(dead_code)]
    config_path: std::path::PathBuf,
}

impl DiskEngine {
    /// Open an existing pool from its config file path.
    pub fn open(config_path: &Path) -> Result<Self, EngineError> {
        trace!("DiskEngine::open config={}", config_path.display());
        let config = PoolConfig::load(config_path)
            .map_err(|e| EngineError::Io(std::io::Error::other(e.to_string())))?;

        let primary = config.primary_disk().ok_or(EngineError::NotInitialized)?;

        let primary_device =
            FileBlockDevice::open(Path::new(&primary.path), 0).map_err(EngineError::Storage)?;

        // Read superblock with full extent layout (handles ZoneMap if present)
        let superblock =
            Superblock::read_with_extents(&primary_device).map_err(EngineError::Storage)?;

        let mut engine = Engine::new(config.node_id as u64);

        // Load object table from metadata zone
        let layout = &superblock.layout;
        engine.object_table = ObjectTable::load(
            &primary_device,
            layout.metadata_zone_offset(),
            layout.metadata_zone_size(),
        )
        .map_err(EngineError::Meta)?;

        // Load index state from index zone
        let mut context_mgr = mimisbrunnr_types::PathContextManager::new();
        let mut blobs = std::collections::HashMap::new();
        Self::load_index_state(
            &primary_device,
            layout,
            ZoneType::Index,
            &mut engine,
            &mut context_mgr,
            &mut blobs,
        )?;

        // Set default compression from config
        engine.set_default_compression(config.default_compression_algo());

        // Resolve placement rules from config (needs ontology to be loaded first)
        Self::resolve_config_rules(&config, &mut engine);

        trace!(
            "DiskEngine::open loaded {} objects, {} rules",
            engine.object_table.count(),
            engine.rules().len(),
        );

        Ok(Self {
            engine,
            context_mgr,
            blobs,
            primary_device,
            superblock,
            config,
            config_path: config_path.to_path_buf(),
        })
    }

    /// Open a pool by finding the config from any disk in the pool.
    pub fn open_from_disk(disk_path: &Path) -> Result<Self, EngineError> {
        let (config_path, _) = PoolConfig::find_from_disk(disk_path)
            .map_err(|e| EngineError::Io(std::io::Error::other(e.to_string())))?;
        Self::open(&config_path)
    }

    /// Flush all in-memory state to disk.
    ///
    /// If the index state doesn't fit in the current index zone, grows
    /// the index zone by adding a new extent carved from the end of the blob zone.
    /// The updated extents are persisted via ZoneMap.
    pub fn flush(&mut self) -> Result<(), EngineError> {
        trace!("DiskEngine::flush");

        // Flush object table to metadata zone
        self.engine
            .object_table
            .flush_all(&self.primary_device)
            .map_err(EngineError::Meta)?;

        // Serialize index state and check if it fits
        let json =
            Self::serialize_index_state(&self.engine, &self.context_mgr, &self.blobs)?;

        let needed = json.len() as u64 + 8; // 8 bytes for length prefix
        let index_size = self.superblock.layout.zone_size(ZoneType::Index);

        if needed > index_size {
            self.grow_index_zone(needed, index_size)?;
        }

        // Write index state using extent-aware writer
        Self::write_index_state(&self.primary_device, &self.superblock.layout, &json)?;

        self.primary_device.sync().map_err(EngineError::Storage)?;
        trace!("DiskEngine::flush complete");
        Ok(())
    }

    /// Grow the index zone by stealing blocks from the end of the blob zone.
    ///
    /// Adds a new extent to the index zone and shrinks the last blob extent.
    /// Persists the updated layout via ZoneMap and superblock.
    fn grow_index_zone(&mut self, needed: u64, current_size: u64) -> Result<(), EngineError> {
        let grow_by = align_up(needed - current_size, BLOCK_SIZE);
        let blob_size = self.superblock.layout.zone_size(ZoneType::Blob);

        if grow_by > blob_size / 2 {
            return Err(EngineError::Io(std::io::Error::other(format!(
                "index state too large ({needed} bytes), cannot grow index zone \
                 without consuming more than half the blob zone",
            ))));
        }

        // Steal from the end of the last blob extent
        let blob_extents = self.superblock.layout.extents_mut(ZoneType::Blob);
        let last_blob = blob_extents
            .last_mut()
            .ok_or_else(|| EngineError::Io(std::io::Error::other("no blob extents")))?;

        if grow_by > last_blob.size {
            return Err(EngineError::Io(std::io::Error::other(format!(
                "cannot grow index zone by {} bytes: last blob extent is only {} bytes",
                grow_by, last_blob.size,
            ))));
        }

        // New index extent starts where the blob extent now ends
        let new_extent_offset = last_blob.offset + last_blob.size - grow_by;
        last_blob.size -= grow_by;

        // Add the new extent to the index zone
        let new_extent = ZoneExtent::new(new_extent_offset, grow_by);
        self.superblock
            .layout
            .extents_mut(ZoneType::Index)
            .push(new_extent);

        // Write ZoneMap block. Use the block just before the backup superblock
        // (or reuse existing zone_map_offset).
        let zone_map_offset = if self.superblock.zone_map_offset != 0 {
            self.superblock.zone_map_offset
        } else {
            // Allocate zone map block from the end of the blob zone
            let blob_extents = self.superblock.layout.extents_mut(ZoneType::Blob);
            let last_blob = blob_extents.last_mut().unwrap();
            let offset = last_blob.offset + last_blob.size - BLOCK_SIZE;
            last_blob.size -= BLOCK_SIZE;
            offset
        };

        ZoneMap::write_to(&self.superblock.layout, &self.primary_device, zone_map_offset)
            .map_err(EngineError::Storage)?;

        self.superblock.zone_map_offset = zone_map_offset;

        // Write updated superblock (with first extents inline + zone_map_offset)
        self.superblock
            .write_to(&self.primary_device)
            .map_err(EngineError::Storage)?;

        let new_index_size = self.superblock.layout.zone_size(ZoneType::Index);
        let new_blob_size = self.superblock.layout.zone_size(ZoneType::Blob);
        log::info!(
            "grew index zone by {} KiB (now {} KiB across {} extent(s), blob zone now {} KiB)",
            grow_by / 1024,
            new_index_size / 1024,
            self.superblock.layout.extents(ZoneType::Index).len(),
            new_blob_size / 1024,
        );

        Ok(())
    }

    /// Resolve string-based rules from PoolConfig into PlacementRules.
    ///
    /// Must be called after the ontology is loaded so tag name lookups work.
    fn resolve_config_rules(config: &PoolConfig, engine: &mut Engine) {
        // Parse all rules first to avoid borrowing engine.dag and engine mutably at the same time.
        let parsed_rules: Vec<PlacementRule> = {
            let parser = QueryParser::new(&engine.dag);
            let mut rules = Vec::new();
            for rule_config in &config.rules {
                match rule_config.rule_type.as_str() {
                    "compress" => {
                        let query_str = match &rule_config.query {
                            Some(q) => q,
                            None => {
                                log::warn!("compress rule missing query, skipping");
                                continue;
                            }
                        };
                        let algo_str = rule_config.algo.as_deref().unwrap_or("zstd:3");
                        let algo = match parse_compression_algo(algo_str) {
                            Some(a) => a,
                            None => {
                                log::warn!("unknown compression algo '{algo_str}', skipping rule");
                                continue;
                            }
                        };
                        match parser.parse(query_str) {
                            Ok(query) => {
                                rules.push(PlacementRule::Compress { query, algo });
                            }
                            Err(e) => {
                                log::warn!(
                                    "failed to parse rule query '{query_str}': {e}, skipping"
                                );
                            }
                        }
                    }
                    other => {
                        log::warn!("unsupported rule type '{other}', skipping");
                    }
                }
            }
            rules
        };

        for rule in parsed_rules {
            engine.add_rule(rule);
        }
    }

    /// Access the underlying engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Access the underlying engine mutably.
    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    /// Store blob data for an object.
    pub fn store_blob(&mut self, object_id: u64, data: Vec<u8>) {
        self.blobs.insert(object_id, data);
    }

    /// Retrieve blob data for an object.
    pub fn get_blob(&self, object_id: u64) -> Option<&[u8]> {
        self.blobs.get(&object_id).map(|v| v.as_slice())
    }

    /// Serialize engine state to JSON bytes.
    fn serialize_index_state(
        engine: &Engine,
        context_mgr: &mimisbrunnr_types::PathContextManager,
        blobs: &std::collections::HashMap<u64, Vec<u8>>,
    ) -> Result<Vec<u8>, EngineError> {
        let mut state = IndexState::default();

        // Serialize ontology tags
        for tag_id in engine.dag.all_tags() {
            if let Some(def) = engine.dag.get(tag_id) {
                state.tags.push(TagRecord {
                    id: tag_id.raw(),
                    name: def.name.clone(),
                    semantics: format!("{:?}", def.semantics),
                });
            }
        }

        // Serialize implications
        for tag_id in engine.dag.all_tags() {
            for &implied in engine.dag.direct_implies(tag_id) {
                state.implications.push((tag_id.raw(), implied.raw()));
            }
        }

        // Serialize forward index
        for rec in engine.object_table.iter() {
            let oid = mimisbrunnr_types::ObjectId::new(rec.id >> 48, rec.id & 0x0000_FFFF_FFFF_FFFF);
            let entries = engine.forward_index.get(oid);
            if entries.is_empty() {
                continue;
            }

            let mut fwd = ForwardRecord {
                object_id: rec.id,
                tag_ids_direct: Vec::new(),
                tag_ids_materialized: Vec::new(),
                attrs: Vec::new(),
            };

            for entry in entries {
                match &entry.assertion {
                    mimisbrunnr_types::Assertion::Tag(id) => {
                        if entry.origin == mimisbrunnr_types::TagOrigin::Direct {
                            fwd.tag_ids_direct.push(id.raw());
                        } else {
                            fwd.tag_ids_materialized.push(id.raw());
                        }
                    }
                    mimisbrunnr_types::Assertion::Attr { key, value } => {
                        fwd.attrs.push(AttrRecord {
                            key: key.raw(),
                            value: value_to_record(value),
                        });
                    }
                    mimisbrunnr_types::Assertion::Relation { .. } => {
                        // TODO: serialize relations
                    }
                }
            }

            state.forward.push(fwd);
        }

        // Serialize path contexts
        let serialize_entries =
            |proj: &mimisbrunnr_types::PathProjection| -> Vec<ProjectionEntryRecord> {
                proj.entries
                    .iter()
                    .map(|e| ProjectionEntryRecord {
                        object_id: e.object.map(|o| (o.node() << 48) | o.local()),
                        path: e.path.clone(),
                        entry_type: match &e.entry_type {
                            mimisbrunnr_types::ProjectedEntryType::File { mode, uid, gid } => {
                                EntryTypeRecord::File {
                                    mode: *mode,
                                    uid: *uid,
                                    gid: *gid,
                                }
                            }
                            mimisbrunnr_types::ProjectedEntryType::Symlink { target } => {
                                EntryTypeRecord::Symlink {
                                    target: target.clone(),
                                }
                            }
                            mimisbrunnr_types::ProjectedEntryType::Directory { mode } => {
                                EntryTypeRecord::Directory { mode: *mode }
                            }
                        },
                    })
                    .collect()
            };

        // Serialize unscoped projection
        let unscoped = context_mgr.unscoped();
        if !unscoped.is_empty() {
            state.path_contexts.push(PathContextRecord {
                name: None,
                entries: serialize_entries(unscoped),
            });
        }

        // Serialize named contexts
        for ctx_name in context_mgr.list_contexts() {
            if let Ok(proj) = context_mgr.get_context(ctx_name) {
                state.path_contexts.push(PathContextRecord {
                    name: Some(ctx_name.to_string()),
                    entries: serialize_entries(proj),
                });
            }
        }

        // Serialize blobs
        for (&oid_raw, data) in blobs {
            state.blobs.push(BlobRecord {
                object_id: oid_raw,
                data: data.clone(),
            });
        }

        let json = serde_json::to_vec(&state)
            .map_err(|e| EngineError::Io(std::io::Error::other(e.to_string())))?;
        trace!("DiskEngine::serialize_index_state json_len={}", json.len());

        Ok(json)
    }

    /// Write serialized index state to the index zone, spanning extents as needed.
    fn write_index_state(
        dev: &FileBlockDevice,
        layout: &ExtentLayout,
        json: &[u8],
    ) -> Result<(), EngineError> {
        let total_needed = json.len() as u64 + 8;
        let zone_size = layout.zone_size(ZoneType::Index);
        if total_needed > zone_size {
            return Err(EngineError::Io(std::io::Error::other(format!(
                "index state too large: {} bytes, zone is {} bytes",
                json.len(),
                zone_size
            ))));
        }

        // Write length prefix + json data across extents
        let mut data = Vec::with_capacity(total_needed as usize);
        data.extend_from_slice(&(json.len() as u64).to_le_bytes());
        data.extend_from_slice(json);

        let mut remaining = &data[..];
        for extent in layout.extents(ZoneType::Index) {
            if remaining.is_empty() {
                break;
            }
            let chunk_size = remaining.len().min(extent.size as usize);
            dev.write_at(extent.offset, &remaining[..chunk_size])
                .map_err(EngineError::Storage)?;
            remaining = &remaining[chunk_size..];
        }

        Ok(())
    }

    /// Load index state from the index zone and rebuild in-memory indexes.
    fn load_index_state(
        dev: &FileBlockDevice,
        layout: &ExtentLayout,
        zone: ZoneType,
        engine: &mut Engine,
        context_mgr: &mut mimisbrunnr_types::PathContextManager,
        blobs: &mut std::collections::HashMap<u64, Vec<u8>>,
    ) -> Result<(), EngineError> {
        let zone_size = layout.zone_size(zone);

        // Read length prefix from first extent
        let first_offset = layout.zone_offset(zone);
        let mut len_buf = [0u8; 8];
        dev.read_at(first_offset, &mut len_buf)
            .map_err(EngineError::Storage)?;
        let json_len = u64::from_le_bytes(len_buf);
        trace!("DiskEngine::load_index_state json_len={json_len} zone_size={zone_size}");

        if json_len == 0 || json_len > zone_size - 8 {
            // Empty or invalid — fresh pool, nothing to load
            return Ok(());
        }

        // Read json data across extents
        let mut json_buf = vec![0u8; json_len as usize];
        let mut bytes_read = 0usize;
        let mut skip = 8u64; // skip length prefix

        for extent in layout.extents(zone) {
            if bytes_read >= json_buf.len() {
                break;
            }
            if skip >= extent.size {
                skip -= extent.size;
                continue;
            }
            let read_offset = extent.offset + skip;
            let available = (extent.size - skip) as usize;
            let to_read = available.min(json_buf.len() - bytes_read);
            dev.read_at(read_offset, &mut json_buf[bytes_read..bytes_read + to_read])
                .map_err(EngineError::Storage)?;
            bytes_read += to_read;
            skip = 0;
        }

        let state: IndexState = match serde_json::from_slice(&json_buf) {
            Ok(s) => s,
            Err(_) => return Ok(()), // Corrupt or empty, start fresh
        };

        // Rebuild ontology
        for tag_rec in &state.tags {
            let sem = parse_semantics(&tag_rec.semantics);
            let def = mimisbrunnr_ontology::TagDefinition::new(
                mimisbrunnr_types::TagId::new(tag_rec.id),
                &tag_rec.name,
                sem,
            );
            let _ = engine.dag.register_tag(def);
        }

        for &(from, to) in &state.implications {
            let _ = engine.dag.add_implication(
                mimisbrunnr_types::TagId::new(from),
                mimisbrunnr_types::TagId::new(to),
            );
        }

        // Rebuild forward index, tag index, and kv index from forward records
        for fwd in &state.forward {
            let oid = mimisbrunnr_types::ObjectId::new(fwd.object_id >> 48, fwd.object_id & 0x0000_FFFF_FFFF_FFFF);
            let obj_local = oid.local() as u32;

            for &tag_raw in &fwd.tag_ids_direct {
                let tag = mimisbrunnr_types::TagId::new(tag_raw);
                engine.tag_index.tag_object(tag, obj_local);
                engine.forward_index.add(
                    oid,
                    mimisbrunnr_types::Assertion::Tag(tag),
                    mimisbrunnr_types::TagOrigin::Direct,
                );
            }

            for &tag_raw in &fwd.tag_ids_materialized {
                let tag = mimisbrunnr_types::TagId::new(tag_raw);
                engine.tag_index.tag_object(tag, obj_local);
                engine.forward_index.add(
                    oid,
                    mimisbrunnr_types::Assertion::Tag(tag),
                    mimisbrunnr_types::TagOrigin::Materialized,
                );
            }

            for attr in &fwd.attrs {
                let key = mimisbrunnr_types::TagId::new(attr.key);
                let value = record_to_value(&attr.value);
                engine.kv_index.insert(key, &value, obj_local);
                engine.forward_index.add(
                    oid,
                    mimisbrunnr_types::Assertion::Attr { key, value },
                    mimisbrunnr_types::TagOrigin::Direct,
                );
            }
        }

        // Rebuild path contexts
        let deserialize_entry =
            |entry_rec: &ProjectionEntryRecord| -> mimisbrunnr_types::ProjectedEntry {
                let object = entry_rec
                    .object_id
                    .map(|raw| mimisbrunnr_types::ObjectId::new(raw >> 48, raw & 0x0000_FFFF_FFFF_FFFF));
                let entry_type = match &entry_rec.entry_type {
                    EntryTypeRecord::File { mode, uid, gid } => {
                        mimisbrunnr_types::ProjectedEntryType::File {
                            mode: *mode,
                            uid: *uid,
                            gid: *gid,
                        }
                    }
                    EntryTypeRecord::Symlink { target } => {
                        mimisbrunnr_types::ProjectedEntryType::Symlink {
                            target: target.clone(),
                        }
                    }
                    EntryTypeRecord::Directory { mode } => {
                        mimisbrunnr_types::ProjectedEntryType::Directory { mode: *mode }
                    }
                };
                mimisbrunnr_types::ProjectedEntry {
                    object,
                    path: entry_rec.path.clone(),
                    entry_type,
                }
            };

        for ctx_rec in &state.path_contexts {
            match &ctx_rec.name {
                Some(name) => {
                    let _ = context_mgr.create_context(name);
                    for entry_rec in &ctx_rec.entries {
                        let entry = deserialize_entry(entry_rec);
                        let oid =
                            entry.object.unwrap_or(mimisbrunnr_types::ObjectId::new(0, 0));
                        let _ = context_mgr.set_path(name, oid, &entry_rec.path, entry);
                    }
                }
                None => {
                    for entry_rec in &ctx_rec.entries {
                        let entry = deserialize_entry(entry_rec);
                        let oid =
                            entry.object.unwrap_or(mimisbrunnr_types::ObjectId::new(0, 0));
                        context_mgr.set_unscoped_path(oid, &entry_rec.path, entry);
                    }
                }
            }
        }

        // Rebuild blob store
        for blob_rec in &state.blobs {
            blobs.insert(blob_rec.object_id, blob_rec.data.clone());
        }

        Ok(())
    }
}

fn value_to_record(v: &mimisbrunnr_types::Value) -> ValueRecord {
    match v {
        mimisbrunnr_types::Value::Text(s) => ValueRecord::Text(s.clone()),
        mimisbrunnr_types::Value::Int(n) => ValueRecord::Int(*n),
        mimisbrunnr_types::Value::Float(f) => ValueRecord::Float(*f),
        mimisbrunnr_types::Value::Timestamp(t) => ValueRecord::Timestamp(*t),
        mimisbrunnr_types::Value::Blob(b) => ValueRecord::Blob(b.clone()),
    }
}

fn record_to_value(r: &ValueRecord) -> mimisbrunnr_types::Value {
    match r {
        ValueRecord::Text(s) => mimisbrunnr_types::Value::Text(s.clone()),
        ValueRecord::Int(n) => mimisbrunnr_types::Value::Int(*n),
        ValueRecord::Float(f) => mimisbrunnr_types::Value::Float(*f),
        ValueRecord::Timestamp(t) => mimisbrunnr_types::Value::Timestamp(*t),
        ValueRecord::Blob(b) => mimisbrunnr_types::Value::Blob(b.clone()),
    }
}

fn parse_semantics(s: &str) -> mimisbrunnr_ontology::TagSemantics {
    // Simple parser for Debug format strings
    if s == "Label" {
        mimisbrunnr_ontology::TagSemantics::Label
    } else if s.starts_with("Attribute") {
        // Default to text for now
        mimisbrunnr_ontology::TagSemantics::Attribute {
            value_type: mimisbrunnr_ontology::ValueType::Text,
        }
    } else if s == "Grouping" {
        mimisbrunnr_ontology::TagSemantics::Grouping
    } else if s.starts_with("OrderedCollection") {
        mimisbrunnr_ontology::TagSemantics::OrderedCollection {
            element_constraint: None,
        }
    } else if s == "Hierarchical" {
        mimisbrunnr_ontology::TagSemantics::Hierarchical
    } else {
        mimisbrunnr_ontology::TagSemantics::Label
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        mimisbrunnr_ontology::{TagDefinition, TagSemantics},
        mimisbrunnr_storage::WAL_SIZE,
        mimisbrunnr_types::{ObjectId, Query, TagId, Value},
        mimisbrunnr_wal::WriteAheadLog,
        tempfile::TempDir,
    };

    fn create_test_pool(dir: &Path) -> std::path::PathBuf {
        let disk_path = dir.join("disk0.mbrunnr");
        let config_path = dir.join("pool.toml");

        let capacity = 128 * 1024 * 1024u64;
        let dev = FileBlockDevice::open(&disk_path, capacity).unwrap();

        let layout = ExtentLayout::compute(capacity).unwrap();
        let sb = Superblock::new(0, 0, layout.clone());
        sb.write_to(&dev).unwrap();

        WriteAheadLog::create(&dev, layout.wal_offset, WAL_SIZE).unwrap();

        let mut config = PoolConfig::new(0);
        config.add_disk(0, disk_path.to_str().unwrap(), "warm", capacity);
        config.save(&config_path).unwrap();

        config_path
    }

    #[test]
    fn open_fresh_pool() {
        let tmp = TempDir::new().unwrap();
        let config_path = create_test_pool(tmp.path());

        let de = DiskEngine::open(&config_path).unwrap();
        assert_eq!(de.engine().object_table.count(), 0);
    }

    #[test]
    fn create_objects_and_flush() {
        let tmp = TempDir::new().unwrap();
        let config_path = create_test_pool(tmp.path());

        // Create objects and tags
        {
            let mut de = DiskEngine::open(&config_path).unwrap();
            let e = de.engine_mut();

            e.register_tag(TagDefinition::new(
                TagId::new(1),
                "electronic",
                TagSemantics::Label,
            ))
            .unwrap();
            e.register_tag(TagDefinition::new(
                TagId::new(2),
                "portable",
                TagSemantics::Label,
            ))
            .unwrap();

            let oid1 = e.create_object(1000).unwrap();
            let oid2 = e.create_object(1000).unwrap();

            e.add_tag(oid1, TagId::new(1), 1000).unwrap();
            e.add_tag(oid1, TagId::new(2), 1000).unwrap();
            e.add_tag(oid2, TagId::new(1), 1000).unwrap();

            de.flush().unwrap();
        }

        // Reopen and verify state survived
        {
            let de = DiskEngine::open(&config_path).unwrap();
            let e = de.engine();

            assert_eq!(e.object_table.count(), 2);

            // Ontology should be restored
            assert!(e.dag.lookup("electronic").is_some());
            assert!(e.dag.lookup("portable").is_some());

            // Tags should be restored
            let results = e.query(&Query::HasTag(TagId::new(1)));
            assert_eq!(results.len(), 2);

            let results = e.query(&Query::And(vec![
                Query::HasTag(TagId::new(1)),
                Query::HasTag(TagId::new(2)),
            ]));
            assert_eq!(results.len(), 1);
        }
    }

    #[test]
    fn attrs_survive_flush() {
        let tmp = TempDir::new().unwrap();
        let config_path = create_test_pool(tmp.path());

        {
            let mut de = DiskEngine::open(&config_path).unwrap();
            let e = de.engine_mut();

            e.register_tag(TagDefinition::new(
                TagId::new(10),
                "artist",
                TagSemantics::Attribute {
                    value_type: mimisbrunnr_ontology::ValueType::Text,
                },
            ))
            .unwrap();

            let oid = e.create_object(1000).unwrap();
            e.set_attr(oid, TagId::new(10), Value::Text("Aphex Twin".into()), 1000)
                .unwrap();

            de.flush().unwrap();
        }

        {
            let de = DiskEngine::open(&config_path).unwrap();
            let e = de.engine();

            let results = e.query(&Query::HasAttr {
                key: TagId::new(10),
                op: mimisbrunnr_types::CmpOp::Eq,
                value: Value::Text("Aphex Twin".into()),
            });
            assert_eq!(results.len(), 1);
        }
    }

    #[test]
    fn implications_survive_flush() {
        let tmp = TempDir::new().unwrap();
        let config_path = create_test_pool(tmp.path());

        {
            let mut de = DiskEngine::open(&config_path).unwrap();
            let e = de.engine_mut();

            e.register_tag(TagDefinition::new(
                TagId::new(1),
                "car",
                TagSemantics::Label,
            ))
            .unwrap();
            e.register_tag(TagDefinition::new(
                TagId::new(2),
                "vehicle",
                TagSemantics::Label,
            ))
            .unwrap();
            e.add_implication(TagId::new(1), TagId::new(2)).unwrap();

            let oid = e.create_object(1000).unwrap();
            e.add_tag(oid, TagId::new(1), 1000).unwrap(); // car → vehicle

            de.flush().unwrap();
        }

        {
            let de = DiskEngine::open(&config_path).unwrap();
            let e = de.engine();

            // Both direct and materialized tags should be present
            let results = e.query(&Query::HasTag(TagId::new(2)));
            assert_eq!(results.len(), 1); // vehicle via materialization
        }
    }

    #[test]
    fn blob_data_survives_flush() {
        let tmp = TempDir::new().unwrap();
        let config_path = create_test_pool(tmp.path());

        let hash;
        {
            let mut de = DiskEngine::open(&config_path).unwrap();
            let e = de.engine_mut();

            let oid = e.create_object(1000).unwrap();
            hash = e.write_blob(oid, b"hello world", 1000).unwrap();

            // Store plaintext blob for retrieval
            de.store_blob((oid.node() << 48) | oid.local(), b"hello world".to_vec());

            de.flush().unwrap();
        }

        {
            let de = DiskEngine::open(&config_path).unwrap();
            let e = de.engine();

            let oid = ObjectId::new(0, 0);
            let rec = e.get_object(oid).unwrap();
            assert_eq!(rec.content_hash, hash);
            assert_eq!(rec.blob_length, 11);

            // Verify actual blob content is retrievable after reload
            let blob = de.get_blob((oid.node() << 48) | oid.local());
            assert!(blob.is_some(), "blob data should survive flush/reload");
            assert_eq!(blob.unwrap(), b"hello world");
        }
    }

    #[test]
    fn index_zone_grows_when_data_exceeds_initial_size() {
        let tmp = TempDir::new().unwrap();
        let config_path = create_test_pool(tmp.path());

        // Read original index zone size
        let original_index_size;
        {
            let de = DiskEngine::open(&config_path).unwrap();
            original_index_size = de.superblock.layout.zone_size(ZoneType::Index);
        }

        // Store enough blob data to exceed the index zone
        {
            let mut de = DiskEngine::open(&config_path).unwrap();

            // Each blob stored as JSON expands ~4x (byte array [0,1,2,...,255,...])
            // so 1 MB of blobs becomes ~4 MB of JSON, exceeding the ~3.8 MiB index zone
            for i in 0..5u64 {
                let content = vec![(i as u8).wrapping_mul(37); 200 * 1024]; // 200 KB each = 1 MB total
                let oid = {
                    let e = de.engine_mut();
                    let oid = e.create_object(1000).unwrap();
                    e.write_blob(oid, &content, 1000).unwrap();
                    oid
                };
                de.store_blob((oid.node() << 48) | oid.local(), content);
            }

            // This should succeed by growing the index zone
            de.flush().unwrap();

            // Verify the zone grew (now has multiple extents)
            let new_index_size = de.superblock.layout.zone_size(ZoneType::Index);
            assert!(
                new_index_size > original_index_size,
                "index zone should have grown: was {}, now {}",
                original_index_size,
                new_index_size,
            );
            assert!(
                de.superblock.layout.extents(ZoneType::Index).len() > 1,
                "index zone should have multiple extents after growth"
            );
            assert!(
                de.superblock.zone_map_offset != 0,
                "zone_map_offset should be set after growth"
            );
        }

        // Verify data survives reload
        {
            let de = DiskEngine::open(&config_path).unwrap();
            // Verify zone map was loaded (multiple extents)
            assert!(
                de.superblock.layout.extents(ZoneType::Index).len() > 1,
                "after reload, index zone should still have multiple extents"
            );
            for i in 0..5u64 {
                let blob = de
                    .get_blob(i)
                    .unwrap_or_else(|| panic!("blob {i} should exist"));
                assert_eq!(blob.len(), 200 * 1024);
                assert!(blob.iter().all(|&b| b == (i as u8).wrapping_mul(37)));
            }
        }
    }

    #[test]
    fn open_from_disk_path() {
        let tmp = TempDir::new().unwrap();
        let _config_path = create_test_pool(tmp.path());
        let disk_path = tmp.path().join("disk0.mbrunnr");

        let de = DiskEngine::open_from_disk(&disk_path).unwrap();
        assert_eq!(de.engine().object_table.count(), 0);
    }
}
