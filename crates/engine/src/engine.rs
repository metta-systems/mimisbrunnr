//! [`Engine`] — central in-memory state holder (DESIGN §15).
//!
//! Owns every in-memory mirror (object table, location table, all five
//! indices, ontology state, subscriptions, path contexts, oplog) plus the
//! HLC clock and the transform pipeline configuration. Mutations land in the
//! mirrors first; the durable WAL append happens at the [`crate::DiskEngine`]
//! layer.

use mimisbrunnr_index::{ChunkIndex, ForwardIndex, KvIndex, RangeIndex, TagIndex};
use mimisbrunnr_meta::{LocationTable, OBJECT_RECORD_SIZE, ObjectRecord, ObjectTable};
use mimisbrunnr_ontology::{IdAllocator, InstallResult, OntologyModule, OntologyState};
use mimisbrunnr_query::QueryExecutor;
use mimisbrunnr_transform::{TransformPipeline, TransformResult};
use mimisbrunnr_types::{
    Assertion, ChangeInterest, NodeId, ObjectId, ObjectState, Query, SubscriptionId, TagDefinition,
    TagId, TagOrigin, TagSemantics, Value,
};
use mimisbrunnr_unix::PathContextManager;
use mimisbrunnr_watch::{Retention, SubscriptionEngine};
use roaring::RoaringBitmap;

use crate::{EngineError, OpKind, OpLog, clock::HybridClock, oplog::DEFAULT_OPLOG_CAPACITY};
use crate::wal_proj::engine_value_hash;

/// Result of [`Engine::write_blob`]: the post-transform metadata. The engine
/// itself does not own the blob bytes — those land in
/// [`crate::DiskEngine::blobs`].
#[derive(Debug, Clone)]
pub struct BlobWriteResult {
    pub content_hash: [u8; 32],
    pub original_size: u64,
    pub stored_size: u64,
    /// Post-transform bytes (compressed / padded / encrypted as configured).
    pub data: Vec<u8>,
}

/// In-memory engine state.
pub struct Engine {
    pub object_table: ObjectTable,
    pub location_table: LocationTable,
    pub forward_index: ForwardIndex,
    pub tag_index: TagIndex,
    pub kv_index: KvIndex,
    pub range_index: RangeIndex,
    pub chunk_index: ChunkIndex,
    pub ontology: OntologyState,
    pub subscriptions: SubscriptionEngine,
    pub path_contexts: PathContextManager,
    pub oplog: OpLog,
    pub clock: HybridClock,
    pub node_id: NodeId,
    pub transform: TransformPipeline,
    /// Monotonic local sequence counter for newly minted ObjectIds.
    next_oid_local: u64,
    /// Allocator base for `register_tag` (ad-hoc tags).
    next_tag_id: u32,
    /// LSN of the most recently *replayed* WAL entry — used to make replay
    /// idempotent. Live mutations don't bump this (they go through the
    /// `record_*` helpers in `DiskEngine` instead).
    pub last_applied_lsn: u64,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("node_id", &self.node_id)
            .field("next_oid_local", &self.next_oid_local)
            .field("next_tag_id", &self.next_tag_id)
            .field("last_applied_lsn", &self.last_applied_lsn)
            .field("object_count", &self.object_table.len())
            .field("oplog_len", &self.oplog.len())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// New empty engine attributing all events to `node_id`.
    pub fn new(node_id: NodeId) -> Self {
        Self {
            object_table: ObjectTable::new(),
            location_table: LocationTable::new(),
            forward_index: ForwardIndex::new(),
            tag_index: TagIndex::new(),
            kv_index: KvIndex::new(),
            range_index: RangeIndex::new(),
            chunk_index: ChunkIndex::new(),
            ontology: OntologyState::new(),
            subscriptions: SubscriptionEngine::new(),
            path_contexts: PathContextManager::new(),
            oplog: OpLog::new(DEFAULT_OPLOG_CAPACITY),
            clock: HybridClock::new(node_id),
            node_id,
            transform: TransformPipeline::passthrough(),
            next_oid_local: 1,
            next_tag_id: 1,
            last_applied_lsn: 0,
        }
    }

