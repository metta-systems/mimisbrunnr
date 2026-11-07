# Snapshots — design

Scope: snapshot tree, snapshot-aware keys across every snapshot-aware tree, history sidecars, snapshot WAL ops, snapshot-aware queries and blob reads.
Governing spec: SNAPSHOTS-DESIGN§1 (SNAPSHOTS-DESIGN§1.1 snapshot tree, SNAPSHOTS-DESIGN§1.2 key positions, SNAPSHOTS-DESIGN§1.3 visibility, SNAPSHOTS-DESIGN§1.6 bucket retention, SNAPSHOTS-DESIGN§1.7 retention policy, SNAPSHOTS-DESIGN§1.8 rollback).

Plan: [snapshots-implementation-plan.md](snapshots-implementation-plan.md) · Open decisions: [snapshots-decisions.md](snapshots-decisions.md) · Completed: [snapshots-completed.md](snapshots-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

A snapshot captures the assertion graph (tags, attributes, relations, ontology) and the blob contents it references, per node, with key-level versioning instead of tree copies.

## Selected design

- Snapshots are per node; there is no cluster-wide snapshot coordination.
- Every snapshot-aware tree (forward, term dictionary, postings, sequences, ontology, subscriptions) keys by `(…, snapshot)`; the `snapshot` field is 0 until this scope lands.
- Object and location history live in sidecar trees (`ObjectHistory`, `LocationHistory`); the main tables hold the current view.
- Queries take an optional `snapshot_id` and resolve visibility through the snapshot tree's ancestor bitmap.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Snapshots

Snapshots are **key-level**: a snapshot is a 32-bit ID embedded in the position of every key in a
snapshot-aware btree. Multiple versions of the "same" logical key coexist in one btree,
distinguished by their `snapshot` field. Visibility is determined by walking the snapshot tree
(SNAPSHOTS-DESIGN§1.1).

Creation is **O(1)** regardless of pool size — no keys are copied, no checkpoint is forced. Many
thousands or millions of snapshots can exist simultaneously; their cost is the disk space
consumed by keys unique to each snapshot.

### 1.1 Snapshot tree

A `SnapshotId` is a `u32` (4 G snapshots before recycling concerns). Snapshot relationships are
stored in the **snapshots btree** (`BtreeKind::Snapshots`, a BTREE-DESIGN§1 B+ tree keyed by `SnapshotId`):

```rust
#[repr(C, packed)]
struct SnapshotNode {                        // 64 bytes — fits one cache line
    id: u32,                                 //  [0..4]   self-id (also the btree key)
    parent: u32,                             //  [4..8]   0 = root snapshot
    first_child: u32,                        //  [8..12]  first child id (0 = leaf)
    next_sibling: u32,                       // [12..16]  next sibling under same parent (0 = last)
    ancestor_bitmap: u128,                   // [16..32]  bits[i] = "id − i is an ancestor",
                                             //           i ∈ 0..128 — placed at offset 16 so
                                             //           the u128 hits its natural alignment
                                             //           on the hot ancestry-check path
    skiplist: [u32; 3],                      // [32..44]  randomised ancestor IDs for O(log n) deep checks
    depth: u16,                              // [44..46]  distance from root
    flags: u8,                               // [46..47]  SNAPSHOT_FLAG_*
    _pad: u8,                                // [47..48]
    created_ns: i64,                         // [48..56]
    label_offset: u32,                       // [56..60]  into a string heap; 0 = unlabelled
    _reserved: u32,                          // [60..64]
}

// SnapshotNode.flags bits
const SNAPSHOT_FLAG_LEAF:    u8 = 1 << 0;  // subvolume-bearing (a writable head)
const SNAPSHOT_FLAG_DELETED: u8 = 1 << 1;  // marked for SnapshotCleanup (SNAPSHOTS-DESIGN§1.5)
```

**Children topology.** The pair `(first_child, next_sibling)` encodes a **first-child /
next-sibling** linked list, supporting an arbitrary number of children per node at fixed
per-node cost. Walking a node's children is `cur = node.first_child; while cur != 0 { yield
cur; cur = tree[cur].next_sibling; }`. This admits the rollback case (SNAPSHOTS-DESIGN§1.8: a snapshot may
spawn N≥3 sibling branches over its lifetime) without changing node size — same 8 bytes as
the previous `[u32; 2]` form.

**Ancestry check.** The 128-bit `ancestor_bitmap` answers `is X an ancestor of Y?` in O(1) when
`Y.id − X.id ≤ 128` — which covers the vast majority of cases (most snapshots reference recent
ancestors). For older queries, the 3-entry randomised skiplist provides O(log n) traversal to
the root. During early recovery, before this data is validated, queries fall back to a simple
parent-pointer walk.

`RootPointer.snapshot_chain_root` is the snapshots btree root.

### 1.2 Snapshot-aware bkey position

In snapshot-aware btrees, every bkey position carries an extra `snapshot: u32` field appended
to the kind-specific key fields. With BTREE-DESIGN§1.6 packing, `snapshot` is typically a 0-bit field in
a leaf sorted run (one snapshot dominates the sorted run's keys), or a few bits at most — its
on-disk overhead is in the noise. The unpacked in-memory struct still pays the 4 bytes; this
section's struct definitions show the field explicitly.

**Where the field appears.** Every leaf-entry struct in this document marked snapshot-aware
below carries a `snapshot: u32` field immediately after the kind-specific sort-key fields
(e.g. `oid` for Forward, `(term_id, high)` for Postings, `(kind, key, context, value, term_id)`
for TermDictionary). The
field is part of the **key**, not the value — it participates in ordering and is what the
BTREE-DESIGN§1.6 format descriptor packs to ~0 bits when one snapshot dominates.

**Inheritance for nested structures.** Out-of-line data a snapshot-aware entry points at —
an out-of-line term value, a forward-overflow chain — is shared across snapshots; only the
entry that selects it carries the snapshot id.

| Btree              | Snapshot-aware? | Notes                                         |
| ------------------ | --------------- | --------------------------------------------- |
| `Forward`          | yes             | per-object terms diverge across snapshots      |
| `TermDictionary`   | yes             | per-term statistics and inline postings diverge |
| `Postings`         | yes             | one container per `(term_id, high, snapshot)`; a change diverges one container |
| `Sequences`        | yes             | collection order diverges                      |
| `TermTable`        | no              | a term never changes once created              |
| `ChunkIndex`       | no              | content-addressed; refcount handles divergence |
| `Backpointer`      | no              | physical state, not logical                    |
| `BucketAlloc`, `FreespaceLru` | no | physical state                              |
| `Ontology`         | yes             | snapshot freezes the ontology version          |
| `Subscriptions`    | yes             | per-snapshot watch state                       |
| `ObjectHistory`    | yes             | sidecar for `ObjectTable` (METADATA-DESIGN§1) — current view in the radix, per-snapshot overrides here |
| `LocationHistory`  | yes             | sidecar for `LocationTable` (METADATA-DESIGN§2.1) — same pattern |

For the **positional radix tables** (`ObjectTable` METADATA-DESIGN§1, `LocationTable` METADATA-DESIGN§2.1), snapshot-versioning
uses a sidecar btree per radix (`BtreeKind::ObjectHistory` for METADATA-DESIGN§1; `BtreeKind::LocationHistory`
for METADATA-DESIGN§2.1): each is keyed by `(oid, snapshot)` with values that shadow the radix entry. The radix
always holds the **current** view; reads in a non-current snapshot consult the sidecar first,
falling through to the radix only if no shadowing record applies. This keeps the hot path
(current-snapshot reads) at single-radix-lookup cost while preserving the snapshot model for
older views.

For **posting lists** (INDEX-DESIGN§3.1), each container is its own snapshot-tagged entry: a
write that changes one 65 536-id window of a term writes one new container at the writing
snapshot's id, and every other window stays shared with the ancestor snapshots.

### 1.3 Visibility rules (snapshot iteration)

A **whiteout** is a tombstone marking that a key is **explicitly deleted** at a snapshot.
The snapshot-aware leaves do not share a common discriminator slot — their value shapes
were chosen for their query-hot fields (e.g. `DictEntry` puts `cardinality` at offset 0) —
so the whiteout encoding is **per leaf shape**:

| Btree | Whiteout sentinel |
| ----- | ----------------- |
| `Forward` (INDEX-DESIGN§2.1) | a `ForwardHead` with bit 7 of `flags` set (`FORWARD_FLAG_WHITEOUT`), both counts 0, and no arrays. |
| `Postings` (INDEX-DESIGN§3.2) | a `ContainerHead` with `kind = 0xFF` (`CONTAINER_WHITEOUT`) and no payload. |
| `TermDictionary` (INDEX-DESIGN§4.1) | a `DictEntry` with bit 7 of `flags` set (`DICT_FLAG_WHITEOUT`) and every other field zero. |
| `Sequences` (INDEX-DESIGN§3.3) | ordered: the value `oid = u64::MAX`, never a valid id; ranked: the 1-byte value `0xFF` in place of `0x00`. |
| `Ontology`, `Subscriptions` (ONTOLOGY-DESIGN§1, SUBSCRIPTIONS-DESIGN§1) | a CBOR null (`0xF6`) at the value position, in place of the usual record. |
| `ObjectHistory`, `LocationHistory` (SNAPSHOTS-DESIGN§1.2) | a 1-byte discriminator precedes the shadowed record; `0xFF` (`KEY_TYPE_whiteout`) means whiteout, otherwise the record follows. |

Whiteouts carry no payload beyond the sentinel — the entry's length stops there.

When reading at snapshot `S`, the iterator walks the btree in order. For keys with the same
non-snapshot prefix, it picks the one with the highest `snapshot ≤ S` that is an ancestor of
`S`. A whiteout encountered as the chosen ancestor returns "not visible" — it blocks fall-through
to older versions for descendants of that snapshot. Whiteouts are physically reclaimed during
the next full compaction (BTREE-DESIGN§1.4) of any leaf where every ancestor of the whiteout's snapshot
is itself in the same leaf and either deleted or whited-out.

Pseudocode:

```
fn visible_at(key: &Bkey, snapshot: SnapshotId) -> bool {
    // key.snapshot must be S itself or an ancestor of S
    snapshot_tree.is_ancestor(key.snapshot, snapshot)
}

