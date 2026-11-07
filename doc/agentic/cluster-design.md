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
- **Only direct assertions are synced.** Derived terms are computed on each node from the synced ontology (INDEX-DESIGN§2.4), so peers never exchange them and cannot disagree about them.
- **Local ids stay on their node.** Each pool numbers its tags (`TagId`) and its attribute and relation terms (`TermId`) itself (ONTOLOGY-DESIGN§1, INDEX-DESIGN§1.2). Sync payloads name tags by `TagUid` and terms by value — a tag uid, or an attribute key's uid and a `Value` — never by a local id; a receiver resolves uids to its own `TagId`s and interns values into its own dictionary.
- **Received ops are local operations.** A receiver applies a bundle's ops by journalling them as ordinary local entries and replaying them through the same mutation path, so indexes, rules, constraints and subscriptions (SUBSCRIPTIONS-DESIGN "Selected design") treat them exactly as local mutations; each keeps its origin's hybrid timestamp.
- **The ontology syncs as ops.** A bundle's ontology changes are its `OntologyPut` / `OntologyDelete` entries with local ids replaced by uids, in HLC order ahead of the bundle's other ops (ONTOLOGY-DESIGN "Cluster propagation"). Declarations edited on two nodes merge by union of provenance, other fields last-writer-wins by HLC.

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
   ontology_ops: [OntologyOp]          // OntologyPut / OntologyDelete by uid, HLC-ordered,
                                       //   applied before ops
   posting_deltas: [{ term, high, snapshot, container }]  // per-container divergences (INDEX-DESIGN§3.1);
                                                          //   term = tag uid or (key uid, Value)
```

The bundle is signed and shipped to peers (per the SPECIFICATION-DESIGN§9.4 key hierarchy).

**Resilver path.** When a peer comes back from a degraded state with one disk missing, recovery
is driven by the reconcile subsystem (RECONCILE-DESIGN§1). The disk-state change triggers a scan that
backpointer-walks (METADATA-DESIGN§2.2) the affected disk and enqueues `ReplicaRepair` work items at high
priority. Backpointers are snapshot-agnostic, so the resilver re-fetches extents irrespective
of which snapshot owns them.

