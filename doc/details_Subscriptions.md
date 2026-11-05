This is where the oplog-based architecture pays off massively. Every mutation in Mímisbrunnr is already recorded as a SyncOp with a hybrid timestamp. A watch is just a **saved query plus a cursor into the oplog**. Catching up after being offline is the same operation as live watching — just faster.

## Why inotify Is Broken

inotify's fundamental problems that we can fix:

```
inotify limitation                    Mímisbrunnr fix
─────────────────────────             ────────────────────────────
Watches paths, not content            Watches queries over tags
Must know paths in advance            Query matches future objects too
Misses events while not running       Cursor-based catch-up from oplog
Queue overflow = lost events          Oplog is persistent, never lost
Recursive watch is expensive          Bitmap intersection is O(1)
Moving a file across dirs = 2 events  Retagging is a single mutation
No semantic filtering                 Query IS the filter
Per-process FD limit                  Subscriptions are just cursors
Race between readdir and watch setup  Atomic: subscribe + snapshot
```

## The Core Abstraction: Subscriptions

A subscription is a persistent, named query that tracks changes:

```rust
struct Subscription {
    id: SubscriptionId,
    name: String,                       // "cargo-build-watcher"
    
    // What to watch — a query over the object space
    query: Query,
    
    // What kinds of changes to report
    interest: ChangeInterest,
    
    // Cursor: where we've read up to in the oplog
    cursor: HybridTimestamp,
    
    // Who owns this subscription
    owner: SubscriptionOwner,
    
    // Is someone actively listening right now?
    state: SubscriptionState,
    
    // Retention: how long to keep tracking while offline
    retention: Duration,                // e.g., 30 days
    created_at: HybridTimestamp,
}

enum SubscriptionOwner {
    Process { pid: u32, app_id: AppId },
    Agent { agent_id: AgentId },        // persists across restarts
    System,                              // internal (e.g., build system)
}

enum SubscriptionState {
    Active {                             // someone is consuming events
        notify: NotificationChannel,     // how to deliver
    },
    Dormant {                            // no listener, but tracking cursor
        last_active: HybridTimestamp,
    },
}

bitflags! {
    struct ChangeInterest: u32 {
        const TAG_ADDED      = 0b00000001;
        const TAG_REMOVED    = 0b00000010;
        const ATTR_CHANGED   = 0b00000100;
        const CONTENT_CHANGED= 0b00001000;  // blob was rewritten
        const CREATED        = 0b00010000;  // new object matching query
        const DELETED        = 0b00100000;  // object no longer exists
        const ENTERED        = 0b01000000;  // object newly matches query
        const EXITED         = 0b10000000;  // object no longer matches query
        
        const ALL            = 0b11111111;
        const MUTATIONS      = Self::TAG_ADDED.bits | Self::TAG_REMOVED.bits 
                              | Self::ATTR_CHANGED.bits | Self::CONTENT_CHANGED.bits;
        const MEMBERSHIP     = Self::ENTERED.bits | Self::EXITED.bits 
                              | Self::CREATED.bits | Self::DELETED.bits;
    }
}
```

The key insight is `ENTERED` and `EXITED`. These don't exist in inotify. When a file gains a tag that makes it match your query, that's `ENTERED`. When it loses a tag and falls out of your query's result set, that's `EXITED`. The object didn't move — your _view_ of it changed.

## The Change Event

```rust
struct ChangeEvent {
    timestamp: HybridTimestamp,
    object: ObjectId,
    kind: ChangeKind,
}

enum ChangeKind {
    // Object was just created and matches the query
    Created {
        initial_assertions: Vec<Assertion>,
    },
    
    // Object was deleted
    Deleted,
    
    // Object gained a tag/attribute that made it enter the query's result set
    // (it existed before but didn't match)
    Entered {
        trigger: Assertion,              // the assertion that caused the match
    },
    
    // Object lost a tag/attribute and no longer matches the query
    Exited {
        trigger: Assertion,              // what was removed
    },
    
    // Object already matched the query, and something about it changed
    TagAdded { tag: TagId },
    TagRemoved { tag: TagId },
    AttrChanged { key: TagId, old: Option<Value>, new: Value },
    ContentChanged { 
        old_hash: [u8; 32], 
        new_hash: [u8; 32],
        old_size: u64,
        new_size: u64,
    },
}
```