fn lookup(prefix: &KeyPrefix, snapshot: SnapshotId) -> Option<Value> {
    let mut best: Option<&Bkey> = None;
    for k in btree.range(prefix..) {
        if k.prefix() != prefix { break; }
        if !visible_at(k, snapshot) { continue; }
        if best.is_none() || k.snapshot > best.unwrap().snapshot {
            best = Some(k);
        }
    }
    best.and_then(|k| match k.value {
        KEY_TYPE_whiteout => None,                      // explicitly deleted in snapshot
        v                 => Some(v),
    })
}
```

In practice, this is implemented as a single forward iteration — keys with the same prefix
cluster together in the btree leaf, and the iterator scans them in `(prefix, snapshot)` order,
choosing the closest ancestor as it goes.

### 1.4 Snapshot creation

Snapshot creation is O(1): allocate two new `SnapshotId`s as children of the current snapshot's
node — one becomes the new snapshot's ID; the other replaces the current view's ID. No keys are
copied. Both children inherit visibility of all ancestor keys through the tree. The new pair
is **prepended** to N's child sibling list (each new child becomes the head; previous head
becomes its `next_sibling`).

```
Before snapshot:                After snapshot:

       N (current)                       N
                                         │
                                  first_child = N₁
                                         │ next_sibling
                                         ▼
                                         N₂   (N₁ = new "current", N₂ = the snapshot)
