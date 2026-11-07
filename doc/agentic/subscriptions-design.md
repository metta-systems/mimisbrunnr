# Subscriptions — design

Scope: `crates/watch` (`SubscriptionEngine`, `Subscription`), engine mutation hooks, `mimir watch` commands.
Governing spec: SPECIFICATION-DESIGN§11, SUBSCRIPTIONS-DESIGN§1.

Plan: [subscriptions-implementation-plan.md](subscriptions-implementation-plan.md) · Open decisions: [subscriptions-decisions.md](subscriptions-decisions.md) · Completed: [subscriptions-completed.md](subscriptions-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

A subscription is a saved query plus a cursor into the journal. A consumer polls for the events after its cursor and acknowledges them; live delivery and catching up after being offline are the same operation over gaps of different size.

## Selected design

- **Term-indexed evaluation.** Subscriptions are indexed by the terms and keys of their compiled queries (QUERY-DESIGN "Evaluation"). A mutation looks up the subscriptions of every term that entered or left the object's closure, so derived changes — a tag that implies one a query names, a value rule, a rule install — reach the subscriptions they affect.
- **Per-object evaluation.** Whether an object matches is decided against its own terms — forward entry plus closure — without reading postings.
- **Persisted pending events.** Every event a mutation produces for a subscription is stored in the subscription's pending-event tree, keyed `(subscription, lsn, index)`, until acknowledged; bulk membership changes (a rule install) are stored as one container per id window. Restart and catch-up read them back in order; the journal is never held back for a subscriber.
- **Membership as of the acknowledged cursor** is stored as posting containers in a snapshot-agnostic tree of its own; `ack` applies the acknowledged events to it. It is the baseline for the result-set diff a subscription falls back to when its pending events exceed its `retention` bound.
- **Not snapshot-aware.** Subscriptions watch the head. Creating, deleting or rolling back a snapshot never moves a consumer's cursor, membership or pending events.
- **Pull and acknowledge.** `poll(sub, max)` returns pending events in order, tagged with their LSN; `ack(lsn)` removes them and advances the persisted cursor. Delivery is at least once; consumers deduplicate by LSN. Debouncing and wake-ups run in the engine's `step` (ENGINE-DESIGN "Selected design"); no thread is needed.
- **Ids are never reused**: removing the subscription with the largest id leaves a high-water marker record (SUBSCRIPTIONS-DESIGN§1).
- **Remote changes are local operations.** Ops received by cluster sync are applied as ordinary local journal entries (CLUSTER-DESIGN), so subscriptions see them exactly as they see local mutations; their events carry the origin's hybrid timestamp.

## Why not inotify

Every mutation is already a journal entry with an LSN, so a watch is a saved query, the events it has produced since its cursor, and the cursor; catching up after being offline is the same `poll` as watching live, over more pending events.

| inotify | Subscriptions |
|---|---|
| Watches paths, which must be known in advance | Watches a query, which also matches objects created later |
| Misses events while the watcher is not running | Catches up from the cursor |
| Queue overflow loses events | Pending events are durable; past the subscription's bound, a result-set diff reports the net change |
| Recursive watches cost one watch per directory | One subscription is one membership set and one cursor |
| Moving a file across directories is two events | Retagging is one mutation |
| No semantic filtering | The query is the filter |
| Race between `readdir` and watch setup | Subscribe and its initial result are taken at one LSN |
| Dies with the process | Persists across reboots |

`Entered` and `Exited` have no inotify equivalent: the object did not move, its membership in the query's result set changed. They are also how ontology changes reach watchers — a subscription on `audio` receives `Entered` for existing `m4a` objects when a module adds `m4a → audio`, without listing file formats.

## Evaluation

**Index.** Each subscription's compiled query yields the terms it reads (tags, attribute
values, n-gram and roll-up terms, `_object` when it contains `Not`) and the keys it reads by
range (`HasKey`, comparisons, `Contains`, `Under`). The engine keeps, in memory:

- `term_to_subs: TermId → [SubscriptionId]`
- `key_to_subs: TagId → [SubscriptionId]` for keys read by range
- `content_subs: [SubscriptionId]` — subscriptions whose interest includes content changes

