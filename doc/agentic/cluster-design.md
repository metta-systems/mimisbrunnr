# Cluster sync — design

Scope: peer registry, sync op stream, bitmap delta sync, hydration, hybrid clock use.
Governing spec: SPECIFICATION-DESIGN§10, CLUSTER-DESIGN§1 (cluster diff between snapshots), target-system role and boundary in [operations-design.md](operations-design.md#target-system).

Plan: [cluster-implementation-plan.md](cluster-implementation-plan.md) · Open decisions: [cluster-decisions.md](cluster-decisions.md) · Completed: [cluster-completed.md](cluster-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Each node runs its own Mímisbrunnr, is authoritative for its own writes, and converges its assertion graph with peers by exchanging metadata; a node answers queries from local metadata with no network I/O on the query path.

## Selected design

- Convergence is eventual; there is no consensus, quorum or linearisable cross-node write.
- The network transport is provided by Mettā; Mímisbrunnr rides on it.
- Snapshot diffs are the unit of sync; sync waits for [snapshots-design.md](snapshots-design.md).
- Ordering across nodes uses `HybridTimestamp` (SPECIFICATION-DESIGN§10.4).
- ObjectIds carry the writing node's `NodeId` in their top 16 bits.
- **`value_hash` stays on its node.** Each pool hashes attribute values with its own secret (INDEX-DESIGN§1.2). Sync payloads carry attribute values, never `value_hash`; a receiver hashes them with its own secret. Bitmap-delta sync covers tag bitmaps only; KV index state converges through the assertion ops. No key is distributed between nodes.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Cluster diff between snapshots

With key-level snapshots, the cluster-sync diff between two snapshots `A` and `B` becomes a
**snapshot-set difference**: stream every key whose `snapshot` is in `ancestors(B) \ ancestors(A)`.

```
let from_set = ancestors(A);            // 128-bit bitmap + skiplist walk; small
let to_set   = ancestors(B);
let new_only = to_set - from_set;       // snapshot IDs unique to B's lineage

for tree in snapshot_aware_btrees {
    for key in tree.iter() {
        if new_only.contains(key.snapshot) {
            emit(SyncOp::from(key));
        }
    }
}
```

The diff is a single btree range scan filtered by the small `new_only` set. The same operation
serves recent and old snapshots — no special case for either.

The result is a `SyncBundle`:

```
SyncBundle (CBOR):
   from_snapshot: u32
   to_snapshot: u32
   from_node: NodeId
   ops: [SyncOp]                       // canonical, HLC-ordered
   ontology_delta: Option<OntologyDelta>
   bitmap_deltas: [{ tag_id, snapshot, added/removed }]   // for tag bitmap divergences
```

The bundle is signed and shipped to peers (per the SPECIFICATION-DESIGN§9.4 key hierarchy).

**Resilver path.** When a peer comes back from a degraded state with one disk missing, recovery
is driven by the reconcile subsystem (RECONCILE-DESIGN§1). The disk-state change triggers a scan that
backpointer-walks (METADATA-DESIGN§2.2) the affected disk and enqueues `ReplicaRepair` work items at high
priority. Backpointers are snapshot-agnostic, so the resilver re-fetches extents irrespective
of which snapshot owns them.