```

Subsequent writes to the current view are tagged with `N₁`; reads against the snapshot use `N₂`.
Divergence happens only where modifications occur. If N already has children from prior
snapshots / rollbacks, those nodes follow N₂ in the sibling chain (`N₂.next_sibling`) — the
total ordering reflects creation recency, head-first.

The `SnapshotCreate` WAL op (JOURNAL-DESIGN§1.3) records this as
`{ new_id, parent_id, current_replacement, label }` where `new_id ↔ N₂` (the frozen snapshot),
`parent_id ↔ N` (the previous current), and `current_replacement ↔ N₁` (the new writable head
to which subsequent mutations are tagged).

A new `BackpointerInsert` is **not** issued for shared extents — the underlying blob is unchanged
and its existing backpointer is valid for both snapshots. Backpointers are physical, not logical
(METADATA-DESIGN§2.2 / SNAPSHOTS-DESIGN§1.2 table), so they're snapshot-agnostic.

### 1.5 Snapshot deletion

Deleting a snapshot is **two operations**: a small synchronous step that takes the snapshot
out of visibility, and a long-running background scan that physically reclaims the keys.
The synchronous step's WAL cost is `1 + N` entries (one `SnapshotDelete` plus one
`ReconcileEnqueue` per snapshot-aware btree — N = 8 in the current format, every
yes-row of SNAPSHOTS-DESIGN§1.2's table: `Forward`, `TermDictionary`, `Postings`, `Sequences`,
`Ontology`, `Subscriptions`, plus the two radix sidecars `ObjectHistory` and `LocationHistory`).
The scan's WAL cost is a handful of cursor checkpoints, regardless of how many keys
are involved.

**Synchronous step (`SnapshotDelete` WAL op).**
1. In the snapshots btree, set `SNAPSHOT_FLAG_DELETED` in `SnapshotNode.flags`. The node remains in
   place — its `depth` and `skiplist` are still consulted during ancestry checks for siblings
   and descendants — but no new reads are accepted *at* the deleted snapshot id.
2. Enqueue a `WorkKind::SnapshotCleanup` work item per snapshot-aware btree (RECONCILE-DESIGN§1.2). Each
   item carries `(deleted_id, surviving_descendant_id, btree_kind)`. The surviving descendant
   is determined by the tree shape: a leaf snapshot has none (keys are dropped or whited
   out); an interior snapshot collapses to whichever child remains live.
3. Checkpoint. The deletion is durable after this; subsequent crashes resume the cleanup
   from the work-item queue.

**Background scan (reconcile-driven, no per-key WAL).** For each `SnapshotCleanup` item:

```
let mut cursor = scan_state.cursor;            // resumed from ReconcileScan record (RECONCILE-DESIGN§1.3)
for leaf in btree.leaves_from(cursor) {
    for entry in leaf.entries_with_snapshot(deleted_id) {
        match classify(entry, child_set, sibling_set) {
            Drop          => leaf.remove(entry),
            Whiteout      => leaf.replace_value(entry, KEY_TYPE_whiteout),
            Retag(target) => leaf.set_snapshot(entry, target),
        }
    }
    if leaf.is_dirty() { dirty_node_set.add(leaf); }
    cursor = leaf.next_key();
    if elapsed_since_checkpoint() > scan_step_interval {
        emit ReconcileScanStep { scan_id, btree, cursor_key: cursor };
    }
}
```

The mutations above are ordinary in-memory btree changes that hit the JOURNAL-DESIGN§1.4 dirty-node path —
flushed lazily, not journalled per key. `ReconcileScanStep` entries fire at most every
~30 s of scan progress (configurable), bounding WAL cost to O(scan duration), not O(keys).

**Idempotency.** The scan classification is a function of the entry's snapshot id and the
snapshot tree's current shape — both stable after `SnapshotDelete` is journalled. A leaf
that has already been re-tagged no longer contains entries at `deleted_id`, so resuming the
scan from a stale cursor after a crash is a no-op for already-processed leaves. This
removes the need for any per-key undo or redo log.

**Completion.** When the scan reaches the end of every snapshot-aware btree, a final
**SnapshotTreeReorg pass** runs (deferred to the next checkpoint quiesce — see JOURNAL-DESIGN§1.5 — to
avoid racing with live ancestry queries). The pass emits, in this order:

1. One `SnapshotDepthUpdate` (JOURNAL-DESIGN§1.3) per affected descendant — recomputed
   `depth` / `skiplist` / `ancestor_bitmap` reflecting the new effective parent.
2. One `SnapshotUnlink` (JOURNAL-DESIGN§1.3) per deleted snapshot — splices the node out of its
   parent's sibling chain.

Each op is small (≤ ~50 B) and replays locally against the snapshots btree leaf.
Decomposing the pass into per-edit ops (rather than a single bulk reorg op) keeps each
journal entry small regardless of how many descendants are affected.

**Constraints.**
- A snapshot with more than one non-deleted child cannot collapse during cleanup; all but one
  child must itself be deleted (or itself be the `current_replacement` for a deeper snapshot)
  first. This is checked at `SnapshotDelete` time and the request is rejected if not
  satisfied. (Walking a node's children to count them is O(children_count), which is bounded
  by the per-node fan-out in practice — typically 2, occasionally 3-5 after rollbacks.)
- Multiple deletions can run concurrently — each `SnapshotCleanup` work item has its own
  cursor and operates independently (the per-leaf rewrites compose because each touches
  disjoint snapshot-id sets).

### 1.6 Bucket retention under key-level snapshots

A bucket is reclaimable when no live snapshot references its contents. Because backpointers
record `bucket_gen` at insertion (METADATA-DESIGN§2.2), and snapshots only delay extent-deletion (they don't
prevent generation bumps once *all* snapshots have moved on), retention is enforced
**at the extent level**, not at the bucket level:

- An extent's owning key carries a `snapshot` ID; the extent is logically alive as long as any
  ancestor of any live snapshot can see that key.
- Copy GC (ALLOCATION-DESIGN§1.6) treats an extent as live if any backpointer's `bucket_gen` matches the bucket
  *and* its owner's snapshot is still reachable through the snapshot tree.
- When the deleting snapshot pass (SNAPSHOTS-DESIGN§1.5) removes a key, the corresponding backpointer is
  removed too. Once a bucket has no live backpointers, its generation can be bumped.

Snapshot retention scales with **logical changes**, not bucket counts.

### 1.7 Retention policy

Snapshot space cost is per-key. Per-snapshot overhead in steady state:

- Keys unique to each snapshot: typically a few KB for an "idle" snapshot, scaling with logical
  changes since the parent.
- Snapshot tree node: 64 B + ancestor bitmap maintenance.

**Defaults:**

- Auto-created sync snapshots: kept for `min(grace_period, 7 days)`. Tens of thousands of these
  are cheap because divergence is small.
- Labelled / pinned snapshots: kept until explicitly removed. Cost = sum of unique keys.

### 1.8 Rollback

```
brunnr rollback --to <snapshot_label>
```

1. Resolve the label to a `SnapshotId` `S`.
2. Allocate a fresh `SnapshotId` `S'` and link it as a new child of `S`: prepend it to S's
   child sibling list (`S'.next_sibling = S.first_child; S.first_child = S'`). The first-child /
   next-sibling topology (SNAPSHOTS-DESIGN§1.1) admits any number of children, so this works regardless of
   how many sibling branches `S` has accumulated from prior snapshots and rollbacks.
3. Atomically retag the pool's "current" pointer to `S'`. The view now reflects `S`'s state;
   subsequent writes are tagged with `S'` and diverge from `S` at this point. The previously
   current branch remains intact as another sibling of `S'` until explicitly deleted.

No data is copied. The rollback is O(1); subsequent reads pay the snapshot-iteration cost
(SNAPSHOTS-DESIGN§1.3) until the abandoned branch is deleted via SNAPSHOTS-DESIGN§1.5.

---