## Evaluation Architecture

When a mutation happens (tag add, tag remove, attr change, etc.), the subscription engine needs to determine which subscriptions are affected. Naively checking every subscription against every mutation is O(subscriptions × mutations). We can do much better.

### The Inverted Subscription Index

Just as tag bitmaps index objects by tag, we index subscriptions by what tags they care about:

```rust
struct SubscriptionEngine {
    // All active subscriptions
    subscriptions: HashMap<SubscriptionId, Subscription>,
    
    // Inverted index: tag_id → subscriptions that mention this tag
    // When tag X changes on any object, only check subscriptions
    // in this set — not all subscriptions
    tag_to_subs: HashMap<TagId, Vec<SubscriptionId>>,
    
    // Subscriptions on attributes: attr_key → subs
    attr_to_subs: HashMap<TagId, Vec<SubscriptionId>>,
    
    // Subscriptions that watch ALL changes (expensive, limit these)
    wildcard_subs: Vec<SubscriptionId>,
    
    // Cached: for each subscription, the current matching set
    // (so we can compute ENTERED/EXITED efficiently)
    match_cache: HashMap<SubscriptionId, RoaringBitmap>,
}
```

When a mutation arrives:

```rust
fn on_mutation(&mut self, op: &SyncOp) {
    match &op.op {
        SyncOpKind::AddTag { object, tag } => {
            // 1. Find subscriptions that care about this tag
            let affected_subs = self.tag_to_subs.get(tag)
                .into_iter().flatten()
                .chain(self.wildcard_subs.iter());
            
            for sub_id in affected_subs {
                let sub = &self.subscriptions[sub_id];
                let cache = &mut self.match_cache.get_mut(sub_id);
                
                let matched_before = cache.contains(*object as u32);
                let matches_now = evaluate_query(&sub.query, *object);
                
                match (matched_before, matches_now) {
                    (false, true) => {
                        // Object just entered the subscription's result set
                        cache.insert(*object as u32);
                        emit_event(sub, ChangeEvent {
                            timestamp: op.timestamp,
                            object: *object,
                            kind: ChangeKind::Entered { 
                                trigger: Assertion::Tag(*tag) 
                            },
                        });
                    }
                    (true, true) => {
                        // Already matched, tag changed within the match
                        if sub.interest.contains(ChangeInterest::TAG_ADDED) {
                            emit_event(sub, ChangeEvent {
                                timestamp: op.timestamp,
                                object: *object,
                                kind: ChangeKind::TagAdded { tag: *tag },
                            });
                        }
                    }
                    (true, false) => {
                        // Should not happen on AddTag, but defensive
                        unreachable!();
                    }
                    (false, false) => {
                        // Doesn't match, still doesn't match — ignore
                    }
                }
            }
        }
        
        SyncOpKind::RemoveTag { object, tag } => {
            let affected_subs = self.tag_to_subs.get(tag)
                .into_iter().flatten()
                .chain(self.wildcard_subs.iter());
            
            for sub_id in affected_subs {
                let sub = &self.subscriptions[sub_id];
                let cache = self.match_cache.get_mut(sub_id).unwrap();
                
                let matched_before = cache.contains(*object as u32);
                let matches_now = evaluate_query(&sub.query, *object);
                
                match (matched_before, matches_now) {
                    (true, false) => {
                        // Object just exited the result set
                        cache.remove(*object as u32);
                        emit_event(sub, ChangeEvent {
                            timestamp: op.timestamp,
                            object: *object,
                            kind: ChangeKind::Exited {
                                trigger: Assertion::Tag(*tag),
                            },
                        });
                    }
                    (true, true) => {
                        if sub.interest.contains(ChangeInterest::TAG_REMOVED) {
                            emit_event(sub, ChangeEvent {
                                timestamp: op.timestamp,
                                object: *object,
                                kind: ChangeKind::TagRemoved { tag: *tag },
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        
        // Similar for AttrChanged, ContentChanged, etc.
        _ => { /* ... */ }
    }
}
```

### Cost Analysis