    /// Override the transform pipeline (compression / encryption choice). The
    /// default is [`TransformPipeline::passthrough`].
    pub fn set_transform(&mut self, pipeline: TransformPipeline) {
        self.transform = pipeline;
    }

    // ----------------------------------------------------------------------
    // Mutation API. Each call updates the mirrors *and* the in-memory oplog;
    // WAL append is the DiskEngine's job.
    // ----------------------------------------------------------------------

    /// Allocate a fresh `ObjectId` and insert an Active `ObjectRecord`.
    pub fn create_object(&mut self) -> ObjectId {
        let local = self.next_oid_local;
        self.next_oid_local = self.next_oid_local.saturating_add(1);
        let oid = ObjectId::from_parts(self.node_id, local);

        let ts = self.clock.now();
        let mut rec = ObjectRecord::new(oid.to_u64());
        rec.set_state(ObjectState::Active);
        rec.created_ns = ts.physical_ns;
        rec.modified_ns = ts.physical_ns;
        self.object_table.insert(rec);

        // Engine-level oplog. LSN 0 — DiskEngine overwrites with the WAL LSN
        // when persisted.
        self.oplog
            .record(OpKind::CreateObject { oid }, 0, ts);

        // Subscription hook: object birth.
        self.subscriptions
            .on_object_created(oid, 0, ts.physical_ns);
        oid
    }

    /// Delete `oid`: tombstone the record, drop assertions from every index,
    /// emit subscription hooks. Per DESIGN §7 the ID is **never** reused.
    pub fn delete_object(&mut self, oid: ObjectId) -> Result<(), EngineError> {
        let raw = oid.to_u64();
        let rec = self
            .object_table
            .get_mut(raw)
            .ok_or(EngineError::ObjectNotFound(oid))?;
        let ts = self.clock.now();
        rec.set_state(ObjectState::Tombstoned);
        rec.modified_ns = ts.physical_ns;

        // Drop from indices.
        self.tag_index.remove_object_from_all(oid);
        // KV / range index entries are keyed by `(tag, value_hash)` — strip
        // every Attr assertion observed in the forward index.
        let assertions = self.forward_index.assertions_of(oid).to_vec();
        for (a, _origin) in &assertions {
            if let Assertion::Attr { key, value } = a {
                let local32 = (raw & 0xffff_ffff) as u32;
                self.kv_index.remove(*key, value, local32);
                self.range_index.remove(*key, value, local32);
            }
        }
        let _ = self.forward_index.remove_object(oid);
        self.location_table.remove(raw);

        self.oplog
            .record(OpKind::DeleteObject { oid }, 0, ts);
        self.subscriptions
            .on_object_deleted(oid, 0, ts.physical_ns);
        Ok(())
    }

    /// Add a Direct tag and apply the ontology's implication closure as
    /// Materialized tags.
    pub fn add_tag(&mut self, oid: ObjectId, tag: TagId) -> Result<(), EngineError> {
        let raw = oid.to_u64();
        if self.object_table.get(raw).is_none() {
            return Err(EngineError::ObjectNotFound(oid));
        }
        let ts = self.clock.now();
        let closure = self.ontology.materialise(&[tag]);
        let closure_count = closure.len() as u16;

        // Insert all closure tags. The original `tag` carries Direct; every
        // other closure member is Materialized.
        for closure_tag in &closure {
            let origin = if *closure_tag == tag {
                TagOrigin::Direct
            } else {
                TagOrigin::Materialized
            };
            // Forward index: skip duplicates (HasTag idempotent).
            let already = self
                .forward_index
                .assertions_of(oid)
                .iter()
                .any(|(a, _)| matches!(a, Assertion::Tag(t) if *t == *closure_tag));
            if !already {
                self.forward_index
                    .add_assertion(oid, Assertion::Tag(*closure_tag), origin);
                self.tag_index.add_member(*closure_tag, oid);
            }
        }
        // Watch hook only for the Direct tag — materialised tags are
        // recoverable from ontology + direct tag, so a separate hook would
        // double-fire.
        self.subscriptions
            .on_tag_added(oid, tag, 0, ts.physical_ns);

        // ObjectRecord bookkeeping.
        if let Some(rec) = self.object_table.get_mut(raw) {
            rec.tag_count = rec.tag_count.saturating_add(closure_count);
            rec.modified_ns = ts.physical_ns;
        }

        self.oplog.record(OpKind::AddTag { oid, tag }, 0, ts);
        Ok(())
    }

