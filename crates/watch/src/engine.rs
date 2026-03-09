use std::collections::HashMap;

use {
    mimisbrunnr_index::{KvIndex, TagIndex},
    mimisbrunnr_ontology::ImplicationDag,
    mimisbrunnr_query::QueryExecutor,
    mimisbrunnr_types::{HybridTimestamp, ObjectId, Query, SubscriptionId, TagId},
    roaring::RoaringBitmap,
};

use crate::{
    error::WatchError,
    event::WatchEvent,
    subscription::{ChangeInterest, Subscription, SubscriptionState},
};

/// The subscription engine: evaluates mutations against active subscriptions
/// and produces events.
///
/// Subscriptions are indexed by which tags they reference. When tag X changes,
/// only subscriptions mentioning tag X are evaluated — not all subscriptions.
pub struct SubscriptionEngine {
    /// All subscriptions by ID.
    subscriptions: HashMap<SubscriptionId, Subscription>,
    /// Inverted index: tag → subscriptions that reference it.
    tag_to_subs: HashMap<TagId, Vec<SubscriptionId>>,
    /// Pending events per subscription.
    pending_events: HashMap<SubscriptionId, Vec<WatchEvent>>,
    /// Next subscription ID.
    next_id: SubscriptionId,
}

impl SubscriptionEngine {
    pub fn new() -> Self {
        Self {
            subscriptions: HashMap::new(),
            tag_to_subs: HashMap::new(),
            pending_events: HashMap::new(),
            next_id: 1,
        }
    }

    /// Atomic subscribe + snapshot: register a subscription and return the
    /// initial result set. No events can be missed between registration and
    /// the snapshot.
    #[expect(clippy::too_many_arguments)]
    pub fn subscribe(
        &mut self,
        name: String,
        query: Query,
        interest: ChangeInterest,
        tag_index: &TagIndex,
        kv_index: &KvIndex,
        dag: &ImplicationDag,
        current_lsn: u64,
    ) -> Result<(SubscriptionId, Vec<u32>), WatchError> {
        let id = self.next_id;
        self.next_id += 1;

        // Execute query to get initial result set
        let executor = QueryExecutor::new(tag_index, kv_index, dag);
        let initial_bm = executor.execute(&query);
        let initial_vec: Vec<u32> = initial_bm.iter().collect();

        // Index subscription by referenced tags
        let tags = extract_tags(&query);
        for tag in &tags {
            self.tag_to_subs.entry(*tag).or_default().push(id);
        }

        let sub = Subscription::new(id, name, query, interest, current_lsn, initial_bm);
        self.subscriptions.insert(id, sub);
        self.pending_events.insert(id, Vec::new());

        Ok((id, initial_vec))
    }

    /// Notify the engine that a tag was added to an object.
    /// Evaluates affected subscriptions and generates events.
    pub fn notify_tag_added(
        &mut self,
        oid: ObjectId,
        tag: TagId,
        timestamp: HybridTimestamp,
        tag_index: &TagIndex,
        kv_index: &KvIndex,
        dag: &ImplicationDag,
    ) {
        let obj_local = oid.local() as u32;
        let affected = self.subscriptions_for_tag(tag);

        for sub_id in affected {
            let sub = match self.subscriptions.get_mut(&sub_id) {
                Some(s) if s.is_active() => s,
                _ => continue,
            };

            let was_member = sub.cached_result.contains(obj_local);

            // Re-evaluate if this object matches the query
            let executor = QueryExecutor::new(tag_index, kv_index, dag);
            let now_matches = executor.execute(&sub.query).contains(obj_local);

            if now_matches && !was_member {
                // Object entered the result set
                sub.cached_result.insert(obj_local);
                if sub.interest.contains(ChangeInterest::ENTERED) {
                    self.pending_events
                        .entry(sub_id)
                        .or_default()
                        .push(WatchEvent::Entered { oid, timestamp });
                }
            } else if now_matches && was_member {
                // Object was already in set — just a tag add
                if sub.interest.contains(ChangeInterest::TAG_ADDED) {
                    self.pending_events
                        .entry(sub_id)
                        .or_default()
                        .push(WatchEvent::TagAdded {
                            oid,
                            tag,
                            timestamp,
                        });
                }
            }
        }
    }

