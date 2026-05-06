//! [`SubscriptionEngine`] — DESIGN §11.3.
//!
//! Maintains an inverted `tag → [subscription_id]` index and dispatches
//! [`WatchEvent`]s to per-subscription pending queues. The engine layer
//! (Phase 5) wires the executor and the WAL replay logic; this crate is
//! purely the in-memory bookkeeping.
//!
//! ## Persistence (R1b-9)
//!
//! On disk the engine occupies one 256 KiB §1.5 B+ tree region of
//! [`BtreeKind::Subscriptions`]. The whole [`PersistedEngine`]
//! (`next_id` plus the sorted subscription list with their cached
//! roaring-bitmap results) is materialised into a single CBOR-encoded
//! sorted-run entry keyed by `u32 snapshot` (always `0` today; R6 will
//! populate older snapshots).
//!
//! TODO(rewrite-phase-R1d): replace the single-entry blob with the IMPL
//! §10.2 native per-subscription shape — a sorted run keyed by
//! `(SubscriptionId, snapshot)` with one [`PersistedSub`] per entry, so
//! mutating a single subscription doesn't rewrite the whole region.

use std::collections::{BTreeSet, HashMap, VecDeque};

use mimisbrunnr_index::RoaringBitmap;
use mimisbrunnr_ontology::OntologyState;
use mimisbrunnr_storage::{BlockDevice, BtreeKind, BtreeRegion, LoadedNode, SortedRun};
use mimisbrunnr_types::{
    ChangeInterest, HybridTimestamp, NodeId, ObjectId, Query, SubscriptionId, SubscriptionState,
    TagId, WatchEvent,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::WatchError,
    event::EventKind,
    subscription::{Retention, Subscription},
};

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // consumed by engine layout once R1b lands engine-side.
pub const SUBSCRIPTIONS_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

/// Truncate an [`ObjectId`] to a `u32` for storage in a roaring bitmap. The
/// rest of the codebase uses the same convention (low 32 bits) — see
/// `mimisbrunnr-index::tag_store`.
#[inline]
fn oid_to_u32(oid: ObjectId) -> u32 {
    (oid.to_u64() & 0xffff_ffff) as u32
}

/// Build a `HybridTimestamp` from a wall-clock nanosecond reading. The
/// engine layer can override `node_id` later if it needs to attribute
/// events to a specific cluster node — for this crate's mutation hooks the
/// node id is `0` (matches the `HybridTimestamp::zero()` sentinel).
#[inline]
fn hlc(ts_ns: i64) -> HybridTimestamp {
    HybridTimestamp::new(ts_ns, 0, 0 as NodeId)
}

/// Recursive tag extractor over a [`Query`] (DESIGN §11.3 inverted index
/// maintenance). Pulls every `TagId` that appears anywhere in the query
/// tree — `HasTag`, `HasAttr.key`, `Related.predicate`, `IsA(t)`, plus all
/// children of `And` / `Or` / `Not`.
pub fn extract_tags(query: &Query) -> Vec<TagId> {
    let mut out: BTreeSet<TagId> = BTreeSet::new();
    walk(query, &mut out);
    out.into_iter().collect()
}

fn walk(query: &Query, out: &mut BTreeSet<TagId>) {
    match query {
        Query::HasTag(t) | Query::IsA(t) => {
            out.insert(*t);
        }
        Query::HasAttr { key, .. } => {
            out.insert(*key);
        }
        Query::Related { predicate, .. } => {
            out.insert(*predicate);
        }
        Query::And(qs) | Query::Or(qs) => {
            for q in qs {
                walk(q, out);
            }
        }
        Query::Not(inner) => walk(inner, out),
    }
}

/// Inverted-index-driven dispatcher. DESIGN §11.3.
#[derive(Debug, Default)]
pub struct SubscriptionEngine {
    pub subscriptions: HashMap<SubscriptionId, Subscription>,
    pub tag_to_subs: HashMap<TagId, Vec<SubscriptionId>>,
    pub pending_events: HashMap<SubscriptionId, VecDeque<WatchEvent>>,
    pub next_id: SubscriptionId,
}