    /// Remove a tag. **Semantics**: removing a Direct tag also removes the
    /// Materialized tags that this Direct tag *uniquely* sourced — i.e. any
    /// implied tag that no other Direct tag still implies stays gone, others
    /// stay. Removing a Materialized tag directly is allowed but only
    /// removes that single edge (it may reappear after another `add_tag`).
    pub fn remove_tag(&mut self, oid: ObjectId, tag: TagId) -> Result<(), EngineError> {
        let raw = oid.to_u64();
        if self.object_table.get(raw).is_none() {
            return Err(EngineError::ObjectNotFound(oid));
        }
        let ts = self.clock.now();

        let was_direct = self
            .forward_index
            .assertions_of(oid)
            .iter()
            .any(|(a, o)| matches!(a, Assertion::Tag(t) if *t == tag) && *o == TagOrigin::Direct);

        if self
            .forward_index
            .remove_assertion(oid, &Assertion::Tag(tag))
        {
            self.tag_index.remove_member(tag, oid);
        }

        if was_direct {
            let surviving_direct: Vec<TagId> = self.forward_index.direct_tags(oid);
            let mut still_implied = std::collections::BTreeSet::new();
            for t in &surviving_direct {
                for c in self.ontology.materialise(&[*t]) {
                    still_implied.insert(c);
                }
            }
            let materialized_now: Vec<TagId> = self.forward_index.materialized_tags(oid);
            for m in materialized_now {
                if !still_implied.contains(&m)
                    && self
                        .forward_index
                        .remove_assertion(oid, &Assertion::Tag(m))
                {
                    self.tag_index.remove_member(m, oid);
                }
            }
        }

        if let Some(rec) = self.object_table.get_mut(raw) {
            rec.tag_count = rec.tag_count.saturating_sub(1);
            rec.modified_ns = ts.physical_ns;
        }

        self.subscriptions
            .on_tag_removed(oid, tag, 0, ts.physical_ns);
        self.oplog.record(OpKind::RemoveTag { oid, tag }, 0, ts);
        Ok(())
    }

    /// Set / overwrite an attribute. Updates kv-index and range-index, and
    /// records a forward-index `Attr` assertion (Direct origin).
    pub fn set_attr(
        &mut self,
        oid: ObjectId,
        key: TagId,
        value: Value,
    ) -> Result<(), EngineError> {
        let raw = oid.to_u64();
        if self.object_table.get(raw).is_none() {
            return Err(EngineError::ObjectNotFound(oid));
        }
        let ts = self.clock.now();
        let local32 = (raw & 0xffff_ffff) as u32;
        self.forward_index.add_assertion(
            oid,
            Assertion::Attr {
                key,
                value: value.clone(),
            },
            TagOrigin::Direct,
        );
        self.kv_index.insert(key, &value, local32);
        self.range_index.insert(key, &value, local32);

        if let Some(rec) = self.object_table.get_mut(raw) {
            rec.attr_count = rec.attr_count.saturating_add(1);
            rec.modified_ns = ts.physical_ns;
        }

        self.subscriptions
            .on_content_changed(oid, 0, ts.physical_ns);
        self.oplog
            .record(OpKind::SetAttr { oid, key, value }, 0, ts);
        Ok(())
    }