    /// Notify the engine that a tag was removed from an object.
    pub fn notify_tag_removed(
        &mut self,
        oid: ObjectId,
        tag: TagId,
        timestamp: HybridTimestamp,
        tag_index: &TagIndex,
        kv_index: &KvIndex,
        dag: &ImplicationDag,
    ) {
        let obj_local = oid.local() as u32;
        let affected = self.subscriptions_for_tag(tag);

        for sub_id in affected {
            let sub = match self.subscriptions.get_mut(&sub_id) {
                Some(s) if s.is_active() => s,
                _ => continue,
            };

            let was_member = sub.cached_result.contains(obj_local);
            if !was_member {
                continue;
            }

            let executor = QueryExecutor::new(tag_index, kv_index, dag);
            let still_matches = executor.execute(&sub.query).contains(obj_local);

            if !still_matches {
                // Object exited the result set
                sub.cached_result.remove(obj_local);
                if sub.interest.contains(ChangeInterest::EXITED) {
                    self.pending_events
                        .entry(sub_id)
                        .or_default()
                        .push(WatchEvent::Exited { oid, timestamp });
                }
            } else if sub.interest.contains(ChangeInterest::TAG_REMOVED) {
                self.pending_events
                    .entry(sub_id)
                    .or_default()
                    .push(WatchEvent::TagRemoved {
                        oid,
                        tag,
                        timestamp,
                    });
            }
        }
    }

    /// Notify that an object was created.
    pub fn notify_created(
        &mut self,
        oid: ObjectId,
        timestamp: HybridTimestamp,
        tag_index: &TagIndex,
        kv_index: &KvIndex,
        dag: &ImplicationDag,
    ) {
        let obj_local = oid.local() as u32;
        let executor = QueryExecutor::new(tag_index, kv_index, dag);

        for (sub_id, sub) in &mut self.subscriptions {
            if !sub.is_active() {
                continue;
            }
            if executor.execute(&sub.query).contains(obj_local) {
                sub.cached_result.insert(obj_local);
                if sub.interest.contains(ChangeInterest::CREATED) {
                    self.pending_events
                        .entry(*sub_id)
                        .or_default()
                        .push(WatchEvent::Created { oid, timestamp });
                }
            }
        }
    }

    /// Notify that an object was deleted.
    pub fn notify_deleted(&mut self, oid: ObjectId, timestamp: HybridTimestamp) {
        let obj_local = oid.local() as u32;

        for (sub_id, sub) in &mut self.subscriptions {
            if !sub.is_active() {
                continue;
            }
            if sub.cached_result.remove(obj_local) && sub.interest.contains(ChangeInterest::DELETED)
            {
                self.pending_events
                    .entry(*sub_id)
                    .or_default()
                    .push(WatchEvent::Deleted { oid, timestamp });
            }
        }
    }

    /// Notify that blob content changed.
    pub fn notify_content_changed(&mut self, oid: ObjectId, timestamp: HybridTimestamp) {
        let obj_local = oid.local() as u32;

        for (sub_id, sub) in &mut self.subscriptions {
            if !sub.is_active() {
                continue;
            }
            if sub.cached_result.contains(obj_local)
                && sub.interest.contains(ChangeInterest::CONTENT_CHANGED)
            {
                self.pending_events
                    .entry(*sub_id)
                    .or_default()
                    .push(WatchEvent::ContentChanged { oid, timestamp });
            }
        }
    }