**Which subscriptions a mutation reaches.** Maintaining postings already computes the terms that
enter and leave an object's closure (INDEX-DESIGN§2.4). The subscriptions reached are those
under each such term in `term_to_subs`, those under its key in `key_to_subs`, and, for a content
write, `content_subs`. Object creation and deletion change `_object` and the object's terms like
any other mutation.

**Per-object evaluation.** For each reached subscription, *after* is the compiled query
evaluated against the object's terms after the mutation — its forward entry plus closure, with
range and `Contains` predicates compared through the term table — and *before* is the same
against its terms before (both closures are at hand). No postings are read.

| Before | After | Event |
|---|---|---|
| no | yes | `Entered` (`Created` for a new object) |
| yes | no | `Exited` (`Deleted` for a deleted object) |
| yes | yes | `TagAdded` / `TagRemoved` / `ContentChanged`, if the interest mask asks for it |
| no | no | none |

**Rule changes.** A rule install or removal reworks postings container by container
(INDEX-DESIGN§2.4); each container delta of a term in `term_to_subs` is handed to the
subscriptions under that term, whose objects are evaluated as above. A module adding
`m4a → audio` delivers `Entered` for every existing `m4a` object in bulk, in the commit that
installs it, stored as one bulk event per id window (SUBSCRIPTIONS-DESIGN§1).

Active and dormant subscriptions are evaluated alike: a dormant one accumulates pending events
until its consumer returns or its bound is exceeded. An overflowed subscription is no longer
evaluated until its consumer catches up. Cost per mutation is one lookup per changed term, one
evaluation per reached subscription and one pending-event entry per event (SPECIFICATION-DESIGN§14.4).

## Subscribe, lifecycle and boot

**Subscribe** evaluates the query at the current LSN; the result becomes the subscription's
membership, the LSN its cursor, and the result is returned to the caller. Every mutation after
the cursor is an event, so nothing falls between setup and initial state (SPECIFICATION-DESIGN§11.4).

```text
SUBSCRIBE ─▶ ACTIVE ──(consumer gone)──▶ DORMANT ──(bound exceeded)──▶ OVERFLOWED
             poll / ack, cursor moves    events accumulate              events dropped, not evaluated
                ▲                           │ (consumer returns)           │ (consumer returns)
                └───────────────────────────┴── poll pending events ◀──────┴── poll the diff
```

**Boot** loads every record (SUBSCRIPTIONS-DESIGN§1), compiles each query and rebuilds the
indexes; pending events are already on disk. A subscription becomes active again when its
consumer reconnects.

## Delivery

```text
poll(sub, max) -> [Event { lsn, hlc, oid, kind }]     // events after the cursor, oldest first
ack(sub, lsn)                                         // everything up to lsn is handled
```

- **Produce.** The mutation path evaluates reached subscriptions (above) and inserts each event
  into the pending-event tree as part of the mutation. The inserts are a deterministic function
  of the mutation and the subscriptions, so journal replay re-creates them like postings and
  they add no journal bytes of their own. A bulk change inserts one container per id window.
- **Poll** reads the subscription's pending events from its cursor in key order — the order
  the mutations committed — expanding bulk entries object by object, and applies debouncing.
- **Ack** removes the events up to `lsn`, applies them to the stored membership, and persists
  the new cursor and `cursor_ns` (the time of the acknowledged entry), in one journal entry.

A crash between `poll` and `ack` redelivers the same events with the same LSNs; consumers
deduplicate by LSN. `Buffered` durability (JOURNAL-DESIGN§1.5) applies to acks like any mutation.

**Bound.** A subscription's `retention` (SUBSCRIPTION-D2) bounds its pending events. When a new
event would exceed it, the subscription becomes `Overflowed`: its pending events are dropped
and it stops being evaluated. The next `poll` returns the result-set diff (below) and resumes
it. A subscriber that never acknowledges costs only its own bounded events; the journal trims
at its own pace (JOURNAL-DESIGN§1.6).