    /// Remove an attribute by `(key, value_hash)`.
    pub fn remove_attr(
        &mut self,
        oid: ObjectId,
        key: TagId,
        value_hash: u64,
    ) -> Result<(), EngineError> {
        let raw = oid.to_u64();
        if self.object_table.get(raw).is_none() {
            return Err(EngineError::ObjectNotFound(oid));
        }
        let ts = self.clock.now();
        let local32 = (raw & 0xffff_ffff) as u32;

        let assertions = self.forward_index.assertions_of(oid).to_vec();
        let mut to_remove: Option<Value> = None;
        for (a, _) in &assertions {
            if let Assertion::Attr { key: k, value } = a
                && *k == key
                && engine_value_hash(value) == value_hash
            {
                to_remove = Some(value.clone());
                break;
            }
        }
        if let Some(value) = to_remove {
            self.forward_index.remove_assertion(
                oid,
                &Assertion::Attr {
                    key,
                    value: value.clone(),
                },
            );
            self.kv_index.remove(key, &value, local32);
            self.range_index.remove(key, &value, local32);
            if let Some(rec) = self.object_table.get_mut(raw) {
                rec.attr_count = rec.attr_count.saturating_sub(1);
                rec.modified_ns = ts.physical_ns;
            }
        }

        self.subscriptions
            .on_content_changed(oid, 0, ts.physical_ns);
        self.oplog.record(
            OpKind::RemoveAttr {
                oid,
                key,
                value_hash,
            },
            0,
            ts,
        );
        Ok(())
    }

    /// Add a `(predicate, target)` relation.
    pub fn add_relation(
        &mut self,
        oid: ObjectId,
        predicate: TagId,
        target: ObjectId,
    ) -> Result<(), EngineError> {
        let raw = oid.to_u64();
        if self.object_table.get(raw).is_none() {
            return Err(EngineError::ObjectNotFound(oid));
        }
        let ts = self.clock.now();
        self.forward_index.add_assertion(
            oid,
            Assertion::Relation { predicate, target },
            TagOrigin::Direct,
        );
        if let Some(rec) = self.object_table.get_mut(raw) {
            rec.relation_count = rec.relation_count.saturating_add(1);
            rec.modified_ns = ts.physical_ns;
        }
        self.oplog.record(
            OpKind::AddRelation {
                oid,
                predicate,
                target,
            },
            0,
            ts,
        );
        Ok(())
    }

    /// Remove a `(predicate, target)` relation.
    pub fn remove_relation(
        &mut self,
        oid: ObjectId,
        predicate: TagId,
        target: ObjectId,
    ) -> Result<(), EngineError> {
        let raw = oid.to_u64();
        if self.object_table.get(raw).is_none() {
            return Err(EngineError::ObjectNotFound(oid));
        }
        let ts = self.clock.now();
        let removed = self
            .forward_index
            .remove_assertion(oid, &Assertion::Relation { predicate, target });
        if removed
            && let Some(rec) = self.object_table.get_mut(raw)
        {
            rec.relation_count = rec.relation_count.saturating_sub(1);
            rec.modified_ns = ts.physical_ns;
        }
        self.oplog.record(
            OpKind::RemoveRelation {
                oid,
                predicate,
                target,
            },
            0,
            ts,
        );
        Ok(())
    }