impl SubscriptionEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new subscription. DESIGN §11.4 atomic subscribe + snapshot.
    ///
    /// `initial_result` is the result of evaluating `query` *at the same LSN
    /// that the engine layer sampled `current_lsn` from*. Any mutation
    /// recorded after `current_lsn` will be delivered as an event.
    pub fn register(
        &mut self,
        name: String,
        query: Query,
        interest: ChangeInterest,
        retention: Retention,
        initial_result: RoaringBitmap,
        current_lsn: u64,
    ) -> SubscriptionId {
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;

        // Inverted index: every tag mentioned in the query points at this sub.
        for tag in extract_tags(&query) {
            self.tag_to_subs.entry(tag).or_default().push(id);
        }

        let sub = Subscription::new(
            id,
            name,
            query,
            interest,
            retention,
            current_lsn,
            initial_result,
        );
        self.subscriptions.insert(id, sub);
        self.pending_events.insert(id, VecDeque::new());
        id
    }

    /// Unregister a subscription. Cleans the inverted index and the pending
    /// queue.
    pub fn unregister(&mut self, id: SubscriptionId) -> Result<(), WatchError> {
        let sub = self
            .subscriptions
            .remove(&id)
            .ok_or(WatchError::UnknownSubscription(id))?;
        for tag in extract_tags(&sub.query) {
            if let Some(list) = self.tag_to_subs.get_mut(&tag) {
                list.retain(|sid| *sid != id);
                if list.is_empty() {
                    self.tag_to_subs.remove(&tag);
                }
            }
        }
        self.pending_events.remove(&id);
        Ok(())
    }

    /// Pause a live subscription (DESIGN §11.7 Active → Dormant).
    pub fn pause(&mut self, id: SubscriptionId) {
        if let Some(sub) = self.subscriptions.get_mut(&id) {
            sub.state = SubscriptionState::Dormant;
        }
    }

    /// Resume a dormant subscription. Live events flow again from the
    /// current cursor position.
    pub fn resume(&mut self, id: SubscriptionId) {
        if let Some(sub) = self.subscriptions.get_mut(&id) {
            sub.state = SubscriptionState::Active;
        }
    }

    /// Pull-style consumer API: drain and clear the pending queue.
    pub fn drain(&mut self, id: SubscriptionId) -> Vec<WatchEvent> {
        match self.pending_events.get_mut(&id) {
            Some(q) => q.drain(..).collect(),
            None => Vec::new(),
        }
    }

    /// Set the cursor (LSN of last delivered event). Used by the engine
    /// layer when it has finished replaying the WAL during catch-up.
    // TODO(rewrite-phase-N): wire WAL scan in engine.
    pub fn set_cursor(&mut self, id: SubscriptionId, lsn: u64) -> Result<(), WatchError> {
        let sub = self
            .subscriptions
            .get_mut(&id)
            .ok_or(WatchError::UnknownSubscription(id))?;
        sub.cursor = lsn;
        Ok(())
    }

    /// Refresh the inverted index against a new ontology snapshot
    /// (DESIGN §11.8 ontology-aware subscriptions). Re-derives every
    /// subscription's tag set — picking up any newly-installed implications
    /// for `IsA(t)` clauses.
    pub fn refresh_ontology_links(&mut self, ontology: &OntologyState) {
        self.tag_to_subs.clear();
        // Snapshot ids — we mutate `tag_to_subs` while iterating subs.
        let ids: Vec<SubscriptionId> = self.subscriptions.keys().copied().collect();
        for id in ids {
            let query = self.subscriptions[&id].query.clone();
            for tag in tags_for_indexing(&query, ontology) {
                self.tag_to_subs.entry(tag).or_default().push(id);
            }
        }
    }

    // ------------------------------------------------------------------
    // Mutation hooks. Called by the engine layer as objects mutate.
    // ------------------------------------------------------------------

    /// Tag added to `oid`. For each subscription whose query references
    /// `tag`, recompute membership and emit the appropriate event.
    pub fn on_tag_added(&mut self, oid: ObjectId, tag: TagId, lsn: u64, ts_ns: i64) {
        let sub_ids = self.tag_to_subs.get(&tag).cloned().unwrap_or_default();
        let timestamp = hlc(ts_ns);
        for sid in sub_ids {
            self.handle_tag_added(sid, oid, tag, lsn, timestamp);
        }
    }

    fn handle_tag_added(
        &mut self,
        sid: SubscriptionId,
        oid: ObjectId,
        tag: TagId,
        lsn: u64,
        timestamp: HybridTimestamp,
    ) {
        let Some(sub) = self.subscriptions.get_mut(&sid) else {
            return;
        };
        if !sub.is_active() {
            return;
        }
        let key = oid_to_u32(oid);
        let was_member = sub.cached_result.contains(key);
        // Phase 4b: for `HasTag(t)` (and queries like `IsA(t)` / `Related`
        // where this single tag is sufficient), adding the tag transitions
        // the object into the matching set. For arbitrary boolean queries,
        // the engine layer is expected to call `set_membership` directly.
        // TODO(rewrite-phase-N): full query re-evaluation in this hook.
        if !was_member {
            sub.cached_result.insert(key);
            emit(
                sub,
                self.pending_events.entry(sid).or_default(),
                lsn,
                ChangeInterest::ENTERED,
                WatchEvent::Entered { oid, timestamp },
            );
        } else {
            emit(
                sub,
                self.pending_events.entry(sid).or_default(),
                lsn,
                ChangeInterest::TAG_ADDED,
                WatchEvent::TagAdded { oid, tag, timestamp },
            );
        }
    }

    /// Tag removed from `oid`. Mirrors [`Self::on_tag_added`].
    pub fn on_tag_removed(&mut self, oid: ObjectId, tag: TagId, lsn: u64, ts_ns: i64) {
        let sub_ids = self.tag_to_subs.get(&tag).cloned().unwrap_or_default();
        let timestamp = hlc(ts_ns);
        for sid in sub_ids {
            self.handle_tag_removed(sid, oid, tag, lsn, timestamp);
        }
    }

    fn handle_tag_removed(
        &mut self,
        sid: SubscriptionId,
        oid: ObjectId,
        tag: TagId,
        lsn: u64,
        timestamp: HybridTimestamp,
    ) {
        let Some(sub) = self.subscriptions.get_mut(&sid) else {
            return;
        };
        if !sub.is_active() {
            return;
        }
        let key = oid_to_u32(oid);
        let was_member = sub.cached_result.contains(key);
        if was_member {
            // Conservative for `HasTag(t)`: removing the watched tag means
            // the object no longer matches. Boolean queries must be
            // re-evaluated by the engine layer.
            // TODO(rewrite-phase-N): full query re-evaluation in this hook.
            sub.cached_result.remove(key);
            // Emit TagRemoved first (the spec lets a sub care about both),
            // then Exited, but masked individually by `interest`.
            let queue = self.pending_events.entry(sid).or_default();
            if sub.interest.contains(ChangeInterest::TAG_REMOVED) {
                queue.push_back(WatchEvent::TagRemoved { oid, tag, timestamp });
                apply_retention(sub, queue);
                sub.cursor = lsn;
            }
            if sub.interest.contains(ChangeInterest::EXITED) {
                queue.push_back(WatchEvent::Exited { oid, timestamp });
                apply_retention(sub, queue);
                sub.cursor = lsn;
            }
        }
    }

    /// New object created. Visible to every subscription whose query is
    /// already satisfied by this object — but with no per-object tag info
    /// available, we emit `Created` to every subscription that requested
    /// it. (The engine layer disambiguates with the proper executor.)
    pub fn on_object_created(&mut self, oid: ObjectId, lsn: u64, ts_ns: i64) {
        let timestamp = hlc(ts_ns);
        let ids: Vec<SubscriptionId> = self.subscriptions.keys().copied().collect();
        for sid in ids {
            let Some(sub) = self.subscriptions.get_mut(&sid) else {
                continue;
            };
            if !sub.is_active() {
                continue;
            }
            emit(
                sub,
                self.pending_events.entry(sid).or_default(),
                lsn,
                ChangeInterest::CREATED,
                WatchEvent::Created { oid, timestamp },
            );
        }
    }

    /// Object deleted. Emits `Deleted` to every active subscription that
    /// currently lists `oid` as a member, and removes it from each
    /// `cached_result`.
    pub fn on_object_deleted(&mut self, oid: ObjectId, lsn: u64, ts_ns: i64) {
        let timestamp = hlc(ts_ns);
        let key = oid_to_u32(oid);
        let ids: Vec<SubscriptionId> = self.subscriptions.keys().copied().collect();
        for sid in ids {
            let Some(sub) = self.subscriptions.get_mut(&sid) else {
                continue;
            };
            if !sub.is_active() {
                continue;
            }
            if sub.cached_result.contains(key) {
                sub.cached_result.remove(key);
                emit(
                    sub,
                    self.pending_events.entry(sid).or_default(),
                    lsn,
                    ChangeInterest::DELETED,
                    WatchEvent::Deleted { oid, timestamp },
                );
            }
        }
    }

    /// Object content changed. Visible only to subscriptions where `oid` is
    /// already in the matching set.
    pub fn on_content_changed(&mut self, oid: ObjectId, lsn: u64, ts_ns: i64) {
        let timestamp = hlc(ts_ns);
        let key = oid_to_u32(oid);
        let ids: Vec<SubscriptionId> = self.subscriptions.keys().copied().collect();
        for sid in ids {
            let Some(sub) = self.subscriptions.get_mut(&sid) else {
                continue;
            };
            if !sub.is_active() {
                continue;
            }
            if sub.cached_result.contains(key) {
                emit(
                    sub,
                    self.pending_events.entry(sid).or_default(),
                    lsn,
                    ChangeInterest::CONTENT_CHANGED,
                    WatchEvent::ContentChanged { oid, timestamp },
                );
            }
        }
    }

    /// Override the membership bitmap for a subscription. The engine layer
    /// uses this when it has re-evaluated a complex boolean query against a
    /// fresh state and wants the watch crate's cached_result to match.
    pub fn set_membership(
        &mut self,
        id: SubscriptionId,
        new_members: RoaringBitmap,
    ) -> Result<(), WatchError> {
        let sub = self
            .subscriptions
            .get_mut(&id)
            .ok_or(WatchError::UnknownSubscription(id))?;
        sub.cached_result = new_members;
        Ok(())
    }

    /// Advance per-subscription debounce timers. Phase 4b: a no-op shim;
    /// debouncing flushes are left as TODO. Provided so that callers can
    /// wire it without reaching into private state later.
    // TODO(rewrite-phase-N): implement per-sub debounce flush here.
    pub fn tick(&mut self, _now_ns: i64) {}

    // ------------------------------------------------------------------
    // CBOR persistence (placeholder for the §10.2 B+ tree).
    // ------------------------------------------------------------------

    /// Serialise the durable engine state. Skips the in-memory
    /// `pending_events` queue (DESIGN §11.5: pending events are recovered by
    /// WAL replay, not by replaying a snapshot).
    pub fn serialise(&self) -> Result<Vec<u8>, WatchError> {
        let snap = self.snapshot()?;
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&snap, &mut buf)
            .map_err(|e| WatchError::CborEncode(e.to_string()))?;
        Ok(buf)
    }

    /// Restore from a CBOR snapshot.
    pub fn deserialise(bytes: &[u8]) -> Result<Self, WatchError> {
        let snap: PersistedEngine = ciborium::de::from_reader(bytes)
            .map_err(|e| WatchError::CborDecode(e.to_string()))?;
        snap.into_engine()
    }

    // ----------------------------------------------------------------
    // R1b-9: §1.5 B+ tree persistence (single-entry CBOR run).
    // ----------------------------------------------------------------

    /// Build a [`LoadedNode`] containing a single sorted-run entry
    /// `(snapshot=0, PersistedEngine)`. The node uses
    /// [`BtreeKind::Subscriptions`] and the spec's 18-bit (256 KiB)
    /// region size.
    pub fn to_loaded_node(&self) -> Result<LoadedNode<u32, PersistedEngine>, WatchError> {
        let snap = self.snapshot()?;
        let entries = vec![(0u32, snap)];

        let mut node: LoadedNode<u32, PersistedEngine> =
            LoadedNode::new(BtreeKind::Subscriptions, 0, REGION_SIZE_LOG2);
        let run = SortedRun::from_sorted(0, 0, entries);
        node.sorted_runs.push(run);
        node.header.sorted_run_count = 1;
        Ok(node)
    }

    /// Restore the in-memory state from a [`LoadedNode`] read via
    /// [`BtreeRegion::read`]. Picks the entry under `snapshot = 0`; an
    /// empty node returns the default.
    pub fn from_loaded_node(
        node: &LoadedNode<u32, PersistedEngine>,
    ) -> Result<Self, WatchError> {
        for (k, v) in node.merge_iter() {
            if *k == 0 {
                return v.clone().into_engine();
            }
        }
        Ok(Self::default())
    }

    /// Write the in-memory state as a fresh 256 KiB region at byte
    /// `offset` on `device`.
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<(), WatchError> {
        let mut node = self.to_loaded_node()?;
        BtreeRegion::write_full::<D, u32, PersistedEngine>(device, offset, &mut node)
            .map_err(|e| WatchError::CborEncode(e.to_string()))?;
        Ok(())
    }

    /// Read the in-memory state from the 256 KiB region at byte
    /// `offset` on `device`. An all-zero region returns
    /// [`Self::default`].
    pub fn load_from_region<D: BlockDevice>(
        device: &D,
        offset: u64,
    ) -> Result<Self, WatchError> {
        let mut probe = [0u8; 8];
        device
            .read_at(offset, &mut probe)
            .map_err(|e| WatchError::CborDecode(e.to_string()))?;
        if probe.iter().all(|&b| b == 0) {
            return Ok(Self::default());
        }
        let node = BtreeRegion::read::<D, u32, PersistedEngine>(
            device,
            offset,
            BtreeKind::Subscriptions,
        )
        .map_err(|e| WatchError::CborDecode(e.to_string()))?;
        Self::from_loaded_node(&node)
    }

    fn snapshot(&self) -> Result<PersistedEngine, WatchError> {
        let mut subs: Vec<PersistedSub> = Vec::with_capacity(self.subscriptions.len());
        for sub in self.subscriptions.values() {
            let mut bitmap_bytes = Vec::with_capacity(sub.cached_result.serialized_size());
            sub.cached_result
                .serialize_into(&mut bitmap_bytes)
                .map_err(|e| WatchError::Bitmap(e.to_string()))?;
            subs.push(PersistedSub {
                id: sub.id,
                name: sub.name.clone(),
                query: sub.query.clone(),
                interest: sub.interest,
                cursor: sub.cursor,
                state: sub.state,
                retention: sub.retention,
                debounce_ms: sub.debounce_ms,
                cached_result: bitmap_bytes,
            });
        }
        subs.sort_by_key(|s| s.id);
        Ok(PersistedEngine {
            next_id: self.next_id,
            subscriptions: subs,
        })
    }
}

