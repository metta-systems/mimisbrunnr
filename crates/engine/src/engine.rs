use mimisbrunnr_index::{ForwardIndex, KvIndex, TagIndex};
use mimisbrunnr_meta::ObjectTable;
use mimisbrunnr_ontology::{ImplicationDag, Materializer};
use mimisbrunnr_query::QueryExecutor;
use mimisbrunnr_transform::TransformPipeline;
use mimisbrunnr_types::{
    Assertion, HybridTimestamp, ObjectId, ObjectState, Query, TagId, TagOrigin, Value,
};

use crate::error::EngineError;
use crate::oplog::{OpKind, OpLog, OpLogEntry};

/// The main storage engine, tying all layers together.
///
/// Provides object CRUD, tagging, querying, and the 4-phase deletion protocol.
/// All mutations are recorded in the oplog for subscription catch-up.
pub struct Engine {
    /// Object metadata table.
    pub object_table: ObjectTable,
    /// Tag inverted index (bitmap per tag).
    pub tag_index: TagIndex,
    /// Forward index (object → assertions).
    pub forward_index: ForwardIndex,
    /// Key-value equality index.
    pub kv_index: KvIndex,
    /// Ontology: tag definitions and implication DAG.
    pub dag: ImplicationDag,
    /// Operation log for subscriptions.
    pub oplog: OpLog,
    /// HLC clock for this node.
    clock: HybridTimestamp,
    /// This node's ID.
    node_id: u16,
    /// Transform pipeline for blob data.
    transform: TransformPipeline,
}

impl Engine {
    /// Create a new in-memory engine (no disk backing yet).
    pub fn new(node_id: u16) -> Self {
        Self {
            // 1M record capacity for in-memory use
            object_table: ObjectTable::new(0, 128 * 1024 * 1024),
            tag_index: TagIndex::new(),
            forward_index: ForwardIndex::new(),
            kv_index: KvIndex::new(),
            dag: ImplicationDag::new(),
            oplog: OpLog::new(),
            clock: HybridTimestamp::new(0, 0, node_id),
            node_id,
            transform: TransformPipeline::passthrough(),
        }
    }

    /// Set the transform pipeline.
    pub fn set_transform(&mut self, transform: TransformPipeline) {
        self.transform = transform;
    }

    /// Advance the clock and return the new timestamp.
    fn tick(&mut self, now_ms: u64) -> HybridTimestamp {
        self.clock.tick(now_ms);
        self.clock
    }

    fn emit_op(&mut self, now_ms: u64, op: OpKind) -> u64 {
        let ts = self.tick(now_ms);
        let lsn = self.oplog.len() as u64 + 1;
        self.oplog.push(OpLogEntry {
            timestamp: ts,
            lsn,
            op,
        });
        lsn
    }

    // ── Object CRUD ────────────────────────────────────────────────

    /// Create a new object. Returns its ObjectId.
    pub fn create_object(&mut self, now_ms: u64) -> Result<ObjectId, EngineError> {
        let oid = self.object_table.create(self.node_id)?;
        let rec = self.object_table.get_mut(oid).unwrap();
        rec.created_ns = (now_ms as i64) * 1_000_000;
        rec.modified_ns = rec.created_ns;

        self.emit_op(now_ms, OpKind::CreateObject { oid });
        Ok(oid)
    }

    /// Delete an object (phase 1: tombstone).
    pub fn delete_object(&mut self, oid: ObjectId, now_ms: u64) -> Result<(), EngineError> {
        let rec = self
            .object_table
            .get_mut(oid)
            .ok_or(EngineError::ObjectNotFound(oid))?;

        if rec.state != ObjectState::Active {
            return Err(EngineError::ObjectDeleted(oid));
        }

        // Phase 1: Tombstone
        rec.state = ObjectState::Tombstoned;

        // Phase 2: Index cleanup — remove from all bitmaps using forward index
        let entries = self.forward_index.remove_object(oid);
        let obj_local = oid.local() as u32;
        for entry in &entries {
            match &entry.assertion {
                Assertion::Tag(tag) => {
                    self.tag_index.untag_object(*tag, obj_local);
                }
                Assertion::Attr { key, value } => {
                    self.kv_index.remove(*key, value, obj_local);
                }
                Assertion::Relation { .. } => {}
            }
        }

        self.emit_op(now_ms, OpKind::DeleteObject { oid });
        Ok(())
    }