**Why membership is stored as of the acknowledged cursor.** The stored membership has one
use: when a subscription overflows and its pending events are dropped, the engine compares the
current result with what the subscriber last saw and reports the difference. That comparison
needs the membership as of the last `ack`, so `ack` is the only thing that updates it. Nothing
else reads it — matching a mutation is decided against the object's own terms, and the current
result is the query over postings — so keeping it current would only add a container rewrite
per membership-changing event.

## Offline catch-up

A returning consumer polls like any other ([SUBSCRIPTION6](subscriptions-implementation-plan.md)):

- **Pending events** (within the bound): every change since the cursor, in order — which build
  systems need. Cost is proportional to the events, which were written when they happened.
- **Result-set diff** (the subscription overflowed): evaluate the query now and compare with
  the stored membership as of the cursor. `now − member` is reported as
  `Created` for objects created after `cursor_ns` and `Entered` otherwise; `member − now` as
  `Deleted` for deleted objects and `Exited` otherwise. Members in both sets report
  `ContentChanged` when `ObjectRecord.modified_ns > cursor_ns` and, if the interest asks for
  tag changes, a description change when the forward entry's `modified_ns > cursor_ns`
  (INDEX-DESIGN§2.1). Order and intermediate states are lost; cost is proportional to the result
  set, not the gap. Afterwards the stored membership is the current result and the cursor is
  the current LSN.

A subscriber that must never lose order sets a bound large enough for its longest absence;
each pending event costs about 32 bytes before packing.

## Batching

A build system watching sources wants one batch after a save-all, not one event per file. `debounce_ms` holds a subscription's events until the window passes without new events for the same object, and `poll` returns one event per object with its latest state; the window is checked in `step` ([SUBSCRIPTION4](subscriptions-implementation-plan.md)). SPECIFICATION-DESIGN§11.6 also names a maximum delay since the first held event, a maximum batch size that forces delivery, and a coalescing switch; these need record fields ([SUBSCRIPTION-D3](subscriptions-decisions.md)).

## Usage examples

- **Build system:** `project=vesper AND source`, interest `CONTENT_CHANGED | CREATED | DELETED | ENTERED`, 200 ms debounce. Changed, created or entered objects go to an incremental rebuild; deleted ones are dropped from the build graph.
- **IDE:** `project=vesper` with all interest bits: source edits, new object files, test results and `stale` markers from one subscription.
- **Backup agent:** `critical`, interest `CONTENT_CHANGED | CREATED | ENTERED`, 30 s debounce. A full backup of the initial result, then incremental backups per event; after a week offline, catch-up reports what changed without a rescan.
- **Hydration trigger:** `active AND project AND _presence:remote`, interest `ENTERED`: objects that become active while their content is only remote are queued for hydration (SPECIFICATION-DESIGN§10.3).

## CLI

Commands: `mimir watch register <name> <query> | list | drain <name> | unsubscribe <name> | stream <name>`. How `stream` runs is [SUBSCRIPTION-D1](subscriptions-decisions.md). The intended surface, including catch-up reporting, is [SUBSCRIPTION-D4](subscriptions-decisions.md).

## Rejected alternatives

- **A hybrid-logical-clock timestamp as the cursor.** The cursor is the journal LSN of the last acknowledged event; events still carry their HLC timestamp.
- **Indexing subscriptions by the tags their query names** (`tag_to_subs`). A mutation's derived changes — an implied tag, a value rule, a rule install — never mention those tags; subscriptions are indexed by compiled terms and reached through the closure delta.
- **Result sets as postings of a term in the `Postings` tree.** That tree is snapshot-aware: a rollback would rewind every subscription's membership with the pool. Membership has its own snapshot-agnostic tree with the same container format.
- **Snapshot-aware subscription records.** A rollback or snapshot deletion would move consumers' cursors.
- **Push delivery from an engine thread.** The engine is step-driven; consumers poll.
- **Persisting subscriptions at fixed offsets with the match bitmap in a separate area.** Subscriptions are per-record B+ tree entries (SUBSCRIPTIONS-DESIGN§1).
- **Replaying the journal for each subscriber** (membership at the cursor, events re-derived from journal history on restart). A subscriber that does not acknowledge would hold the shared journal back for every subscriber, on the fastest tier, and order would depend on the pool's journal cap rather than the subscriber's own bound.
- **Membership at the head with a diff on every restart.** Every restart or buffer overflow would lose order.
- **Stored membership kept current with every mutation**, rewound by the pending events when a subscription overflows. It costs a container rewrite per membership-changing event and gives nothing the acknowledged-cursor membership lacks: matching is decided against each object's own terms, and the current result is the query over postings.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Subscriptions

