# Cluster sync — open decisions

Design: [cluster-design.md](cluster-design.md) · Plan: [cluster-implementation-plan.md](cluster-implementation-plan.md)

## CLUSTER-D1 — Unique NodeId assignment

ObjectIds embed a 16-bit `NodeId` (SPECIFICATION-DESIGN§2.1). Decide how a node obtains a cluster-unique id (operator-assigned, derived from Mettā identity, coordinated on join) and what happens on collision. Blocks [CLUSTER1](cluster-implementation-plan.md).

## CLUSTER-D2 — Conflict resolution rules

SPECIFICATION-DESIGN§10.7 names conflict resolution without specifying it. Bundles carry changed direct state (CLUSTER-DESIGN§1); define how a receiver merges an object changed on both sides. Candidates, by kind of state:

- **Term sets** (tags, attribute values, relations — attribute keys are multi-valued, so every value is a set element):
  - (a) **three-way merge** against a per-peer base snapshot: base + (local adds ∪ remote adds) − (removes neither side re-added). No per-term metadata; correct when every pair of nodes merges directly or through one hub, but changes reaching a node by two paths (a removal through a hub, a re-add directly) can diverge, and it needs one base snapshot per peer. The maintainer expects it not to scale to multi-node sync;
  - (b) **observed-remove set** (add wins): each added term carries a dot (origin node, counter) and each node a version vector, so concurrent add and remove resolve the same way on every node whatever the path. Costs about 8–10 bytes per direct term in forward entries plus version vectors per peer;
  - investigate whether Mettā's topology (peers and hubs) makes (a) safe, or needs (b).
- **Single-valued fields** (content hash and length, `state`, a ranked collection's score): last-writer-wins by hybrid timestamp, or for content, keeping the losing version as a conflict copy.
- **Delete against a concurrent edit:** delete wins when it is causally after the edit (METADATA-DESIGN "Deletion and cluster sync"); define "causally after" under the chosen set model.
- **Constraints:** an `Exclusive` removal on one node racing an assertion of the other tag on another.

Ontology declarations already merge as ONTOLOGY-DESIGN "Cluster propagation" records (union of provenance, other fields last-writer-wins). Blocks [CLUSTER2](cluster-implementation-plan.md).

## CLUSTER-D3 — Hydration states

SPECIFICATION-DESIGN§10.3 has both `Partial` and `Hydrating` (with progress). Decide whether both states are needed and what each means for reads. Blocks [CLUSTER5](cluster-implementation-plan.md).

## CLUSTER-D4 — Sync transport

DESIGN notes suggest iroh as the sync transport; the target-system boundary ([operations-design.md](operations-design.md#boundary)) places the transport outside Mímisbrunnr (Mettā provides peer reach). Decide whether Mímisbrunnr defines a transport binding at all, and if so which.

## CLUSTER-D7 — What a sender knows of each peer

A sender ships only what a peer has not seen, and tombstones expire once every peer has acknowledged the deletion. Options, tied to CLUSTER-D2:

- (a) **A sender snapshot per peer:** the sender keeps the snapshot it last shipped to each peer and diffs from it (CLUSTER-DESIGN§1). Simple diffs, but one retained snapshot per peer per node, and a relayed change is shipped again on every path.
- (b) **Version vectors:** each node keeps, per origin node, the newest hybrid timestamp it has applied; a sender ships the changes newer than the peer's vector entry for each origin, selected by the timestamps carried with direct state rather than by snapshot ancestry. Scales with nodes, not with snapshots; changes relayed through hubs are recognised as seen. Needs per-change timestamps in the trees that are diffed.
- (c) **Delta-state CRDT anti-entropy:** what (b) does, with the version vectors and dots of an observed-remove set (CLUSTER-D2 (b)) doubling as the record of what is known.

Shapes [CLUSTER1](cluster-implementation-plan.md) and [CLUSTER9](cluster-implementation-plan.md), and the watermark of METADATA-DESIGN "Object deletion".

## CLUSTER-D6 — Collection order under concurrent inserts

Ordered collections keep sparse `u64` positions and renumber when a gap runs out (INDEX-DESIGN§3.3). Renumbering cannot be merged across nodes, and two nodes inserting at the same place pick colliding positions. Candidates:

- (a) **Fractional indexing** with a node-id tie-break: positions become variable-length byte strings between their neighbours, never renumbered. Simple and fits the `(term, position)` sequences tree; concurrent inserts at one place may interleave.
- (b) **LSEQ / Logoot:** the same with bounded position growth.
- (c) **RGA or Fugue:** no interleaving anomalies (Fugue is the strongest), but tombstones and a tree per sequence, heavier than sparse positions.

Moves are a delete and an insert, or Kleppmann's move operation where an element must never be duplicated. The choice changes the forward entry's order slots (INDEX-DESIGN§2.1) and the `Sequences` key (INDEX-DESIGN§3.3) and removes renumbering on a single node too. Blocks the sequence part of [CLUSTER2](cluster-implementation-plan.md); shapes [INDEX15](index-implementation-plan.md).

## Future work

- Remote storage backends (SPECIFICATION-DESIGN§10.9), including an IPFS-style frontend developed outside Mettā.