    /// Drain pending events for a subscription.
    pub fn drain_events(&mut self, sub_id: SubscriptionId) -> Vec<WatchEvent> {
        self.pending_events
            .get_mut(&sub_id)
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Mark a subscription as dormant (agent offline).
    pub fn set_dormant(&mut self, sub_id: SubscriptionId) -> Result<(), WatchError> {
        let sub = self
            .subscriptions
            .get_mut(&sub_id)
            .ok_or(WatchError::NotFound(sub_id))?;
        sub.state = SubscriptionState::Dormant;
        Ok(())
    }

    /// Reactivate a dormant subscription.
    pub fn set_active(&mut self, sub_id: SubscriptionId) -> Result<(), WatchError> {
        let sub = self
            .subscriptions
            .get_mut(&sub_id)
            .ok_or(WatchError::NotFound(sub_id))?;
        sub.state = SubscriptionState::Active;
        Ok(())
    }

    /// Catch up a dormant subscription by diffing current vs cached result set.
    /// Returns (entered, exited) object lists.
    pub fn catch_up(
        &mut self,
        sub_id: SubscriptionId,
        tag_index: &TagIndex,
        kv_index: &KvIndex,
        dag: &ImplicationDag,
    ) -> Result<(Vec<u32>, Vec<u32>), WatchError> {
        let sub = self
            .subscriptions
            .get_mut(&sub_id)
            .ok_or(WatchError::NotFound(sub_id))?;

        let executor = QueryExecutor::new(tag_index, kv_index, dag);
        let current = executor.execute(&sub.query);

        let entered: RoaringBitmap = &current - &sub.cached_result;
        let exited: RoaringBitmap = &sub.cached_result - &current;

        sub.cached_result = current;
        sub.state = SubscriptionState::Active;

        Ok((entered.iter().collect(), exited.iter().collect()))
    }

    /// Remove a subscription.
    pub fn unsubscribe(&mut self, sub_id: SubscriptionId) -> Result<(), WatchError> {
        self.subscriptions
            .remove(&sub_id)
            .ok_or(WatchError::NotFound(sub_id))?;
        self.pending_events.remove(&sub_id);

        // Clean up tag_to_subs
        for subs in self.tag_to_subs.values_mut() {
            subs.retain(|&id| id != sub_id);
        }

        Ok(())
    }

    /// Get a subscription by ID.
    pub fn get(&self, sub_id: SubscriptionId) -> Option<&Subscription> {
        self.subscriptions.get(&sub_id)
    }

    /// Number of active subscriptions.
    pub fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }

    fn subscriptions_for_tag(&self, tag: TagId) -> Vec<SubscriptionId> {
        self.tag_to_subs.get(&tag).cloned().unwrap_or_default()
    }
}