```
Typical mutation: AddTag(obj:42, genre:jazz)

1. Look up tag "jazz" in tag_to_subs           O(1) hash lookup
2. Find 3 subscriptions care about "jazz"       iterate 3 entries
3. For each: check match_cache for obj 42       O(1) bitmap contains
4. For each that changed: evaluate full query   O(query_depth) bitmap ops
5. Emit event if needed                         O(1)

Total: ~microseconds for typical case

Compare with inotify:
  - kernel scans all watches on the parent directory
  - every nested watch is checked
  - recursive watches = linear scan
```

The inverted subscription index means we only evaluate subscriptions that _could_ be affected by the specific tag that changed. A music app watching `lang=rust AND source` is never evaluated when a music file is tagged.

## Offline Catch-Up

This is where the design shines. When a subscription is dormant (agent not running), the cursor stops advancing but the oplog keeps growing. When the agent reconnects:

```rust
fn catch_up(
    sub: &mut Subscription,
    oplog: &OpLog,
) -> Vec<ChangeEvent> {
    let mut events = Vec::new();
    
    // Strategy depends on how far behind we are
    let ops_behind = oplog.count_since(sub.cursor);
    
    if ops_behind < REPLAY_THRESHOLD {
        // Small gap: replay individual ops
        events = replay_ops(sub, oplog);
    } else {
        // Large gap: compute diff between old and new result sets
        events = diff_result_sets(sub, oplog);
    }
    
    // Advance cursor to current
    sub.cursor = oplog.latest_timestamp();
    events
}
```

### Strategy 1: Op Replay (Small Gap)

If the agent was offline for a short time (minutes to hours), replay the ops since the cursor:

```rust
fn replay_ops(
    sub: &mut Subscription,
    oplog: &OpLog,
) -> Vec<ChangeEvent> {
    let mut events = Vec::new();
    let mut current_match = sub.match_cache.clone().unwrap_or_else(|| {
        // If no cached bitmap, recompute from current indexes
        execute_query(&sub.query)
    });
    
    // Replay chronologically from cursor
    for op in oplog.range(sub.cursor..) {
        // Same logic as on_mutation, but batch-collected
        if let Some(event) = evaluate_op_against_sub(
            &op, sub, &mut current_match
        ) {
            events.push(event);
        }
    }
    
    // Update the cached match set
    self.match_cache.insert(sub.id, current_match);
    
    events
}
```

This preserves the full event sequence — the agent sees every individual change in order. Useful for build systems that need to know exactly what changed.

### Strategy 2: Result Set Diff (Large Gap)

If the agent was offline for days/weeks, replaying millions of ops is wasteful. Instead, compute what the result set looks like now vs. what it looked like when the agent went dormant:

```rust
fn diff_result_sets(
    sub: &mut Subscription,
    oplog: &OpLog,
) -> Vec<ChangeEvent> {
    // Current result set from live indexes
    let current = execute_query(&sub.query);
    
    // Previous result set from cached bitmap
    let previous = self.match_cache.get(&sub.id)
        .cloned()
        .unwrap_or_default();
    
    let mut events = Vec::new();
    
    // Objects that are now in the result but weren't before
    let entered = &current - &previous;
    for obj_id in entered.iter() {
        let record = object_table.get(obj_id);
        if record.created_ns > sub.cursor.to_nanos() {
            // Created after we went dormant
            events.push(ChangeEvent {
                timestamp: HybridTimestamp::from_nanos(record.created_ns),
                object: ObjectId(obj_id as u64),
                kind: ChangeKind::Created {
                    initial_assertions: forward_index.get(obj_id),
                },
            });
        } else {
            // Existed before but didn't match — entered via tag change
            events.push(ChangeEvent {
                timestamp: oplog.latest_timestamp(), // approximate
                object: ObjectId(obj_id as u64),
                kind: ChangeKind::Entered {
                    trigger: Assertion::Tag(TagId(0)), // can't determine exact trigger
                },
            });
        }
    }
    
    // Objects that were in the result but aren't now
    let exited = &previous - &current;
    for obj_id in exited.iter() {
        let record = object_table.get(obj_id);
        if record.state == ObjectState::Deleted { .. } {
            events.push(ChangeEvent {
                timestamp: HybridTimestamp::from_nanos(record.modified_ns),
                object: ObjectId(obj_id as u64),
                kind: ChangeKind::Deleted,
            });
        } else {
            events.push(ChangeEvent {
                timestamp: oplog.latest_timestamp(),
                object: ObjectId(obj_id as u64),
                kind: ChangeKind::Exited {
                    trigger: Assertion::Tag(TagId(0)),
                },
            });
        }
    }
    
    // Objects in both sets — check for content changes
    if sub.interest.contains(ChangeInterest::CONTENT_CHANGED) {
        let still_present = &current & &previous;
        // This is expensive: must check each object's content hash
        // Only do if the subscription specifically asked for content changes
        for obj_id in still_present.iter() {
            if let Some(event) = check_content_change(obj_id, sub.cursor) {
                events.push(event);
            }
        }
    }
    
    // Update cache
    self.match_cache.insert(sub.id, current);
    
    events
}
```