    /// Run the configured transform pipeline over `plaintext` and update the
    /// `ObjectRecord` content metadata.
    pub fn write_blob(
        &mut self,
        oid: ObjectId,
        plaintext: &[u8],
    ) -> Result<BlobWriteResult, EngineError> {
        let raw = oid.to_u64();
        if self.object_table.get(raw).is_none() {
            return Err(EngineError::ObjectNotFound(oid));
        }
        let ts = self.clock.now();
        let result: TransformResult = self.transform.apply(plaintext)?;
        if let Some(rec) = self.object_table.get_mut(raw) {
            rec.content_hash = result.content_hash;
            rec.blob_length = result.original_size;
            rec.stored_size = result.stored_size;
            rec.modified_ns = ts.physical_ns;
        }
        self.subscriptions
            .on_content_changed(oid, 0, ts.physical_ns);
        self.oplog.record(
            OpKind::WriteBlob {
                oid,
                content_hash: result.content_hash,
                size: result.original_size,
            },
            0,
            ts,
        );
        Ok(BlobWriteResult {
            content_hash: result.content_hash,
            original_size: result.original_size,
            stored_size: result.stored_size,
            data: result.data,
        })
    }

    // ----------------------------------------------------------------------
    // Query / read API.
    // ----------------------------------------------------------------------

    /// Evaluate a query against the live indices.
    pub fn query(&self, query: &Query) -> Result<RoaringBitmap, EngineError> {
        let exec = QueryExecutor::new(
            &self.tag_index,
            &self.kv_index,
            &self.range_index,
            &self.forward_index,
            &self.ontology,
        );
        Ok(exec.evaluate(query)?)
    }

    /// Evaluate a query and reconstruct full [`ObjectId`]s.
    pub fn query_full(&self, query: &Query) -> Result<Vec<ObjectId>, EngineError> {
        let exec = QueryExecutor::new(
            &self.tag_index,
            &self.kv_index,
            &self.range_index,
            &self.forward_index,
            &self.ontology,
        );
        Ok(exec.evaluate_full(query)?)
    }

    // ----------------------------------------------------------------------
    // Subscriptions.
    // ----------------------------------------------------------------------

    /// Register a subscription. Computes the initial result via the live
    /// query executor.
    ///
    /// Note (DESIGN §11.3): for arbitrary boolean queries the engine layer is
    /// expected to call `SubscriptionEngine::set_membership` after every
    /// mutation that could change a sub's matched set. Phase 6 leaves this as
    /// a TODO — only `HasTag` / `IsA` clauses get correct live updates.
    /// TODO(rewrite-phase-N): re-evaluate boolean queries on every mutation.
    pub fn subscribe(
        &mut self,
        name: String,
        query: Query,
        interest: ChangeInterest,
        retention: Retention,
    ) -> Result<SubscriptionId, EngineError> {
        let initial = {
            let exec = QueryExecutor::new(
                &self.tag_index,
                &self.kv_index,
                &self.range_index,
                &self.forward_index,
                &self.ontology,
            );
            exec.evaluate(&query)?
        };
        let id = self
            .subscriptions
            .register(name, query, interest, retention, initial, 0);
        Ok(id)
    }

    // ----------------------------------------------------------------------
    // Ontology.
    // ----------------------------------------------------------------------

    /// Install an [`OntologyModule`].
    pub fn install_ontology_module(
        &mut self,
        module: OntologyModule,
    ) -> Result<InstallResult, EngineError> {
        let mut alloc = IdAllocator::starting_at(self.next_tag_id);
        let res = self.ontology.install(module, &mut alloc)?;
        let max_id = self
            .ontology
            .tags
            .keys()
            .map(|t| t.raw())
            .max()
            .unwrap_or(0);
        self.next_tag_id = self.next_tag_id.max(max_id + 1);
        self.subscriptions.refresh_ontology_links(&self.ontology);
        Ok(res)
    }

    /// Look up a tag by name.
    pub fn resolve_tag_name(&self, name: &str) -> Option<TagId> {
        self.ontology.names.get(name).copied()
    }

    /// Register an ad-hoc Label tag if absent. Returns the tag's id.
    pub fn register_tag(&mut self, name: &str) -> TagId {
        if let Some(existing) = self.resolve_tag_name(name) {
            return existing;
        }
        let id = TagId::new(self.next_tag_id);
        self.next_tag_id = self.next_tag_id.saturating_add(1);
        let def = TagDefinition {
            id,
            name: name.into(),
            semantics: TagSemantics::Label,
            implies: vec![],
            storage: None,
        };
        self.ontology.dag.add_tag(id);
        self.ontology.names.insert(name.to_string(), id);
        self.ontology.tags.insert(id, def);
        id
    }