    /// Get object info.
    pub fn get_object(
        &self,
        oid: ObjectId,
    ) -> Result<&mimisbrunnr_meta::ObjectRecord, EngineError> {
        self.object_table
            .get(oid)
            .filter(|r| r.state == ObjectState::Active)
            .ok_or(EngineError::ObjectNotFound(oid))
    }

    // ── Tagging ────────────────────────────────────────────────────

    /// Add a tag to an object, with automatic materialization of implied tags.
    pub fn add_tag(
        &mut self,
        oid: ObjectId,
        tag: TagId,
        now_ms: u64,
    ) -> Result<Vec<TagId>, EngineError> {
        self.ensure_active(oid)?;
        let obj_local = oid.local() as u32;

        // Add direct tag
        self.tag_index.tag_object(tag, obj_local);
        self.forward_index
            .add(oid, Assertion::Tag(tag), TagOrigin::Direct);

        // Materialize implied tags
        let materialized =
            Materializer::materialize_tag(&self.dag, &mut self.tag_index, &mut self.forward_index, oid, tag);

        // Update record
        if let Some(rec) = self.object_table.get_mut(oid) {
            rec.tag_count = self.forward_index.tag_ids(oid).len() as u16;
            rec.modified_ns = (now_ms as i64) * 1_000_000;
        }

        self.emit_op(now_ms, OpKind::AddTag { oid, tag });
        Ok(materialized)
    }

    /// Remove a tag from an object, with de-materialization.
    pub fn remove_tag(
        &mut self,
        oid: ObjectId,
        tag: TagId,
        now_ms: u64,
    ) -> Result<Vec<TagId>, EngineError> {
        self.ensure_active(oid)?;
        let obj_local = oid.local() as u32;

        // Remove direct tag
        self.tag_index.untag_object(tag, obj_local);
        self.forward_index
            .remove(oid, &Assertion::Tag(tag));

        // De-materialize tags no longer justified
        let dematerialized = Materializer::dematerialize_tag(
            &self.dag,
            &mut self.tag_index,
            &mut self.forward_index,
            oid,
            tag,
        );

        // Update record
        if let Some(rec) = self.object_table.get_mut(oid) {
            rec.tag_count = self.forward_index.tag_ids(oid).len() as u16;
            rec.modified_ns = (now_ms as i64) * 1_000_000;
        }

        self.emit_op(now_ms, OpKind::RemoveTag { oid, tag });
        Ok(dematerialized)
    }

    // ── Attributes ─────────────────────────────────────────────────

    /// Set an attribute on an object.
    pub fn set_attr(
        &mut self,
        oid: ObjectId,
        key: TagId,
        value: Value,
        now_ms: u64,
    ) -> Result<(), EngineError> {
        self.ensure_active(oid)?;
        let obj_local = oid.local() as u32;

        // Remove previous value for this key (if any) from kv index
        let existing: Vec<_> = self
            .forward_index
            .get(oid)
            .iter()
            .filter_map(|e| match &e.assertion {
                Assertion::Attr { key: k, value: v } if *k == key => Some(v.clone()),
                _ => None,
            })
            .collect();
        for old_val in &existing {
            self.kv_index.remove(key, old_val, obj_local);
            self.forward_index
                .remove(oid, &Assertion::Attr { key, value: old_val.clone() });
        }

        // Add new value
        self.kv_index.insert(key, &value, obj_local);
        self.forward_index
            .add(oid, Assertion::Attr { key, value }, TagOrigin::Direct);

        if let Some(rec) = self.object_table.get_mut(oid) {
            rec.attr_count = self
                .forward_index
                .get(oid)
                .iter()
                .filter(|e| matches!(e.assertion, Assertion::Attr { .. }))
                .count() as u16;
            rec.modified_ns = (now_ms as i64) * 1_000_000;
        }

        self.emit_op(now_ms, OpKind::SetAttr { oid, tag: key });
        Ok(())
    }

