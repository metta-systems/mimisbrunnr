# Cluster sync — design

Scope: peer registry, sync bundles, merge, hydration, hybrid clock use.
Governing spec: SPECIFICATION-DESIGN§10, CLUSTER-DESIGN§1 (sync bundles from snapshot diffs), target-system role and boundary in [operations-design.md](operations-design.md#target-system).

Plan: [cluster-implementation-plan.md](cluster-implementation-plan.md) · Open decisions: [cluster-decisions.md](cluster-decisions.md) · Completed: [cluster-completed.md](cluster-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Each node runs its own Mímisbrunnr, is authoritative for its own writes, and converges its assertion graph with peers by exchanging metadata; a node answers queries from local metadata with no network I/O on the query path.

## Selected design

- Convergence is eventual; there is no consensus, quorum or linearisable cross-node write.
- The network transport is provided by Mettā; Mímisbrunnr rides on it.
- Ordering across nodes uses `HybridTimestamp` (SPECIFICATION-DESIGN§10.4).
- ObjectIds carry the writing node's `NodeId` in their top 16 bits.
- **Sync ships state, not history.** A bundle carries what changed between two of the sender's snapshots, read from the snapshot diff (CLUSTER-DESIGN§1) of the trees that hold direct state only: object records (without the content locator, which is local), forward entries, the ontology tree and collection sequences. No operation log is kept for sync and nothing reads journal history.
- **Only direct assertions are synced.** Derived terms, postings, the term dictionary and roll-ups are computed on each node from the synced state and ontology (INDEX-DESIGN§2.4), so peers never exchange them and cannot disagree about them. No posting containers travel.
- **Local ids stay on their node.** Each pool numbers its tags (`TagId`) and its attribute and relation terms (`TermId`) itself (ONTOLOGY-DESIGN§1, INDEX-DESIGN§1.2). Bundles name tags by `TagUid` and terms by value — a tag uid, an attribute key's uid and a `Value`, a relation predicate's uid and a target `ObjectId` — never by a local id; a receiver resolves uids to its own `TagId`s and interns values into its own dictionary.
- **Received changes are local operations.** A receiver applies a bundle by journalling its changes as ordinary local entries and replaying them through the same mutation path, so indexes, rules, constraints and subscriptions (SUBSCRIPTIONS-DESIGN "Selected design") treat them exactly as local mutations; each keeps its origin's hybrid timestamp.
- **What a peer already has.** A sender ships only changes a peer has not seen, and a peer's acknowledgement is its watermark for tombstone expiry (METADATA-DESIGN "Object deletion"). How that knowledge is kept — a snapshot of the sender per peer, a version vector of per-origin hybrid timestamps, or what the merge model needs — is [CLUSTER-D7](cluster-decisions.md), decided together with the merge model.
- **The ontology syncs as records.** A bundle carries the ontology records changed between its snapshots, by uid, ahead of its other changes (ONTOLOGY-DESIGN "Cluster propagation"). Declarations edited on two nodes merge by union of provenance, other fields last-writer-wins by hybrid timestamp.
- **Merge rules** for object state are [CLUSTER-D2](cluster-decisions.md); **collection order** under concurrent inserts is [CLUSTER-D6](cluster-decisions.md).
- **Open parts of CLUSTER-DESIGN§1.** How the base snapshot `A` is chosen for a peer, or whether changes are selected by per-origin timestamps instead, and what an acknowledgement names (CLUSTER-D7); whether a delta carries the terms at `A` (CLUSTER-D2); the shape of sequence deltas (CLUSTER-D6). §1 specifies the bundle for the snapshot-based selection.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Sync bundles from snapshot diffs

With key-level snapshots, the changes between two of the sender's snapshots `A` and `B` (now)
are the keys whose `snapshot` is in `ancestors(B) \ ancestors(A)`, read from the trees of direct
state:

```
let new_only = ancestors(B) - ancestors(A);     // snapshot ids unique to B's lineage

for tree in [ObjectTable /* radix records and ObjectHistory */, Forward, Ontology, Sequences] {
    for entry in tree.entries_with_snapshot_in(&new_only) {   // blocks skipped by snapshot range,
        emit(entry);                                          //   BTREE-DESIGN§1.2
    }
}
```

The scan reads only blocks whose snapshot range meets `new_only`, so its cost follows the
changes, not the pool. Each emitted entry becomes an object delta: the object's record fields and
its direct terms added and removed between `A` and `B`, translated to uids and values.

```text
SyncBundle = [version,                     // positional CBOR (FORMAT-DESIGN§1.6)
              from_node: uint,
              from_snapshot: uint,         // A: what the peer had
              to_snapshot: uint,           // B
              ontology: [OntologyRecord],  // by uid, applied first
              objects: [ObjectDelta],      // HLC-ordered
              sequences: [SequenceDelta]]

ObjectDelta = [oid: uint, hlc: HybridTimestamp,
               record: [state, length, content_hash, created_ns, modified_ns] / null,
               added: [Term], removed: [Term]]

Term = [0, tag_uid: bytes(16)]
     / [1, key_uid: bytes(16), value: Value]
     / [2, predicate_uid: bytes(16), target: uint]
```

The bundle is signed and shipped to peers (per the SPECIFICATION-DESIGN§9.4 key hierarchy).
After applying it, the receiver acknowledges `to_snapshot`.

A new node's first bundle diffs from the empty snapshot: the sender's whole direct state.

**Resilver path.** When a peer comes back from a degraded state with one disk missing, recovery
is driven by the reconcile subsystem (RECONCILE-DESIGN§1). The disk-state change starts a scan
that walks the affected disk's backpointers (METADATA-DESIGN§2.2) and repairs replicas at high
priority. Backpointers are snapshot-agnostic, so the resilver re-fetches extents irrespective
of which snapshot owns them.