// -------------------------------------------------------------------------
// Helpers.
// -------------------------------------------------------------------------

/// Push `event` onto `queue` if `interest` includes `kind`. Applies the
/// subscription's retention policy and bumps the cursor.
fn emit(
    sub: &mut Subscription,
    queue: &mut VecDeque<WatchEvent>,
    lsn: u64,
    kind: ChangeInterest,
    event: WatchEvent,
) {
    if !sub.interest.contains(kind) {
        return;
    }
    queue.push_back(event);
    apply_retention(sub, queue);
    sub.cursor = lsn;
}

fn apply_retention(sub: &Subscription, queue: &mut VecDeque<WatchEvent>) {
    match sub.retention {
        Retention::Unlimited => {}
        Retention::AtMostOnce => {
            // Drop the *oldest* if anything would queue beyond 1.
            while queue.len() > 1 {
                queue.pop_front();
            }
        }
        Retention::Bounded { max_events } => {
            let cap = max_events as usize;
            while queue.len() > cap {
                queue.pop_front();
            }
        }
    }
}

/// Tags to index a subscription against, taking ontology closures into
/// account for `IsA(t)` clauses (DESIGN §11.8). For non-`IsA` clauses this
/// is identical to [`extract_tags`].
fn tags_for_indexing(query: &Query, ontology: &OntologyState) -> Vec<TagId> {
    let mut out: BTreeSet<TagId> = BTreeSet::new();
    walk_with_ontology(query, ontology, &mut out);
    out.into_iter().collect()
}