    // ----------------------------------------------------------------------
    // Replay helpers — called by `wal_proj::replay_wal_op` only.
    // ----------------------------------------------------------------------

    pub(crate) fn replay_create_object(
        &mut self,
        oid: ObjectId,
        created_ns: i64,
        _lsn: u64,
    ) -> Result<(), EngineError> {
        let raw = oid.to_u64();
        if self.object_table.get(raw).is_none() {
            let mut rec = ObjectRecord::new(raw);
            rec.set_state(ObjectState::Active);
            rec.created_ns = created_ns;
            rec.modified_ns = created_ns;
            self.object_table.insert(rec);
        }
        let local = oid.local_seq();
        if local >= self.next_oid_local {
            self.next_oid_local = local.saturating_add(1);
        }
        Ok(())
    }

    pub(crate) fn replay_delete_object(
        &mut self,
        oid: ObjectId,
        _lsn: u64,
    ) -> Result<(), EngineError> {
        let raw = oid.to_u64();
        if let Some(rec) = self.object_table.get_mut(raw) {
            rec.set_state(ObjectState::Tombstoned);
        }
        self.tag_index.remove_object_from_all(oid);
        self.forward_index.remove_object(oid);
        self.location_table.remove(raw);
        Ok(())
    }

    pub(crate) fn replay_add_tag(
        &mut self,
        oid: ObjectId,
        tag: TagId,
        _lsn: u64,
    ) -> Result<(), EngineError> {
        if self.object_table.get(oid.to_u64()).is_none() {
            let mut rec = ObjectRecord::new(oid.to_u64());
            rec.set_state(ObjectState::Active);
            self.object_table.insert(rec);
        }
        let closure = self.ontology.materialise(&[tag]);
        for closure_tag in &closure {
            let origin = if *closure_tag == tag {
                TagOrigin::Direct
            } else {
                TagOrigin::Materialized
            };
            let already = self
                .forward_index
                .assertions_of(oid)
                .iter()
                .any(|(a, _)| matches!(a, Assertion::Tag(t) if *t == *closure_tag));
            if !already {
                self.forward_index
                    .add_assertion(oid, Assertion::Tag(*closure_tag), origin);
                self.tag_index.add_member(*closure_tag, oid);
            }
        }
        Ok(())
    }