impl Default for SubscriptionEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract all tag IDs referenced by a query (for indexing subscriptions).
fn extract_tags(query: &Query) -> Vec<TagId> {
    let mut tags = Vec::new();
    match query {
        Query::HasTag(t) | Query::IsA(t) => tags.push(*t),
        Query::HasAttr { key, .. } => tags.push(*key),
        Query::Related { predicate, .. } => tags.push(*predicate),
        Query::And(subs) | Query::Or(subs) => {
            for s in subs {
                tags.extend(extract_tags(s));
            }
        }
        Query::Not(inner) => tags.extend(extract_tags(inner)),
    }
    tags
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        mimisbrunnr_index::TagIndex,
        mimisbrunnr_ontology::{ImplicationDag, TagDefinition, TagSemantics},
    };

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    fn oid(local: u64) -> ObjectId {
        ObjectId::new(0, local)
    }

    fn ts(ms: u64) -> HybridTimestamp {
        HybridTimestamp::new(ms, 0, 0)
    }

    struct TestFixture {
        tag_index: TagIndex,
        kv_index: KvIndex,
        dag: ImplicationDag,
        watch_engine: SubscriptionEngine,
    }

    impl TestFixture {
        fn new() -> Self {
            let mut dag = ImplicationDag::new();
            dag.register_tag(TagDefinition::new(tag(1), "source", TagSemantics::Label))
                .unwrap();
            dag.register_tag(TagDefinition::new(tag(2), "test", TagSemantics::Label))
                .unwrap();
            dag.register_tag(TagDefinition::new(
                tag(3),
                "electronic",
                TagSemantics::Label,
            ))
            .unwrap();

            Self {
                tag_index: TagIndex::new(),
                kv_index: KvIndex::new(),
                dag,
                watch_engine: SubscriptionEngine::new(),
            }
        }
    }

    #[test]
    fn subscribe_returns_initial_result() {
        let mut f = TestFixture::new();
        // Pre-populate some objects with tag(1)
        f.tag_index.tag_object(tag(1), 10);
        f.tag_index.tag_object(tag(1), 20);

        let (sub_id, initial) = f
            .watch_engine
            .subscribe(
                "test-watch".into(),
                Query::HasTag(tag(1)),
                ChangeInterest::ALL,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();

        assert_eq!(sub_id, 1);
        assert_eq!(initial, vec![10, 20]);
    }

    #[test]
    fn entered_event_on_tag_add() {
        let mut f = TestFixture::new();

        // Subscribe to tag(1)
        let (sub_id, _) = f
            .watch_engine
            .subscribe(
                "watch".into(),
                Query::HasTag(tag(1)),
                ChangeInterest::ENTERED | ChangeInterest::EXITED,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();

        // Add tag(1) to object 5
        f.tag_index.tag_object(tag(1), 5);
        f.watch_engine.notify_tag_added(
            oid(5),
            tag(1),
            ts(1000),
            &f.tag_index,
            &f.kv_index,
            &f.dag,
        );

        let events = f.watch_engine.drain_events(sub_id);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], WatchEvent::Entered { .. }));
        assert_eq!(events[0].object_id(), oid(5));
    }

    #[test]
    fn exited_event_on_tag_remove() {
        let mut f = TestFixture::new();

        // Object 5 has tag(1)
        f.tag_index.tag_object(tag(1), 5);

        let (sub_id, initial) = f
            .watch_engine
            .subscribe(
                "watch".into(),
                Query::HasTag(tag(1)),
                ChangeInterest::ENTERED | ChangeInterest::EXITED,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();
        assert_eq!(initial, vec![5]);

        // Remove tag(1) from object 5
        f.tag_index.untag_object(tag(1), 5);
        f.watch_engine.notify_tag_removed(
            oid(5),
            tag(1),
            ts(2000),
            &f.tag_index,
            &f.kv_index,
            &f.dag,
        );

        let events = f.watch_engine.drain_events(sub_id);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], WatchEvent::Exited { .. }));
    }

    #[test]
    fn tag_added_event_for_member() {
        let mut f = TestFixture::new();
        f.tag_index.tag_object(tag(1), 5);

        let (sub_id, _) = f
            .watch_engine
            .subscribe(
                "watch".into(),
                Query::HasTag(tag(1)),
                ChangeInterest::TAG_ADDED,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();

        // Add another tag to the same object — it's already in the result set
        f.tag_index.tag_object(tag(2), 5);
        f.watch_engine.notify_tag_added(
            oid(5),
            tag(1),
            ts(1000), // tag(1) re-applied or redundant
            &f.tag_index,
            &f.kv_index,
            &f.dag,
        );

        let events = f.watch_engine.drain_events(sub_id);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], WatchEvent::TagAdded { .. }));
    }

    #[test]
    fn deleted_event() {
        let mut f = TestFixture::new();
        f.tag_index.tag_object(tag(1), 5);

        let (sub_id, _) = f
            .watch_engine
            .subscribe(
                "watch".into(),
                Query::HasTag(tag(1)),
                ChangeInterest::DELETED,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();

        f.watch_engine.notify_deleted(oid(5), ts(2000));

        let events = f.watch_engine.drain_events(sub_id);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], WatchEvent::Deleted { .. }));
    }

    #[test]
    fn content_changed_event() {
        let mut f = TestFixture::new();
        f.tag_index.tag_object(tag(1), 5);

        let (sub_id, _) = f
            .watch_engine
            .subscribe(
                "watch".into(),
                Query::HasTag(tag(1)),
                ChangeInterest::CONTENT_CHANGED,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();

        f.watch_engine.notify_content_changed(oid(5), ts(3000));

        let events = f.watch_engine.drain_events(sub_id);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], WatchEvent::ContentChanged { .. }));
    }

    #[test]
    fn no_events_for_uninterested() {
        let mut f = TestFixture::new();
        f.tag_index.tag_object(tag(1), 5);

        // Only interested in DELETED, not content changes
        let (sub_id, _) = f
            .watch_engine
            .subscribe(
                "watch".into(),
                Query::HasTag(tag(1)),
                ChangeInterest::DELETED,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();

        f.watch_engine.notify_content_changed(oid(5), ts(1000));

        let events = f.watch_engine.drain_events(sub_id);
        assert!(events.is_empty());
    }

    #[test]
    fn dormant_and_catch_up() {
        let mut f = TestFixture::new();
        f.tag_index.tag_object(tag(1), 5);

        let (sub_id, _) = f
            .watch_engine
            .subscribe(
                "watch".into(),
                Query::HasTag(tag(1)),
                ChangeInterest::ALL,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();

        // Go dormant
        f.watch_engine.set_dormant(sub_id).unwrap();

        // While dormant: object 10 gains tag(1), object 5 loses it
        f.tag_index.tag_object(tag(1), 10);
        f.tag_index.untag_object(tag(1), 5);

        // Catch up
        let (entered, exited) = f
            .watch_engine
            .catch_up(sub_id, &f.tag_index, &f.kv_index, &f.dag)
            .unwrap();

        assert_eq!(entered, vec![10]);
        assert_eq!(exited, vec![5]);

        // Should be active again
        assert!(f.watch_engine.get(sub_id).unwrap().is_active());
    }

    #[test]
    fn unsubscribe() {
        let mut f = TestFixture::new();
        let (sub_id, _) = f
            .watch_engine
            .subscribe(
                "watch".into(),
                Query::HasTag(tag(1)),
                ChangeInterest::ALL,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();

        assert_eq!(f.watch_engine.subscription_count(), 1);
        f.watch_engine.unsubscribe(sub_id).unwrap();
        assert_eq!(f.watch_engine.subscription_count(), 0);
    }

    #[test]
    fn unsubscribe_nonexistent() {
        let mut engine = SubscriptionEngine::new();
        assert!(engine.unsubscribe(999).is_err());
    }

    #[test]
    fn dormant_subscription_receives_no_events() {
        let mut f = TestFixture::new();
        let (sub_id, _) = f
            .watch_engine
            .subscribe(
                "watch".into(),
                Query::HasTag(tag(1)),
                ChangeInterest::ALL,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();

        f.watch_engine.set_dormant(sub_id).unwrap();

        // This should not generate events for dormant sub
        f.tag_index.tag_object(tag(1), 5);
        f.watch_engine.notify_tag_added(
            oid(5),
            tag(1),
            ts(1000),
            &f.tag_index,
            &f.kv_index,
            &f.dag,
        );

        let events = f.watch_engine.drain_events(sub_id);
        assert!(events.is_empty());
    }

    #[test]
    fn compound_query_subscription() {
        let mut f = TestFixture::new();
        // Subscribe to: source AND test
        let (sub_id, _) = f
            .watch_engine
            .subscribe(
                "watch".into(),
                Query::And(vec![Query::HasTag(tag(1)), Query::HasTag(tag(2))]),
                ChangeInterest::ENTERED,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();

        // Add tag(1) to obj 5 — doesn't match yet (needs both)
        f.tag_index.tag_object(tag(1), 5);
        f.watch_engine.notify_tag_added(
            oid(5),
            tag(1),
            ts(1000),
            &f.tag_index,
            &f.kv_index,
            &f.dag,
        );
        assert!(f.watch_engine.drain_events(sub_id).is_empty());

        // Add tag(2) to obj 5 — now matches!
        f.tag_index.tag_object(tag(2), 5);
        f.watch_engine.notify_tag_added(
            oid(5),
            tag(2),
            ts(2000),
            &f.tag_index,
            &f.kv_index,
            &f.dag,
        );
        let events = f.watch_engine.drain_events(sub_id);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], WatchEvent::Entered { .. }));
    }

    #[test]
    fn multiple_subscriptions() {
        let mut f = TestFixture::new();

        let (sub1, _) = f
            .watch_engine
            .subscribe(
                "s1".into(),
                Query::HasTag(tag(1)),
                ChangeInterest::ENTERED,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();

        let (sub2, _) = f
            .watch_engine
            .subscribe(
                "s2".into(),
                Query::HasTag(tag(1)),
                ChangeInterest::ENTERED,
                &f.tag_index,
                &f.kv_index,
                &f.dag,
                0,
            )
            .unwrap();

        f.tag_index.tag_object(tag(1), 5);
        f.watch_engine.notify_tag_added(
            oid(5),
            tag(1),
            ts(1000),
            &f.tag_index,
            &f.kv_index,
            &f.dag,
        );

        // Both subscriptions should get the event
        assert_eq!(f.watch_engine.drain_events(sub1).len(), 1);
        assert_eq!(f.watch_engine.drain_events(sub2).len(), 1);
    }
}