fn walk_with_ontology(query: &Query, ontology: &OntologyState, out: &mut BTreeSet<TagId>) {
    match query {
        Query::HasTag(t) => {
            out.insert(*t);
        }
        Query::IsA(t) => {
            // Closure includes `t` itself plus every tag whose implication
            // chain ends at `t` ("car implies vehicle" → `IsA(vehicle)`
            // matches both `car` and `vehicle`).
            //
            // `ImplicationDag::closure(&[t])` walks *outgoing* edges
            // (descendants), but for `IsA(vehicle)` we want every tag that
            // has `vehicle` as an ancestor — so we walk the implication
            // table looking for tags that satisfy `is_a(other, t)`.
            out.insert(*t);
            for other in ontology.dag.tags() {
                if other != *t && ontology.dag.is_a(other, *t) {
                    out.insert(other);
                }
            }
        }
        Query::HasAttr { key, .. } => {
            out.insert(*key);
        }
        Query::Related { predicate, .. } => {
            out.insert(*predicate);
        }
        Query::And(qs) | Query::Or(qs) => {
            for q in qs {
                walk_with_ontology(q, ontology, out);
            }
        }
        Query::Not(inner) => walk_with_ontology(inner, ontology, out),
    }
}

// -------------------------------------------------------------------------
// CBOR persistence shapes.
// -------------------------------------------------------------------------