    /// Remove an attribute from an object.
    pub fn remove_attr(
        &mut self,
        oid: ObjectId,
        key: TagId,
        value: &Value,
        now_ms: u64,
    ) -> Result<(), EngineError> {
        self.ensure_active(oid)?;
        let obj_local = oid.local() as u32;

        self.kv_index.remove(key, value, obj_local);
        self.forward_index
            .remove(oid, &Assertion::Attr { key, value: value.clone() });

        if let Some(rec) = self.object_table.get_mut(oid) {
            rec.attr_count = self
                .forward_index
                .get(oid)
                .iter()
                .filter(|e| matches!(e.assertion, Assertion::Attr { .. }))
                .count() as u16;
            rec.modified_ns = (now_ms as i64) * 1_000_000;
        }

        self.emit_op(now_ms, OpKind::RemoveAttr { oid, tag: key });
        Ok(())
    }

    // ── Blob data ──────────────────────────────────────────────────

    /// Write blob data for an object through the transform pipeline.
    /// Returns the content hash.
    pub fn write_blob(
        &mut self,
        oid: ObjectId,
        data: &[u8],
        now_ms: u64,
    ) -> Result<[u8; 32], EngineError> {
        self.ensure_active(oid)?;

        let result = self.transform.transform_write(data)?;

        if let Some(rec) = self.object_table.get_mut(oid) {
            rec.content_hash = result.content_hash;
            rec.blob_length = result.original_size as u64;
            rec.stored_size = result.stored_size as u64;
            rec.modified_ns = (now_ms as i64) * 1_000_000;
        }

        self.emit_op(now_ms, OpKind::WriteBlob { oid });
        Ok(result.content_hash)
    }

    // ── Queries ────────────────────────────────────────────────────

    /// Execute a query and return matching object IDs.
    pub fn query(&self, query: &Query) -> Vec<ObjectId> {
        let executor = QueryExecutor::new(&self.tag_index, &self.kv_index, &self.dag);
        executor
            .execute(query)
            .iter()
            .filter_map(|local| {
                let oid = ObjectId::new(self.node_id, local as u64);
                // Only return active objects
                self.object_table
                    .get(oid)
                    .filter(|r| r.state == ObjectState::Active)
                    .map(|_| oid)
            })
            .collect()
    }

    /// Parse and execute a query string.
    pub fn query_str(&self, query_str: &str) -> Result<Vec<ObjectId>, EngineError> {
        let parser = mimisbrunnr_query::QueryParser::new(&self.dag);
        let query = parser.parse(query_str)?;
        Ok(self.query(&query))
    }

    // ── Info ───────────────────────────────────────────────────────

    /// Get all assertions for an object.
    pub fn assertions(&self, oid: ObjectId) -> Result<&[mimisbrunnr_index::ForwardEntry], EngineError> {
        self.ensure_active(oid)?;
        Ok(self.forward_index.get(oid))
    }

    /// Get all tags on an object (both direct and materialized).
    pub fn tags(&self, oid: ObjectId) -> Result<Vec<TagId>, EngineError> {
        self.ensure_active(oid)?;
        Ok(self.forward_index.tag_ids(oid))
    }

    /// Get only direct tags on an object.
    pub fn direct_tags(&self, oid: ObjectId) -> Result<Vec<TagId>, EngineError> {
        self.ensure_active(oid)?;
        Ok(self.forward_index.direct_tags(oid))
    }

    // ── Ontology ───────────────────────────────────────────────────

    /// Register a tag in the ontology.
    pub fn register_tag(
        &mut self,
        def: mimisbrunnr_ontology::TagDefinition,
    ) -> Result<TagId, EngineError> {
        Ok(self.dag.register_tag(def)?)
    }

    /// Add an implication and materialize it across existing objects.
    pub fn add_implication(&mut self, from: TagId, to: TagId) -> Result<(), EngineError> {
        self.dag.add_implication(from, to)?;
        Materializer::materialize_implication(&mut self.tag_index, from, to);
        Ok(())
    }

    /// This node's ID.
    pub fn node_id(&self) -> u16 {
        self.node_id
    }

    // ── Helpers ─────────────────────────────────────────────────────