The diff approach loses individual event ordering but gives a correct summary in O(result_set_size) time instead of O(total_ops_missed). For an agent that was offline for a month, this is the difference between replaying 5 million ops and diffing two bitmaps.

```
Catch-up cost comparison:

                     Op Replay          Result Set Diff
                     ─────────          ───────────────
Gap: 5 minutes       ~1000 ops          unnecessary, use replay
(100 ops)            ~0.1ms             

Gap: 1 day           ~50K ops           diff two bitmaps
(50K ops)            ~50ms              ~1ms + per-object checks

Gap: 1 month         ~5M ops            diff two bitmaps
(5M ops)             ~5 seconds         ~1ms + per-object checks

Gap: 6 months        oplog may be       diff two bitmaps
                     truncated!         ~1ms + per-object checks
```

### The Oplog Retention Problem

The oplog is finite (64 MB circular buffer for WAL, plus optional extended retention for subscriptions). What if a dormant subscription's cursor points to ops that have been overwritten?

```rust
struct OpLogRetention {
    // The WAL: 64 MB circular, for crash recovery
    wal: CircularBuffer,
    
    // Extended retention: for dormant subscriptions
    // Kept as compressed oplog segments on disk
    extended: Vec<OpLogSegment>,
    
    // Oldest timestamp still available
    oldest_available: HybridTimestamp,
}

struct OpLogSegment {
    time_range: (HybridTimestamp, HybridTimestamp),
    ops: CompressedOps,     // zstd-compressed batch of SyncOps
    size_bytes: u64,
}

impl OpLogRetention {
    fn retain_for_subscriptions(&mut self, subs: &[Subscription]) {
        // Keep extended segments at least as far back as
        // the oldest dormant subscription's cursor
        let oldest_cursor = subs.iter()
            .filter(|s| matches!(s.state, SubscriptionState::Dormant { .. }))
            .map(|s| s.cursor)
            .min();
        
        if let Some(oldest) = oldest_cursor {
            // Don't trim extended log past this point
            self.extended.retain(|seg| seg.time_range.1 >= oldest);
        }
        
        // But enforce a maximum retention to bound disk usage
        let max_age = Duration::days(90);
        self.extended.retain(|seg| {
            now() - seg.time_range.1.to_instant() < max_age
        });
    }
    
    fn can_replay_from(&self, cursor: HybridTimestamp) -> bool {
        cursor >= self.oldest_available
    }
}
```

If the cursor is too old (beyond retention), fall back to result set diff — which doesn't need the oplog at all, just the current index state and the cached bitmap.

## Subscription Lifecycle