**Records.** `RootPointer.subscriptions_root` (ENGINE-DESIGN§1.2) roots a BTREE-DESIGN§1 B+ tree
of `BtreeKind::Subscriptions`, snapshot-agnostic, with one entry per subscription: key
`id: u64` (`SubscriptionId`, packed-key codec), value the positional, versioned CBOR
`SubscriptionRecord` (FORMAT-DESIGN§1.6), a variable-size value (`value_kind = 1`,
BTREE-DESIGN§1.6), schema version 1:

```text
SubscriptionRecord = [version,
                      name: text,
                      query: Query,          // stored form: tags by TagUid (SPECIFICATION-DESIGN§2.3)
                      interest: uint,        // ChangeInterest bits
                      cursor: uint,          // LSN of the last acknowledged event
                      cursor_ns: int,        // time of that journal entry
                      state: uint,           // 0 Active, 1 Dormant, 2 Overflowed,
                                             //   0xFF high-water marker
                      retention: Retention,  // SUBSCRIPTION-D2: the bound on pending events
                      debounce_ms: uint,
                      pending_count: uint]   // pending events (bulk entries count their objects)
```

**Ids.** Ids are allocated from 1 and never reused. The counter is derived at boot as one past
the largest key. When the subscription with the largest id is removed, its record is replaced by
a high-water marker (`state = 0xFF`, other fields empty) so the counter cannot fall back; a
marker is removed once a larger id exists. This is the pattern of term and tag ids
(INDEX-DESIGN§1.2, ONTOLOGY-DESIGN§1).

**Membership.** `RootPointer.subscription_members_root` roots a B+ tree of
`BtreeKind::SubscriptionMembers`, snapshot-agnostic, keyed by `(id: u64, high: u64)` with
`high` the object id shifted right by 16, whose values are posting containers in the
INDEX-DESIGN§3.2 format (`ContainerHead` and array, bitmap or run payload). The containers of
one id are the subscription's membership as of its cursor. An `ack` rewrites only the
containers whose windows its events touch; removing a subscription removes its containers.

**Pending events.** `RootPointer.subscription_events_root` roots a B+ tree of
`BtreeKind::SubscriptionEvents`, snapshot-agnostic, keyed by `(id: u64, lsn: u64, index: u32)` —
`index` orders the events one mutation produces for one subscription — with variable-size
values:

```rust
#[repr(C)]
struct PendingEvent {                        // 32 bytes
    kind: u8,                                //  [0..1]   EventKind
    _pad: [u8; 3],                           //  [1..4]
    tag: u32,                                //  [4..8]   TagAdded / TagRemoved: the TagId; else 0
    oid: u64,                                //  [8..16]  the object; bulk kinds: the id window (oid >> 16)
    hlc: HybridTimestamp,                    // [16..32]  the mutation's timestamp (SPECIFICATION-DESIGN§10.4)
}
// Bulk kinds are followed by a posting container (INDEX-DESIGN§3.2) of the window's objects.

#[repr(u8)]
enum EventKind {
    Entered = 0, Exited = 1, Created = 2, Deleted = 3,
    TagAdded = 4, TagRemoved = 5, ContentChanged = 6,
    BulkEntered = 7, BulkExited = 8,         // rule rework (SUBSCRIPTIONS-DESIGN "Evaluation")
}
```

Inserts happen with the mutation that produces them and are re-created by journal replay; an
`ack` removes the acknowledged range; an overflow or removal drops the subscription's range.

**One entry per subscription.** A change to one subscription is one replacement entry, appended
as a new run to its node (BTREE-DESIGN§1.2) without rewriting the others; compaction folds the
runs.
