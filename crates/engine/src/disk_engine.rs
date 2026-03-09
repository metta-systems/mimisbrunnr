use std::path::Path;

use {
    mimisbrunnr_meta::ObjectTable,
    mimisbrunnr_pool::PoolConfig,
    mimisbrunnr_storage::{BlockDevice, FileBlockDevice, Superblock},
};

use crate::{engine::Engine, error::EngineError};

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
    name: String,
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
        let config = PoolConfig::load(config_path)
            .map_err(|e| EngineError::Io(std::io::Error::other(e.to_string())))?;

        let primary = config.primary_disk().ok_or(EngineError::NotInitialized)?;

        let primary_device =
            FileBlockDevice::open(Path::new(&primary.path), 0).map_err(EngineError::Storage)?;

        let superblock = Superblock::read_from(&primary_device).map_err(EngineError::Storage)?;

        let mut engine = Engine::new(config.node_id);

        // Load object table from metadata zone
        let layout = &superblock.layout;
        engine.object_table = ObjectTable::load(
            &primary_device,
            layout.metadata_zone_offset,
            layout.metadata_zone_size,
        )
        .map_err(EngineError::Meta)?;

        // Load index state from index zone
        let mut context_mgr = mimisbrunnr_types::PathContextManager::new();
        let mut blobs = std::collections::HashMap::new();
        Self::load_index_state(
            &primary_device,
            layout.index_zone_offset,
            layout.index_zone_size,
            &mut engine,
            &mut context_mgr,
            &mut blobs,
        )?;

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
    pub fn flush(&mut self) -> Result<(), EngineError> {
        let layout = &self.superblock.layout;

        // Flush object table to metadata zone
        self.engine
            .object_table
            .flush_all(&self.primary_device)
            .map_err(EngineError::Meta)?;

        // Flush index state to index zone
        Self::save_index_state(
            &self.primary_device,
            layout.index_zone_offset,
            layout.index_zone_size,
            &self.engine,
            &self.context_mgr,
            &self.blobs,
        )?;

        self.primary_device.sync().map_err(EngineError::Storage)?;
        Ok(())
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

    /// Save index state (forward index, ontology, contexts, blobs) to the index zone.
    fn save_index_state(
        dev: &FileBlockDevice,
        zone_offset: u64,
        zone_size: u64,
        engine: &Engine,
        context_mgr: &mimisbrunnr_types::PathContextManager,
        blobs: &std::collections::HashMap<u64, Vec<u8>>,
    ) -> Result<(), EngineError> {
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
            let oid = mimisbrunnr_types::ObjectId::from_raw(rec.id);
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
        for ctx_name in context_mgr.list_contexts() {
            if let Ok(proj) = context_mgr.get_context(ctx_name) {
                let entries = proj
                    .entries
                    .iter()
                    .map(|e| ProjectionEntryRecord {
                        object_id: e.object.map(|o| o.raw()),
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
                    .collect();
                state.path_contexts.push(PathContextRecord {
                    name: ctx_name.to_string(),
                    entries,
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

        if json.len() as u64 + 8 > zone_size {
            return Err(EngineError::Io(std::io::Error::other(format!(
                "index state too large: {} bytes, zone is {} bytes",
                json.len(),
                zone_size
            ))));
        }

        // Write: [length: u64][json bytes]
        let len_bytes = (json.len() as u64).to_le_bytes();
        dev.write_at(zone_offset, &len_bytes)
            .map_err(EngineError::Storage)?;
        dev.write_at(zone_offset + 8, &json)
            .map_err(EngineError::Storage)?;

        Ok(())
    }

    /// Load index state from the index zone and rebuild in-memory indexes.
    fn load_index_state(
        dev: &FileBlockDevice,
        zone_offset: u64,
        zone_size: u64,
        engine: &mut Engine,
        context_mgr: &mut mimisbrunnr_types::PathContextManager,
        blobs: &mut std::collections::HashMap<u64, Vec<u8>>,
    ) -> Result<(), EngineError> {
        // Read length
        let mut len_buf = [0u8; 8];
        dev.read_at(zone_offset, &mut len_buf)
            .map_err(EngineError::Storage)?;
        let json_len = u64::from_le_bytes(len_buf);

        if json_len == 0 || json_len > zone_size - 8 {
            // Empty or invalid — fresh pool, nothing to load
            return Ok(());
        }

        let mut json_buf = vec![0u8; json_len as usize];
        dev.read_at(zone_offset + 8, &mut json_buf)
            .map_err(EngineError::Storage)?;

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
            let oid = mimisbrunnr_types::ObjectId::from_raw(fwd.object_id);
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
        for ctx_rec in &state.path_contexts {
            let _ = context_mgr.create_context(&ctx_rec.name);
            for entry_rec in &ctx_rec.entries {
                let object = entry_rec
                    .object_id
                    .map(mimisbrunnr_types::ObjectId::from_raw);
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
                let entry = mimisbrunnr_types::ProjectedEntry {
                    object,
                    path: entry_rec.path.clone(),
                    entry_type,
                };
                let oid = object.unwrap_or(mimisbrunnr_types::ObjectId::from_raw(0));
                let _ = context_mgr.set_path(&ctx_rec.name, oid, &entry_rec.path, entry);
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

        let layout = mimisbrunnr_storage::ZoneLayout::compute(capacity).unwrap();
        let sb = Superblock::new(0, 0, layout);
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

            de.flush().unwrap();
        }

        {
            let de = DiskEngine::open(&config_path).unwrap();
            let e = de.engine();

            let oid = ObjectId::new(0, 0);
            let rec = e.get_object(oid).unwrap();
            assert_eq!(rec.content_hash, hash);
            assert_eq!(rec.blob_length, 11);
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