    pub(crate) fn replay_remove_tag(
        &mut self,
        oid: ObjectId,
        tag: TagId,
        _lsn: u64,
    ) -> Result<(), EngineError> {
        if self.object_table.get(oid.to_u64()).is_none() {
            return Ok(());
        }
        let was_direct = self
            .forward_index
            .assertions_of(oid)
            .iter()
            .any(|(a, o)| matches!(a, Assertion::Tag(t) if *t == tag) && *o == TagOrigin::Direct);
        if self
            .forward_index
            .remove_assertion(oid, &Assertion::Tag(tag))
        {
            self.tag_index.remove_member(tag, oid);
        }
        if was_direct {
            let surviving_direct: Vec<TagId> = self.forward_index.direct_tags(oid);
            let mut still_implied = std::collections::BTreeSet::new();
            for t in &surviving_direct {
                for c in self.ontology.materialise(&[*t]) {
                    still_implied.insert(c);
                }
            }
            let materialized_now: Vec<TagId> = self.forward_index.materialized_tags(oid);
            for m in materialized_now {
                if !still_implied.contains(&m)
                    && self
                        .forward_index
                        .remove_assertion(oid, &Assertion::Tag(m))
                {
                    self.tag_index.remove_member(m, oid);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn replay_set_attr(
        &mut self,
        oid: ObjectId,
        key: TagId,
        value: Value,
        _lsn: u64,
    ) -> Result<(), EngineError> {
        if self.object_table.get(oid.to_u64()).is_none() {
            let mut rec = ObjectRecord::new(oid.to_u64());
            rec.set_state(ObjectState::Active);
            self.object_table.insert(rec);
        }
        let local32 = (oid.to_u64() & 0xffff_ffff) as u32;
        self.forward_index.add_assertion(
            oid,
            Assertion::Attr {
                key,
                value: value.clone(),
            },
            TagOrigin::Direct,
        );
        self.kv_index.insert(key, &value, local32);
        self.range_index.insert(key, &value, local32);
        Ok(())
    }

    pub(crate) fn replay_remove_attr_by_hash(
        &mut self,
        oid: ObjectId,
        key: TagId,
        value_hash: u64,
        _lsn: u64,
    ) -> Result<(), EngineError> {
        let local32 = (oid.to_u64() & 0xffff_ffff) as u32;
        let assertions = self.forward_index.assertions_of(oid).to_vec();
        for (a, _) in &assertions {
            if let Assertion::Attr { key: k, value } = a
                && *k == key
                && engine_value_hash(value) == value_hash
            {
                self.forward_index.remove_assertion(
                    oid,
                    &Assertion::Attr {
                        key,
                        value: value.clone(),
                    },
                );
                self.kv_index.remove(key, value, local32);
                self.range_index.remove(key, value, local32);
                break;
            }
        }
        Ok(())
    }

    pub(crate) fn replay_add_relation(
        &mut self,
        oid: ObjectId,
        predicate: TagId,
        target: ObjectId,
        _lsn: u64,
    ) -> Result<(), EngineError> {
        if self.object_table.get(oid.to_u64()).is_none() {
            let mut rec = ObjectRecord::new(oid.to_u64());
            rec.set_state(ObjectState::Active);
            self.object_table.insert(rec);
        }
        self.forward_index.add_assertion(
            oid,
            Assertion::Relation { predicate, target },
            TagOrigin::Direct,
        );
        Ok(())
    }

    pub(crate) fn replay_remove_relation(
        &mut self,
        oid: ObjectId,
        predicate: TagId,
        target: ObjectId,
        _lsn: u64,
    ) -> Result<(), EngineError> {
        self.forward_index
            .remove_assertion(oid, &Assertion::Relation { predicate, target });
        Ok(())
    }

    pub(crate) fn replay_write_blob(
        &mut self,
        oid: ObjectId,
        content_hash: [u8; 32],
        size: u64,
        _lsn: u64,
    ) -> Result<(), EngineError> {
        if self.object_table.get(oid.to_u64()).is_none() {
            let mut rec = ObjectRecord::new(oid.to_u64());
            rec.set_state(ObjectState::Active);
            self.object_table.insert(rec);
        }
        if let Some(rec) = self.object_table.get_mut(oid.to_u64()) {
            rec.content_hash = content_hash;
            rec.blob_length = size;
            rec.stored_size = size;
        }
        Ok(())
    }

    // ----------------------------------------------------------------------
    // Helpers for tests / external callers (not part of the durable surface).
    // ----------------------------------------------------------------------

    /// Borrow the highest local sequence number issued so far.
    pub fn next_oid_local(&self) -> u64 {
        self.next_oid_local
    }

    /// Number of objects currently in the table (active + tombstoned).
    pub fn object_count(&self) -> usize {
        self.object_table.len()
    }

    /// Quickly probe whether a tag has at least one member object.
    pub fn tag_has_members(&self, tag: TagId) -> bool {
        self.tag_index
            .get(tag)
            .is_some_and(|s| !s.members().is_empty())
    }

    /// Diagnostic — total bytes the object table would occupy if flushed
    /// without overflow records.
    pub fn object_table_bytes(&self) -> usize {
        self.object_table.len() * OBJECT_RECORD_SIZE
    }
}
