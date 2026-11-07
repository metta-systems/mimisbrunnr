# Subscriptions — design

Scope: `crates/watch` (`SubscriptionEngine`, `Subscription`), engine mutation hooks, `mimir watch` commands.
Governing spec: SPECIFICATION-DESIGN§11, SUBSCRIPTIONS-DESIGN§1.

Plan: [subscriptions-implementation-plan.md](subscriptions-implementation-plan.md) · Open decisions: [subscriptions-decisions.md](subscriptions-decisions.md) · Completed: [subscriptions-completed.md](subscriptions-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

A subscription is a saved query plus a cursor; every mutation updates matching subscriptions' membership, and a subscriber that was offline catches up from its cursor.

## Selected design

- **Per-record tree.** One sorted-run entry per subscription: key `(SubscriptionId: u64, snapshot: u32)` through the packed-key codec, value the CBOR `SubscriptionRecord` under `VARINT` value size.
- **`next_id` is derived at boot** as `max(loaded ids)` (0 when empty); the allocator pre-bumps, so `next_id` equals the last assigned id.
- **`cached_result`** is inline in the record until sub-bucket allocation exists; it then moves out of line as posting containers (INDEX-DESIGN§3.2), so a cursor-only change does not rewrite the bitmap.
- Engine mutations call `set_membership` after re-evaluating each affected subscription's query; debounced events flush on `tick`.
- Format changes are one-way: pools recreate.

## Why not inotify

Every mutation is already a WAL entry with an LSN, so a watch is a saved query plus a cursor into that log; catching up after being offline is the same operation as watching live, run faster.

| inotify | Subscriptions |
|---|---|
| Watches paths, which must be known in advance | Watches a query, which also matches objects created later |
| Misses events while the watcher is not running | Catches up from the cursor |
| Queue overflow loses events | Events derive from the durable log or from a result-set diff |
| Recursive watches cost one watch per directory | One subscription is one bitmap and one cursor |
| Moving a file across directories is two events | Retagging is one mutation |
| No semantic filtering | The query is the filter |
| Race between `readdir` and watch setup | Subscribe and snapshot are atomic |
| Dies with the process | Persists across reboots |

`Entered` and `Exited` have no inotify equivalent: the object did not move, its membership in the query's result set changed. They are also how ontology changes reach watchers — a subscription on `audio` receives `Entered` for existing `m4a` objects when a module adds `m4a → audio`, without listing file formats.

## Evaluation

The engine keeps an inverted index `tag_to_subs: TagId → [SubscriptionId]` over every tag a query mentions (`HasTag`, `IsA`, `HasAttr.key`, `Related.predicate`, through `And` / `Or` / `Not`; `extract_tags`). A mutation on tag `t` evaluates only the subscriptions listed under `t`. For each one, membership before (the `cached_result` bit) and after (the query evaluated for that object) decide the event:

| Before | After | Event |
|---|---|---|
| no | yes | `Entered`; the object joins `cached_result` |
| yes | no | `Exited`; the object leaves `cached_result` |
| yes | yes | `TagAdded` / `TagRemoved` / `ContentChanged`, if the interest mask asks for it |
| no | no | none |

Object creation and deletion run the same check through `on_object_created` / `on_object_deleted` and report `Created` / `Deleted`. Dormant subscriptions are skipped; their cursor stays where it was. Cost per mutation is a hash lookup, one bitmap probe per listed subscription and a query evaluation only for those (SPECIFICATION-DESIGN§14.4). Full re-evaluation in every hook is [SUBSCRIPTION3](subscriptions-implementation-plan.md).

## Subscribe, lifecycle and boot

**Subscribe** samples the current LSN and evaluates the query against the state at that LSN; the result becomes `cached_result`, the LSN the cursor, and the result is returned to the caller. Every mutation after the cursor is delivered as an event, so nothing falls between setup and initial state (SPECIFICATION-DESIGN§11.4).

```text
SUBSCRIBE ─▶ ACTIVE ──(consumer gone)──▶ DORMANT ──(consumer returns)──▶ CATCH-UP ─▶ ACTIVE
             live events, cursor moves   cursor frozen, record kept      replay or diff
```

**Boot** loads every record (SUBSCRIPTIONS-DESIGN§1), rebuilds `tag_to_subs` from each query, and restores `cached_result`. A subscription becomes active again when its consumer reconnects; until then the log keeps growing behind its cursor.

## Offline catch-up

A returning consumer gets the events between its cursor and now by one of two strategies ([SUBSCRIPTION6](subscriptions-implementation-plan.md)):

- **Replay** (small gap): run every log entry after the cursor through the evaluation above, in order. The consumer sees each change in sequence, which build systems need. Cost is proportional to the entries missed.
- **Result-set diff** (large gap, or the cursor is older than the oldest retained entry): evaluate the query now and compare with `cached_result`. `now − cached` is reported as `Created` for objects created since the subscription went dormant and `Entered` otherwise; `cached − now` as `Deleted` for deleted objects and `Exited` otherwise. If the interest includes content changes, members in both sets are checked for content changes since the cursor, object by object, which is the expensive part. Order and intermediate states are lost; cost is proportional to the result set, not the gap.

| Gap | Replay | Diff |
|---|---|---|
| 5 minutes, ~100 entries | ~0.1 ms | not needed |
| 1 day, ~50 K entries | ~50 ms | ~1 ms plus per-object checks |
| 1 month, ~5 M entries | ~5 s | ~1 ms plus per-object checks |
| 6 months | log no longer holds it | ~1 ms plus per-object checks |

The switch between strategies is a threshold on the number of entries behind. Replay reads the retained journal, which is held back for subscriptions that want it within a per-pool cap (JOURNAL-DESIGN§1.6); a cursor older than the retained history uses the diff.

## Batching

A build system watching sources wants one batch after a save-all, not one event per file. `debounce_ms` holds a subscription's events until the window passes without new events for the same object, and delivers one event per object with its latest state ([SUBSCRIPTION4](subscriptions-implementation-plan.md)). SPECIFICATION-DESIGN§11.6 also names a maximum delay since the first held event, a maximum batch size that forces delivery, and a coalescing switch; these need record fields ([SUBSCRIPTION-D3](subscriptions-decisions.md)).

## Usage examples

- **Build system:** `project=vesper AND source`, interest `CONTENT_CHANGED | CREATED | DELETED | ENTERED`, 200 ms debounce. Changed, created or entered objects go to an incremental rebuild; deleted ones are dropped from the build graph.
- **IDE:** `project=vesper` with all interest bits: source edits, new object files, test results and `stale` markers from one subscription.
- **Backup agent:** `critical`, interest `CONTENT_CHANGED | CREATED | ENTERED`, 30 s debounce. A full backup of the initial snapshot, then incremental backups per event; after a week offline, catch-up reports what changed without a rescan.
- **Hydration trigger:** `active AND project AND _presence:remote`, interest `ENTERED`: objects that become active while their content is only remote are queued for hydration (SPECIFICATION-DESIGN§10.3).

## CLI

Commands: `mimir watch register <name> <query> | list | drain <name> | unsubscribe <name> | stream <name>`. The intended surface, including catch-up reporting, is [SUBSCRIPTION-D4](subscriptions-decisions.md).

## Rejected alternatives

- **A hybrid-logical-clock timestamp as the cursor.** The cursor is the WAL LSN of the last delivered event; events still carry their HLC timestamp.
- **Persisting subscriptions at fixed offsets with the match bitmap in a separate area.** Subscriptions are per-record B+ tree entries (SUBSCRIPTIONS-DESIGN§1).
- **Separate attribute and wildcard indexes** (`attr_to_subs`, `wildcard_subs`). Attribute keys and relation predicates are tags, so one `tag_to_subs` covers them, and every query mentions at least one tag.
- **The subscription engine owning its own extended log.** History is the retained journal (JOURNAL-DESIGN§1.6); a subscription beyond it falls back to the diff.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Subscriptions

The subscription directory is a BTREE-DESIGN§1 B+ tree region of
`BtreeKind::Subscriptions` with one leaf entry per subscription. The
2-field packed key `(SubscriptionId, snapshot)` (BTREE-DESIGN§1.6) lets the
codec encode the typical "one snapshot dominates" case in zero bytes for the snapshot
field. The value is the positional-CBOR `SubscriptionRecord`, a variable-size value
(`value_kind = 1`, BTREE-DESIGN§1.6) — query ASTs and retention configs are heterogeneous so
per-entry value lengths vary.

```rust
// Run key — 2 fields via KeyFormat (BTREE-DESIGN§1.6).
struct SubscriptionsKey {
    id:       u64,                            // SubscriptionId
    snapshot: u32,                            // SnapshotId (SNAPSHOTS-DESIGN§1)
}

// Sorted-run value tail — positional CBOR (FORMAT-DESIGN§1.6): an array
// of these fields in this order.
struct SubscriptionRecord {                   // ~100–400 B typical
    version:       u16,                       //   schema version (FORMAT-DESIGN§1.6)
    name:          String,                    //   human label
    query:         Query,                     //   subscription's predicate AST
    interest:      ChangeInterest,            //   add/remove/update mask
    cursor:        u64,                       //   last delivered WAL LSN
    state:         SubscriptionState,         //   Active / Dormant
    retention:     Retention,                 //   Unlimited / AtMostOnce / Bounded { max_events }
    debounce_ms:   u32,                       //   coalesce window
    cached_result: Vec<u8>,                   //   roaring bitmap byte image (inline)
}
```

**`next_id` is not persisted.** Boot recovers it as
`max(loaded_subscription_ids)` (or `0` on an empty pool). The
allocator is pre-bump (`next_id += 1; id = next_id`), so `next_id`
ends up equal to the last assigned id rather than the next unused
one. This mirrors ENGINE-DESIGN§2's "scalars are derived, not stored" rule for
`next_oid_local` and `next_tag_id`.

**`cached_result` is inline.** The record carries the serialised roaring
bitmap of the subscription's current result set. Per-subscription result
sets are typically small (a handful of objects), so the bitmap costs
little next to the query AST; the cost is that a change to the cursor
alone rewrites the bitmap bytes with the rest of the record.

**One entry per subscription.** Because each subscription is its own
key with a variable-length value, a change to one subscription is one
replacement entry: it can be written as a one-entry sorted run appended
to the region (BTREE-DESIGN§1.2) without rewriting the other
subscriptions, and full compaction (BTREE-DESIGN§1.4) folds the runs.