/// Serialised snapshot of the engine. Public because it appears in the
/// R1b-9 [`SubscriptionEngine::to_loaded_node`] /
/// [`SubscriptionEngine::from_loaded_node`] signatures; callers normally
/// only use those indirectly via `flush_to_region` / `load_from_region`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedEngine {
    /// Next subscription id to allocate.
    pub next_id: SubscriptionId,
    /// Subscriptions sorted by id.
    pub subscriptions: Vec<PersistedSub>,
}

/// Serialised single subscription. Mirrors [`Subscription`] modulo the
/// roaring-bitmap proxy (`cached_result` carries the
/// `RoaringBitmap::serialize_into` byte image).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSub {
    /// Subscription id.
    pub id: SubscriptionId,
    /// Human-readable name.
    pub name: String,
    /// Query AST.
    pub query: Query,
    /// Subscribed change kinds.
    pub interest: ChangeInterest,
    /// Last delivered LSN.
    pub cursor: u64,
    /// Subscription state.
    pub state: SubscriptionState,
    /// Retention policy.
    pub retention: Retention,
    /// Debounce window in milliseconds (`None` = no debounce).
    pub debounce_ms: Option<u32>,
    /// Raw roaring-bitmap bytes (its built-in serializer handles run-length
    /// containers etc. — using CBOR's array-of-u32 would balloon the size).
    pub cached_result: Vec<u8>,
}

impl PersistedEngine {
    fn into_engine(self) -> Result<SubscriptionEngine, WatchError> {
        let mut engine = SubscriptionEngine {
            next_id: self.next_id,
            ..SubscriptionEngine::default()
        };
        for ps in self.subscriptions {
            let cached_result = RoaringBitmap::deserialize_from(ps.cached_result.as_slice())
                .map_err(|e| WatchError::Bitmap(e.to_string()))?;
            for tag in extract_tags(&ps.query) {
                engine.tag_to_subs.entry(tag).or_default().push(ps.id);
            }
            let sub = Subscription {
                id: ps.id,
                name: ps.name,
                query: ps.query,
                interest: ps.interest,
                cursor: ps.cursor,
                state: ps.state,
                retention: ps.retention,
                debounce_ms: ps.debounce_ms,
                cached_result,
            };
            engine.subscriptions.insert(ps.id, sub);
            engine.pending_events.insert(ps.id, VecDeque::new());
        }
        Ok(engine)
    }
}