    fn ensure_active(&self, oid: ObjectId) -> Result<(), EngineError> {
        match self.object_table.get(oid) {
            Some(rec) if rec.state == ObjectState::Active => Ok(()),
            Some(_) => Err(EngineError::ObjectDeleted(oid)),
            None => Err(EngineError::ObjectNotFound(oid)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mimisbrunnr_ontology::{TagDefinition, TagSemantics, ValueType};
    use mimisbrunnr_types::CmpOp;

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    fn label(id: u32, name: &str) -> TagDefinition {
        TagDefinition::new(tag(id), name, TagSemantics::Label)
    }

    fn attr_def(id: u32, name: &str) -> TagDefinition {
        TagDefinition::new(
            tag(id),
            name,
            TagSemantics::Attribute {
                value_type: ValueType::Text,
            },
        )
    }

    fn setup_engine() -> Engine {
        let mut e = Engine::new(0);
        // Register some tags
        e.register_tag(label(1, "electronic")).unwrap();
        e.register_tag(label(2, "portable")).unwrap();
        e.register_tag(label(3, "discontinued")).unwrap();
        e.register_tag(label(4, "vehicle")).unwrap();
        e.register_tag(label(5, "car")).unwrap();
        e.register_tag(label(6, "truck")).unwrap();
        e.register_tag(attr_def(10, "artist")).unwrap();
        e.register_tag(TagDefinition::new(
            tag(11),
            "year",
            TagSemantics::Attribute {
                value_type: ValueType::Int,
            },
        ))
        .unwrap();

        // car → vehicle, truck → vehicle
        e.add_implication(tag(5), tag(4)).unwrap();
        e.add_implication(tag(6), tag(4)).unwrap();
        e
    }

    #[test]
    fn create_and_get_object() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();

        let rec = e.get_object(oid).unwrap();
        assert_eq!(rec.id, oid.raw());
        assert!(rec.is_active());
    }

    #[test]
    fn add_tag_and_query() {
        let mut e = setup_engine();
        let oid1 = e.create_object(1000).unwrap();
        let oid2 = e.create_object(1000).unwrap();

        e.add_tag(oid1, tag(1), 1000).unwrap(); // electronic
        e.add_tag(oid2, tag(1), 1000).unwrap(); // electronic
        e.add_tag(oid1, tag(2), 1000).unwrap(); // portable

        let result = e.query(&Query::HasTag(tag(1)));
        assert_eq!(result.len(), 2);

        let result = e.query(&Query::And(vec![
            Query::HasTag(tag(1)),
            Query::HasTag(tag(2)),
        ]));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], oid1);
    }

    #[test]
    fn tag_materialization() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();

        let materialized = e.add_tag(oid, tag(5), 1000).unwrap(); // car
        assert!(materialized.contains(&tag(4))); // vehicle materialized