```
Agent creates subscription
     │
     ▼
┌──────────────────────┐
│ SUBSCRIBE            │
│                      │
│ 1. Parse query       │
│ 2. Execute query     │──── initial result set (snapshot)
│ 3. Cache bitmap      │
│ 4. Register in       │──── inverted sub index updated
│    sub engine        │
│ 5. Set cursor = now  │
│ 6. Return snapshot   │──── agent knows the starting state
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ ACTIVE               │
│                      │     live events delivered via channel
│ Mutations evaluated  │────▶ ChangeEvent stream to agent
│ against subscription │
│ Cursor advances      │
└──────────┬───────────┘
           │ agent disconnects / process exits
           ▼
┌──────────────────────┐
│ DORMANT              │
│                      │
│ Cursor frozen        │     oplog keeps growing
│ Match cache retained │     extended retention holds ops
│ Sub stays registered │
└──────────┬───────────┘
           │ agent reconnects
           ▼
┌──────────────────────┐
│ CATCH-UP             │
│                      │
│ Small gap: replay    │────▶ ordered event stream
│ Large gap: diff      │────▶ summary events (ENTERED/EXITED)
│ Cursor advances      │
│ Cache updated        │
└──────────┬───────────┘
           │
           ▼
        ACTIVE (live events resume)
```

### Atomic Subscribe + Snapshot

A critical race condition in inotify: you set up a watch, then `readdir` to get the initial state. Between those two operations, a file could be created and you'd miss it. Mímisbrunnr solves this atomically:

```rust
fn subscribe(query: Query, interest: ChangeInterest) -> (SubscriptionId, Vec<ObjectId>) {
    // These two operations are atomic w.r.t. the oplog
    let snapshot_cursor = oplog.latest_timestamp();
    let initial_set = execute_query(&query);
    
    let sub = Subscription {
        id: new_sub_id(),
        query,
        interest,
        cursor: snapshot_cursor,
        match_cache: initial_set.clone(),
        // ...
    };
    
    register_subscription(sub);
    
    // No gap: cursor is set to the moment the snapshot was taken.
    // Any mutation after snapshot_cursor will be delivered as an event.
    (sub.id, initial_set.iter().collect())
}
```

## Delivery Mechanisms

Different agents need events delivered differently:

```rust
enum NotificationChannel {
    // In-process callback (fastest, for linked libraries)
    Callback(Box<dyn Fn(ChangeEvent) + Send>),
    
    // Unix domain socket (for local daemons)
    Socket { path: PathBuf },
    
    // Named pipe / eventfd (lightweight wakeup)
    EventFd { fd: RawFd },
    
    // Polling (agent calls check() periodically)
    Poll,
    
    // Accumulated batch (agent collects events in bulk)
    Batch {
        max_events: usize,
        max_delay: Duration,
    },
}
```

### Batching for Build Systems

A build system watching source files doesn't want one event per keystroke during a save-all. It wants a debounced batch:

```rust
struct BatchConfig {
    // Wait this long after the last event before delivering the batch
    debounce: Duration,         // e.g., 200ms
    
    // But never wait longer than this since the first event
    max_delay: Duration,        // e.g., 2 seconds
    
    // If this many events accumulate, deliver immediately
    max_events: usize,          // e.g., 1000
    
    // Coalesce: if same object is modified multiple times,
    // deliver only the latest state
    coalesce: bool,
}

impl BatchAccumulator {
    fn add_event(&mut self, event: ChangeEvent) {
        if self.coalesce {
            // Replace previous event for same object
            self.events.insert(event.object, event);
        } else {
            self.events_ordered.push(event);
        }
        
        if self.events.len() >= self.config.max_events {
            self.flush();
            return;
        }
        
        // Reset debounce timer
        self.debounce_timer.reset(self.config.debounce);
        
        // Start max-delay timer if not already running
        if !self.max_timer.is_running() {
            self.max_timer.start(self.config.max_delay);
        }
    }
    
    fn on_timer(&mut self) {
        self.flush();
    }
    
    fn flush(&mut self) -> Vec<ChangeEvent> {
        self.max_timer.stop();
        self.debounce_timer.stop();
        std::mem::take(&mut self.events)
            .into_values()
            .collect()
    }
}
```

## Example Subscriptions

### Build System: Watch Source Files