// -------------------------------------------------------------------------
// Re-export for tests / consumers that build events manually.
// -------------------------------------------------------------------------

#[allow(dead_code)]
#[doc(hidden)]
pub fn _event_kind_for(ev: &WatchEvent) -> EventKind {
    EventKind::from(ev)
}

#[cfg(test)]
mod tests {
    use mimisbrunnr_ontology::{IdAllocator, OntologyModule, TagDefinition, TagSemantics};

    use super::*;

    fn t(id: u32) -> TagId {
        TagId::new(id)
    }

    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0, local)
    }

    fn fresh() -> SubscriptionEngine {
        SubscriptionEngine::new()
    }

    fn label(name: &str) -> TagDefinition {
        TagDefinition {
            id: TagId::new(0),
            name: name.into(),
            semantics: TagSemantics::Label,
            implies: vec![],
            storage: None,
        }
    }

    #[test]
    fn extract_tags_complex_query() {
        let q = Query::And(vec![
            Query::HasTag(t(1)),
            Query::Or(vec![
                Query::HasTag(t(2)),
                Query::Not(Box::new(Query::HasTag(t(3)))),
            ]),
        ]);
        assert_eq!(extract_tags(&q), vec![t(1), t(2), t(3)]);
    }

    #[test]
    fn extract_tags_covers_all_variants() {
        let q = Query::And(vec![
            Query::HasTag(t(10)),
            Query::HasAttr {
                key: t(20),
                op: mimisbrunnr_types::CmpOp::Eq,
                value: mimisbrunnr_types::Value::Int(1),
            },
            Query::Related {
                predicate: t(30),
                target: oid(99),
            },
            Query::IsA(t(40)),
        ]);
        assert_eq!(extract_tags(&q), vec![t(10), t(20), t(30), t(40)]);
    }

    #[test]
    fn register_populates_inverted_index() {
        let mut e = fresh();
        let id = e.register(
            "all-a".into(),
            Query::HasTag(t(1)),
            ChangeInterest::ALL,
            Retention::default(),
            RoaringBitmap::new(),
            0,
        );
        assert_eq!(e.tag_to_subs.get(&t(1)), Some(&vec![id]));
    }

    #[test]
    fn tag_added_emits_entered_with_correct_payload() {
        let mut e = fresh();
        let id = e.register(
            "watch-a".into(),
            Query::HasTag(t(1)),
            ChangeInterest::ALL,
            Retention::default(),
            RoaringBitmap::new(),
            0,
        );
        e.on_tag_added(oid(7), t(1), 100, 1_700_000_000_000);
        let events = e.drain(id);
        assert_eq!(events.len(), 1);
        match &events[0] {
            WatchEvent::Entered { oid: o, timestamp } => {
                assert_eq!(*o, oid(7));
                assert_eq!(timestamp.physical_ns, 1_700_000_000_000);
            }
            other => panic!("expected Entered, got {other:?}"),
        }
        // Cursor advanced to the LSN of the delivered event.
        assert_eq!(e.subscriptions[&id].cursor, 100);
    }

    #[test]
    fn tag_removed_emits_exited() {
        let mut e = fresh();
        let mut initial = RoaringBitmap::new();
        initial.insert(oid_to_u32(oid(7)));
        let id = e.register(
            "watch-a".into(),
            Query::HasTag(t(1)),
            ChangeInterest::ALL,
            Retention::default(),
            initial,
            0,
        );
        e.on_tag_removed(oid(7), t(1), 200, 0);
        let events = e.drain(id);
        // ALL covers TAG_REMOVED + EXITED → both delivered.
        assert!(events.iter().any(|ev| matches!(ev, WatchEvent::TagRemoved { .. })));
        assert!(events.iter().any(|ev| matches!(ev, WatchEvent::Exited { .. })));
    }

    #[test]
    fn change_interest_masks_exited() {
        let mut e = fresh();
        let mut initial = RoaringBitmap::new();
        initial.insert(oid_to_u32(oid(7)));
        let id = e.register(
            "entered-only".into(),
            Query::HasTag(t(1)),
            ChangeInterest::ENTERED, // explicitly NOT EXITED / TAG_REMOVED
            Retention::default(),
            initial,
            0,
        );
        e.on_tag_removed(oid(7), t(1), 200, 0);
        let events = e.drain(id);
        assert!(events.is_empty(), "got unexpected events: {events:?}");
    }

    #[test]
    fn drain_clears_queue() {
        let mut e = fresh();
        let id = e.register(
            "x".into(),
            Query::HasTag(t(1)),
            ChangeInterest::ALL,
            Retention::default(),
            RoaringBitmap::new(),
            0,
        );
        e.on_tag_added(oid(1), t(1), 1, 0);
        e.on_tag_added(oid(2), t(1), 2, 0);
        assert_eq!(e.drain(id).len(), 2);
        assert!(e.drain(id).is_empty());
    }

    #[test]
    fn pause_blocks_events_resume_unblocks() {
        let mut e = fresh();
        let id = e.register(
            "x".into(),
            Query::HasTag(t(1)),
            ChangeInterest::ALL,
            Retention::default(),
            RoaringBitmap::new(),
            0,
        );
        e.pause(id);
        e.on_tag_added(oid(1), t(1), 5, 0);
        assert!(e.drain(id).is_empty());
        e.resume(id);
        e.on_tag_added(oid(2), t(1), 6, 0);
        let events = e.drain(id);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn unregister_cleans_inverted_index() {
        let mut e = fresh();
        let id = e.register(
            "x".into(),
            Query::HasTag(t(1)),
            ChangeInterest::ALL,
            Retention::default(),
            RoaringBitmap::new(),
            0,
        );
        e.unregister(id).unwrap();
        assert!(!e.tag_to_subs.contains_key(&t(1)));
        // Future events for that tag don't reference the dead id.
        e.on_tag_added(oid(1), t(1), 1, 0);
        assert!(!e.pending_events.contains_key(&id));
    }

    #[test]
    fn unregister_unknown_errors() {
        let mut e = fresh();
        let err = e.unregister(999).unwrap_err();
        assert!(matches!(err, WatchError::UnknownSubscription(999)));
    }

    #[test]
    fn object_created_delivers_to_active_subs() {
        let mut e = fresh();
        let id = e.register(
            "x".into(),
            Query::HasTag(t(1)),
            ChangeInterest::CREATED,
            Retention::default(),
            RoaringBitmap::new(),
            0,
        );
        e.on_object_created(oid(99), 1, 0);
        let events = e.drain(id);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], WatchEvent::Created { .. }));
    }

    #[test]
    fn content_changed_only_for_members() {
        let mut e = fresh();
        let mut initial = RoaringBitmap::new();
        initial.insert(oid_to_u32(oid(1)));
        let id = e.register(
            "x".into(),
            Query::HasTag(t(1)),
            ChangeInterest::CONTENT_CHANGED,
            Retention::default(),
            initial,
            0,
        );
        e.on_content_changed(oid(1), 5, 0);
        e.on_content_changed(oid(2), 6, 0); // not a member
        let events = e.drain(id);
        assert_eq!(events.len(), 1);
        assert_eq!(
            match &events[0] {
                WatchEvent::ContentChanged { oid, .. } => *oid,
                _ => unreachable!(),
            },
            oid(1)
        );
    }

    #[test]
    fn object_deleted_clears_membership() {
        let mut e = fresh();
        let mut initial = RoaringBitmap::new();
        initial.insert(oid_to_u32(oid(1)));
        let id = e.register(
            "x".into(),
            Query::HasTag(t(1)),
            ChangeInterest::DELETED,
            Retention::default(),
            initial,
            0,
        );
        e.on_object_deleted(oid(1), 7, 0);
        let events = e.drain(id);
        assert_eq!(events.len(), 1);
        assert!(!e.subscriptions[&id]
            .cached_result
            .contains(oid_to_u32(oid(1))));
    }

    #[test]
    fn cursor_setter() {
        let mut e = fresh();
        let id = e.register(
            "x".into(),
            Query::HasTag(t(1)),
            ChangeInterest::ALL,
            Retention::default(),
            RoaringBitmap::new(),
            0,
        );
        e.set_cursor(id, 9_999).unwrap();
        assert_eq!(e.subscriptions[&id].cursor, 9_999);
        assert!(matches!(
            e.set_cursor(404, 1),
            Err(WatchError::UnknownSubscription(404))
        ));
    }

    #[test]
    fn retention_bounded_drops_oldest() {
        let mut e = fresh();
        let id = e.register(
            "x".into(),
            Query::HasTag(t(1)),
            ChangeInterest::ALL,
            Retention::Bounded { max_events: 2 },
            RoaringBitmap::new(),
            0,
        );
        for i in 1..=5u64 {
            e.on_tag_added(oid(i), t(1), i, 0);
        }
        let events = e.drain(id);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn cbor_round_trip() {
        let mut e = fresh();
        let mut bm = RoaringBitmap::new();
        bm.insert(1);
        bm.insert(2);
        bm.insert(3);
        let id = e.register(
            "persisted".into(),
            Query::And(vec![Query::HasTag(t(1)), Query::HasTag(t(2))]),
            ChangeInterest::ALL,
            Retention::Bounded { max_events: 8 },
            bm,
            42,
        );
        let bytes = e.serialise().unwrap();
        let back = SubscriptionEngine::deserialise(&bytes).unwrap();
        assert_eq!(back.subscriptions.len(), 1);
        let s = &back.subscriptions[&id];
        assert_eq!(s.cursor, 42);
        assert_eq!(s.cached_result.len(), 3);
        // Inverted index reconstructed.
        assert!(back.tag_to_subs.contains_key(&t(1)));
        assert!(back.tag_to_subs.contains_key(&t(2)));
    }

    #[test]
    fn ontology_aware_indexes_via_isa_closure() {
        // ontology: car implies vehicle; subscription on IsA(vehicle).
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let module = OntologyModule {
            id: "core".into(),
            version: "0.1".into(),
            name: "core".into(),
            tags: vec![label("vehicle"), label("car")],
            implications: vec![("car".into(), "vehicle".into())],
        };
        state.install(module, &mut alloc).unwrap();
        let vehicle = state.names["vehicle"];
        let car = state.names["car"];

        let mut e = fresh();
        let id = e.register(
            "vehicles".into(),
            Query::IsA(vehicle),
            ChangeInterest::ALL,
            Retention::default(),
            RoaringBitmap::new(),
            0,
        );
        e.refresh_ontology_links(&state);
        // Subscription is now indexed against both `car` and `vehicle`.
        assert!(e.tag_to_subs.get(&vehicle).unwrap().contains(&id));
        assert!(e.tag_to_subs.get(&car).unwrap().contains(&id));
    }

    // ----- B+ tree region round-trip (R1b-9) -----

    use mimisbrunnr_storage::FileBlockDevice;
    use tempfile::TempDir;

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("subscriptions.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    #[test]
    fn watch_region_round_trip_empty_returns_default() {
        let (_dir, dev) = fresh_device();
        let e = SubscriptionEngine::load_from_region(&dev, 0).unwrap();
        assert!(e.subscriptions.is_empty());
    }

    #[test]
    fn watch_region_round_trip_preserves_subscriptions() {
        let (_dir, dev) = fresh_device();
        let mut e = fresh();
        let mut bm1 = RoaringBitmap::new();
        bm1.insert(10);
        bm1.insert(11);
        let id1 = e.register(
            "alpha".into(),
            Query::And(vec![Query::HasTag(t(1)), Query::HasTag(t(2))]),
            ChangeInterest::ALL,
            Retention::Bounded { max_events: 8 },
            bm1,
            42,
        );
        let id2 = e.register(
            "beta".into(),
            Query::HasTag(t(3)),
            ChangeInterest::TAG_ADDED,
            Retention::AtMostOnce,
            RoaringBitmap::new(),
            7,
        );
        let next_before = e.next_id;

        e.flush_to_region(&dev, 0).unwrap();
        let back = SubscriptionEngine::load_from_region(&dev, 0).unwrap();

        assert_eq!(back.subscriptions.len(), 2);
        assert_eq!(back.next_id, next_before);
        let s1 = &back.subscriptions[&id1];
        assert_eq!(s1.cursor, 42);
        assert_eq!(s1.cached_result.len(), 2);
        let s2 = &back.subscriptions[&id2];
        assert_eq!(s2.cursor, 7);
        // Inverted tag index reconstructed.
        assert!(back.tag_to_subs.get(&t(1)).unwrap().contains(&id1));
        assert!(back.tag_to_subs.get(&t(3)).unwrap().contains(&id2));
    }

    #[test]
    fn watch_region_round_trip_single_subscription() {
        let (_dir, dev) = fresh_device();
        let mut e = fresh();
        let id = e.register(
            "solo".into(),
            Query::HasTag(t(99)),
            ChangeInterest::ALL,
            Retention::default(),
            RoaringBitmap::new(),
            0,
        );
        e.flush_to_region(&dev, 0).unwrap();
        let back = SubscriptionEngine::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.subscriptions.len(), 1);
        assert!(back.subscriptions.contains_key(&id));
    }

    #[test]
    fn watch_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = fresh();
        first.register(
            "old".into(),
            Query::HasTag(t(1)),
            ChangeInterest::ALL,
            Retention::default(),
            RoaringBitmap::new(),
            0,
        );
        first.flush_to_region(&dev, 0).unwrap();

        let mut second = fresh();
        let id = second.register(
            "fresh".into(),
            Query::HasTag(t(99)),
            ChangeInterest::ALL,
            Retention::default(),
            RoaringBitmap::new(),
            0,
        );
        second.flush_to_region(&dev, 0).unwrap();

        let back = SubscriptionEngine::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.subscriptions.len(), 1);
        assert_eq!(back.subscriptions[&id].name, "fresh");
    }
}