        // Query for vehicle should find the car
        let result = e.query(&Query::HasTag(tag(4)));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], oid);
    }

    #[test]
    fn remove_tag_dematerialization() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();

        e.add_tag(oid, tag(5), 1000).unwrap(); // car → vehicle
        e.remove_tag(oid, tag(5), 2000).unwrap();

        // Vehicle should be gone too
        let result = e.query(&Query::HasTag(tag(4)));
        assert!(result.is_empty());
    }

    #[test]
    fn remove_tag_keeps_justified() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();

        e.add_tag(oid, tag(5), 1000).unwrap(); // car → vehicle
        e.add_tag(oid, tag(6), 1000).unwrap(); // truck → vehicle

        e.remove_tag(oid, tag(5), 2000).unwrap(); // remove car

        // Vehicle still present via truck
        let result = e.query(&Query::HasTag(tag(4)));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn set_and_query_attr() {
        let mut e = setup_engine();
        let oid1 = e.create_object(1000).unwrap();
        let oid2 = e.create_object(1000).unwrap();

        e.set_attr(oid1, tag(10), Value::Text("Aphex Twin".into()), 1000)
            .unwrap();
        e.set_attr(oid2, tag(10), Value::Text("Boards of Canada".into()), 1000)
            .unwrap();

        let result = e.query(&Query::HasAttr {
            key: tag(10),
            op: CmpOp::Eq,
            value: Value::Text("Aphex Twin".into()),
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], oid1);
    }

    #[test]
    fn set_attr_replaces_previous() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();

        e.set_attr(oid, tag(10), Value::Text("old".into()), 1000).unwrap();
        e.set_attr(oid, tag(10), Value::Text("new".into()), 2000).unwrap();

        // Old value should not match
        let old_result = e.query(&Query::HasAttr {
            key: tag(10),
            op: CmpOp::Eq,
            value: Value::Text("old".into()),
        });
        assert!(old_result.is_empty());

        // New value should match
        let new_result = e.query(&Query::HasAttr {
            key: tag(10),
            op: CmpOp::Eq,
            value: Value::Text("new".into()),
        });
        assert_eq!(new_result.len(), 1);
    }

    #[test]
    fn delete_object() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();
        e.add_tag(oid, tag(1), 1000).unwrap();

        e.delete_object(oid, 2000).unwrap();

        // Should not appear in queries
        let result = e.query(&Query::HasTag(tag(1)));
        assert!(result.is_empty());

        // Should not be gettable
        assert!(e.get_object(oid).is_err());
    }

    #[test]
    fn delete_removes_from_indexes() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();
        e.add_tag(oid, tag(1), 1000).unwrap();
        e.add_tag(oid, tag(2), 1000).unwrap();
        e.set_attr(oid, tag(10), Value::Text("test".into()), 1000).unwrap();

        e.delete_object(oid, 2000).unwrap();

        // All indexes should be clean
        assert!(!e.tag_index.has_tag(tag(1), oid.local() as u32));
        assert!(!e.tag_index.has_tag(tag(2), oid.local() as u32));
        assert!(e.forward_index.get(oid).is_empty());
    }

    #[test]
    fn double_delete_fails() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();
        e.delete_object(oid, 2000).unwrap();
        assert!(e.delete_object(oid, 3000).is_err());
    }

    #[test]
    fn write_blob() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();

        let hash = e.write_blob(oid, b"hello world", 1000).unwrap();
        assert_ne!(hash, [0u8; 32]);

        let rec = e.get_object(oid).unwrap();
        assert_eq!(rec.content_hash, hash);
        assert_eq!(rec.blob_length, 11);
    }

    #[test]
    fn query_str() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();
        e.add_tag(oid, tag(1), 1000).unwrap(); // electronic

        let result = e.query_str("electronic").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], oid);
    }

    #[test]
    fn complex_query_str() {
        let mut e = setup_engine();
        let oid1 = e.create_object(1000).unwrap();
        let oid2 = e.create_object(1000).unwrap();
        let oid3 = e.create_object(1000).unwrap();

        e.add_tag(oid1, tag(1), 1000).unwrap(); // electronic
        e.add_tag(oid1, tag(2), 1000).unwrap(); // portable
        e.add_tag(oid2, tag(1), 1000).unwrap(); // electronic
        e.add_tag(oid3, tag(1), 1000).unwrap(); // electronic
        e.add_tag(oid3, tag(3), 1000).unwrap(); // discontinued

        let result = e.query_str("electronic AND NOT discontinued").unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn oplog_records_operations() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();
        e.add_tag(oid, tag(1), 1000).unwrap();
        e.set_attr(oid, tag(10), Value::Text("test".into()), 1000).unwrap();
        e.delete_object(oid, 2000).unwrap();

        assert_eq!(e.oplog.len(), 4); // create + tag + attr + delete
    }

    #[test]
    fn assertions_list() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();
        e.add_tag(oid, tag(1), 1000).unwrap();
        e.set_attr(oid, tag(10), Value::Text("hello".into()), 1000).unwrap();

        let assertions = e.assertions(oid).unwrap();
        assert!(assertions.len() >= 2); // tag + attr (+ possible materialized)
    }

    #[test]
    fn direct_vs_all_tags() {
        let mut e = setup_engine();
        let oid = e.create_object(1000).unwrap();
        e.add_tag(oid, tag(5), 1000).unwrap(); // car → vehicle

        let all = e.tags(oid).unwrap();
        let direct = e.direct_tags(oid).unwrap();

        assert!(all.len() > direct.len()); // all includes materialized
        assert_eq!(direct, vec![tag(5)]); // only car is direct
        assert!(all.contains(&tag(4))); // vehicle is materialized
    }

    #[test]
    fn isa_query_through_engine() {
        let mut e = setup_engine();
        let oid1 = e.create_object(1000).unwrap();
        let oid2 = e.create_object(1000).unwrap();

        e.add_tag(oid1, tag(5), 1000).unwrap(); // car
        e.add_tag(oid2, tag(6), 1000).unwrap(); // truck

        // IsA(vehicle) should find both via materialized tags
        let result = e.query(&Query::IsA(tag(4)));
        assert_eq!(result.len(), 2);
    }
}