```rust
let (sub_id, current_sources) = mimir.subscribe(
    And(vec![
        HasAttr { key: "project", op: Eq, value: Text("vesper") },
        HasTag(tag("source")),
    ]),
    ChangeInterest::CONTENT_CHANGED 
        | ChangeInterest::CREATED 
        | ChangeInterest::DELETED
        | ChangeInterest::ENTERED,
    BatchConfig {
        debounce: Duration::from_millis(200),
        max_delay: Duration::from_secs(2),
        coalesce: true,
    },
);

// Initial state: current_sources contains all source ObjectIds
// Now receive events:
loop {
    let batch = mimir.recv_batch(sub_id).await;
    
    let needs_rebuild: Vec<ObjectId> = batch.iter()
        .filter(|e| matches!(e.kind, 
            ChangeKind::ContentChanged { .. } 
            | ChangeKind::Created { .. }
            | ChangeKind::Entered { .. }
        ))
        .map(|e| e.object)
        .collect();
    
    let was_deleted: Vec<ObjectId> = batch.iter()
        .filter(|e| matches!(e.kind, ChangeKind::Deleted))
        .map(|e| e.object)
        .collect();
    
    if !needs_rebuild.is_empty() || !was_deleted.is_empty() {
        trigger_incremental_build(&needs_rebuild, &was_deleted);
    }
}
```

### IDE: Watch Project Files + Build Artifacts

```rust
// Watch everything in the project — source, generated, all of it
let (sub_id, _) = mimir.subscribe(
    HasAttr { key: "project", op: Eq, value: Text("vesper") },
    ChangeInterest::ALL,
    BatchConfig::default(),
);

// Now the IDE sees:
// - Source edits (for syntax highlighting refresh)
// - New object files (for error parsing)
// - Test results (for test runner UI)
// - Stale markers (for "needs rebuild" indicators)
// All from a single subscription.
```

### Backup Agent: Track All Critical Files

```rust
// Watch everything tagged critical, regardless of project
let (sub_id, snapshot) = mimir.subscribe(
    HasTag(tag("critical")),
    ChangeInterest::CONTENT_CHANGED 
        | ChangeInterest::CREATED 
        | ChangeInterest::ENTERED,
    BatchConfig { 
        debounce: Duration::from_secs(30),
        max_delay: Duration::from_secs(300),
        coalesce: true,
    },
);

// Agent starts, gets current set of critical files
backup_all(&snapshot);

// Then receives ongoing changes
loop {
    let batch = mimir.recv_batch(sub_id).await;
    for event in batch {
        match event.kind {
            ChangeKind::ContentChanged { new_hash, .. } => {
                incremental_backup(event.object, new_hash);
            }
            ChangeKind::Created { .. } | ChangeKind::Entered { .. } => {
                full_backup(event.object);
            }
            _ => {}
        }
    }
}

// Agent crashes. Restarts a week later.
// Subscription was dormant, cursor is 7 days behind.
let catch_up = mimir.resume_subscription(sub_id);
// Gets: list of objects that entered/exited/changed since last run
// No full re-scan needed!
```

### Sync Agent: Hydration Trigger

```rust
// Watch for objects that should be local but aren't
let (sub_id, _) = mimir.subscribe(
    And(vec![
        HasTag(tag("active")),
        HasTag(tag("project")),
        // Objects where content presence is Remote
        HasTag(tag("_presence:remote")),  // internal tag set by presence tracker
    ]),
    ChangeInterest::ENTERED,
    BatchConfig::immediate(),
);

// When an object becomes active+project but its content is remote,
// start hydrating it
loop {
    let events = mimir.recv(sub_id).await;
    for event in events {
        hydration_queue.enqueue(event.object, Priority::High);
    }
}
```

### Ontology-Aware Subscription

Subscriptions automatically benefit from ontology implications:

```rust
// Watch "all audio files" — matches mp3, flac, opus, wav
// even though we only said "audio"
let (sub_id, _) = mimir.subscribe(
    HasTag(tag("audio")),
    ChangeInterest::CREATED | ChangeInterest::ENTERED,
    BatchConfig::default(),
);

// If someone later installs a music ontology module that adds
// the implication "m4a → audio", existing m4a files that were
// previously unmatched will generate ENTERED events — the
// subscription automatically widens as the ontology grows.
```

This is impossible with inotify. You'd need to know every file extension that counts as "audio" and watch for each one.

## Persistence of Subscriptions

Subscriptions are stored in the metadata zone, surviving reboots:

```rust
// Stored in a subscription table in the metadata zone
// Indexed by subscription ID
struct PersistedSubscription {
    id: SubscriptionId,
    name: String,
    query: SerializedQuery,     // the query AST, serialized
    interest: u32,              // ChangeInterest bits
    cursor: HybridTimestamp,    // where we left off
    owner: SubscriptionOwner,
    retention: Duration,
    match_cache: Option<u64>,   // offset to serialized RoaringBitmap
    created_at: HybridTimestamp,
    last_active: HybridTimestamp,
}
```

On boot, the subscription engine loads all persisted subscriptions, rebuilds the inverted subscription index, and any subscription whose owner reconnects can immediately catch up.

```rust
fn boot_subscription_engine(meta_zone: &MetadataZone) -> SubscriptionEngine {
    let persisted = meta_zone.load_subscriptions();
    let mut engine = SubscriptionEngine::new();
    
    for sub in persisted {
        // Rebuild inverted index: which tags does this query mention?
        let referenced_tags = extract_referenced_tags(&sub.query);
        for tag_id in referenced_tags {
            engine.tag_to_subs.entry(tag_id)
                .or_default()
                .push(sub.id);
        }
        
        // Load cached match bitmap if available
        if let Some(cache_offset) = sub.match_cache {
            let bitmap = meta_zone.load_bitmap(cache_offset);
            engine.match_cache.insert(sub.id, bitmap);
        }
        
        // All subscriptions start as dormant until their owner connects
        engine.subscriptions.insert(sub.id, Subscription {
            state: SubscriptionState::Dormant {
                last_active: sub.last_active,
            },
            ..sub.into()
        });
    }
    
    // Expire subscriptions past their retention
    engine.gc_expired();
    
    engine
}
```

## Subscription Garbage Collection

Dormant subscriptions can't live forever. The retention field bounds how long a subscription tracks changes without an active listener:

```rust
fn gc_expired(engine: &mut SubscriptionEngine) {
    let now = HybridTimestamp::now();
    
    let expired: Vec<SubscriptionId> = engine.subscriptions.iter()
        .filter(|(_, sub)| {
            if let SubscriptionState::Dormant { last_active } = sub.state {
                now.duration_since(last_active) > sub.retention
            } else {
                false
            }
        })
        .map(|(id, _)| *id)
        .collect();
    
    for sub_id in expired {
        engine.remove_subscription(sub_id);
    }
}
```

## CLI

```
# Create a persistent subscription
mimir watch "project=vesper AND source" \
    --name cargo-watcher \
    --interest content-changed,created,deleted \
    --retention 30d

# List subscriptions
mimir watch list
  ID    NAME             QUERY                              STATE    CURSOR LAG
  s:01  cargo-watcher    project=vesper AND source           active   0 ops
  s:02  backup-critical  critical                            dormant  12,450 ops (3h)
  s:03  sync-hydrator    active AND _presence:remote         active   0 ops

# Catch up a dormant subscription
mimir watch catch-up s:02
  12,450 ops since last active (3 hours ago)
  Strategy: op replay (small gap)
  Events: 47 content-changed, 3 created, 1 deleted

# Watch live (like inotifywait but query-based)
mimir watch stream "project=vesper AND source" --format json
  {"ts":"2024-03-15T10:23:45Z","obj":42,"kind":"content-changed","name":"main.rs"}
  {"ts":"2024-03-15T10:23:46Z","obj":99,"kind":"tag-added","tag":"needs-review"}
  ...

# One-shot: what changed since last time?
mimir watch since s:02
  Modified:  src/main.rs (content changed)
  Created:   src/new_module.rs
  Entered:   vendor/lib.rs (was vendored, now tagged source)
  Deleted:   src/old_module.rs

# Remove subscription
mimir watch remove s:02
```

## Summary

```
inotify                           Mímisbrunnr Subscriptions
───────                           ─────────────────────────
watch("/src")                     watch("project=vesper AND source")
  sees: files in /src               sees: all source regardless of location
  misses: files moved in             catches: objects gaining "source" tag
  loses events when offline          catches up from oplog cursor
  per-directory FD cost              one subscription = one bitmap + cursor
  race on setup                      atomic subscribe + snapshot
  no semantic filter                 full query algebra
  no debouncing                      configurable batching + coalescing
  kernel overhead per watch          userspace bitmap evaluation
  dies with process                  persists across reboots
```
