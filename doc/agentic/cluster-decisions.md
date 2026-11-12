# Cluster support — open decisions

Design: [cluster-design.md](cluster-design.md) · Plan: [cluster-implementation-plan.md](cluster-implementation-plan.md)

## CLUSTER-D1 — Unique NodeId assignment

ObjectIds embed a 16-bit `NodeId` (SPECIFICATION-DESIGN§2.1). Decide how a node obtains a cluster-unique id (operator-assigned, derived from Mettā identity, assigned by the cluster manager on join) and what happens on collision. Blocks [CLUSTER1](cluster-implementation-plan.md).

## CLUSTER-D2 — Conflict resolution rules

Applying a peer's changes (CLUSTER-DESIGN§1) must merge an object changed on both sides, deterministically, so that every node converges and re-applying a batch changes nothing. The merge runs in the filesystem; what it needs stored (dots, version vectors, a base) is part of the format. Candidates, by kind of state:

- **Term sets** (tags, attribute values, relations — attribute keys are multi-valued, so every value is a set element):
  - (a) **three-way merge** against a per-peer base snapshot: base + (local adds ∪ remote adds) − (removes neither side re-added). No per-term metadata; correct when every pair of nodes merges directly or through one hub, but changes reaching a node by two paths (a removal through a hub, a re-add directly) can diverge, and it needs one base snapshot per peer. The maintainer expects it not to scale to multi-node sync;
  - (b) **observed-remove set** (add wins): each added term carries a dot (origin node, counter) and each node a version vector, so concurrent add and remove resolve the same way on every node whatever the path. Costs about 8–10 bytes per direct term in forward entries plus version vectors per peer;
  - investigate whether the topologies the cluster manager builds (peers and hubs) make (a) safe, or need (b).
- **Single-valued fields** (content hash and length, `state`, a ranked collection's score): last-writer-wins by hybrid timestamp, or for content, keeping the losing version as a conflict copy.
- **Delete against a concurrent edit:** delete wins when it is causally after the edit (METADATA-DESIGN "Deletion and cluster sync"); define "causally after" under the chosen set model. Hybrid timestamps alone cannot tell an edit made after seeing the deletion from one made concurrently — a node that tags an object it has not yet learned is deleted produces a timestamp later than the deletion — so the rule needs causal information (version vectors or dots) or must treat every later edit as concurrent. Decide also whether a dropped concurrent edit is surfaced (an event, a conflict record) or silent.
- **Constraints:** an `Exclusive` removal on one node racing an assertion of the other tag on another.

Ontology declarations already merge as ONTOLOGY-DESIGN "Cluster propagation" records (union of provenance, other fields last-writer-wins). Blocks [CLUSTER2](cluster-implementation-plan.md).

## CLUSTER-D3 — Partially present content

An object whose chunk list is recorded but some chunks are absent (CLUSTER-DESIGN§3, chunk entries with `CHUNK_FLAG_ABSENT` and no replicas) — being fetched, kept as a stub by a `Stub` presence rule, or left metadata-only — needs defined reads and a defined record. Presence itself is derived from chunks, transfer records and presence rules (SPECIFICATION-DESIGN§10.3), so no `Partial` or `Hydrating` state is stored. Decide:

- whether a read of a present range succeeds while other ranges are absent, and what a read of an absent range does (fail with a defined error, block while an inbound transfer is active, or create one with cause `open`);
- how the record marks it: the `Chunks` content kind with absent keys, or `Remote` until the last chunk lands.

Blocks [CLUSTER5](cluster-implementation-plan.md).

## CLUSTER-D6 — Collection order under concurrent inserts

Ordered collections keep sparse `u64` positions and renumber when a gap runs out (INDEX-DESIGN§3.3). Renumbering cannot be merged across nodes, and two nodes inserting at the same place pick colliding positions. Candidates:

- (a) **Fractional indexing** with a node-id tie-break: positions become variable-length byte strings between their neighbours, never renumbered. Simple and fits the `(term, position)` sequences tree; concurrent inserts at one place may interleave.
- (b) **LSEQ / Logoot:** the same with bounded position growth.
- (c) **RGA or Fugue:** no interleaving anomalies (Fugue is the strongest), but tombstones and a tree per sequence, heavier than sparse positions.

Moves are a delete and an insert, or Kleppmann's move operation where an element must never be duplicated. The choice changes the forward entry's order slots (INDEX-DESIGN§2.1) and the `Sequences` key (INDEX-DESIGN§3.3) and removes renumbering on a single node too. Blocks the order part of [CLUSTER2](cluster-implementation-plan.md); shapes [INDEX15](index-implementation-plan.md).

## CLUSTER-D7 — What a cursor holds

A peer record's cursors (CLUSTER-DESIGN§2) say what the peer has seen, so an export ships only what it lacks, and tombstones expire once every peer's cursor has passed the deletion. Options, tied to CLUSTER-D2:

- (a) **A snapshot per peer:** the cursor names the snapshot last exported to the peer, and export diffs from it (CLUSTER-DESIGN§1). Simple diffs, but one retained snapshot per peer per node, and a change relayed through several peers is exported again on every path.
- (b) **Version vectors:** the cursor holds, per origin node, the newest hybrid timestamp the peer has; export selects changes newer than the peer's entry for each origin, by the timestamps carried with direct state rather than by snapshot ancestry. Scales with nodes, not with snapshots; changes relayed through hubs are recognised as seen. Needs per-change timestamps in the trees diffed.
- (c) **Delta-state CRDT anti-entropy:** what (b) does, with the version vectors and dots of an observed-remove set (CLUSTER-D2 (b)) doubling as the record of what is known.

Fixes the `Cursor` type of the peer record and its size, which [ENGINE-D2](engine-decisions.md) and [POOL-D4](pool-decisions.md) need. Shapes [CLUSTER1](cluster-implementation-plan.md) and [CLUSTER9](cluster-implementation-plan.md), and the watermark of METADATA-DESIGN "Object deletion".

## CLUSTER-D9 — Exports for untrusted relays

A node that is always on but not trusted (SPECIFICATION-DESIGN§9.5, `Untrusted`: stores and relays ciphertext only) cannot diff or merge plaintext state. The cluster manager decides whether such relays are used; the filesystem decides what it can hand them. Options:

- (a) **Sealed export batches:** the filesystem seals an export batch for its recipients, and the relay stores and forwards it; a node returning after a long absence then receives the batches it missed in order rather than one delta, so the relay's storage grows with what it holds for the slowest peer, and batches need an expiry or a compaction by a trusted node.
- (b) **Trusted relays only:** untrusted nodes carry transport traffic but never hold state; the filesystem exports only to trusted peers.
- (c) **State a relay can merge blindly:** only for state whose merge needs no plaintext (append-only sets of sealed items); does not cover diffing.

Ties to [CLUSTER-D7](#cluster-d7--what-a-cursor-holds) and [ENCRYPTION-D4](encryption-decisions.md) (trust tier semantics).

## CLUSTER-D10 — Who holds which content

To fetch an object's chunks the cluster manager must know which peers have them. Options:

- (a) **Ask the origin, then peers:** the object's origin node first (`RemoteLocator.origin_node`), then any peer; no new state, but the origin may be offline while another node holds a copy.
- (b) **Per-object presence in synced state:** the set of nodes holding a full copy, carried with the object's record in exports; a few node ids per object, changing only when copies are made or evicted.
- (c) **Per-chunk presence** in the chunk index: precise, but every copy or eviction changes sync state per chunk (rejected in CLUSTER-DESIGN for that reason, kept here only for comparison).

Leaning (b). Shapes [CLUSTER10](cluster-implementation-plan.md).

## CLUSTER-D11 — Publishing sync state to applications

Transfer and peer records (CLUSTER-DESIGN§2, §4) hold everything a UI needs — what syncs, with whom, how far — but only the cluster manager may read them. Options:

- (a) **The cluster manager publishes:** it reads the records through its capability and republishes a view to UI applications in its own format and with its own access policy.
- (b) **The filesystem exposes a read-only view:** a narrower capability for reading transfer and peer state, or transfers surfaced as system labels (`_transfer:inbound`, `_transfer:active`) that queries and subscriptions can use, subject to access control ([ACCESS-D1](access-decisions.md)).
- (c) **Both:** the filesystem's read-only view for live progress, the manager's publication for what only it knows (link quality, peer reachability).

Still being considered. Shapes [CLUSTER11](cluster-implementation-plan.md).

## CLUSTER-D12 — The cluster-support capability

The operations of CLUSTER-DESIGN§5 are privileged. Decide how the capability is granted (one cluster manager per node, or several), whether it is diminishable (e.g. a read-only cluster capability for monitoring, which would answer CLUSTER-D11 (b)), how it maps onto Mettā capability types (ACCESS-D4's candidate mapping), and whether applying changes runs with the origin's identity or the manager's for access control ([ACCESS-D8](access-decisions.md)).

## CLUSTER-D13 — Implementing verified streaming

Transfers within a chunk use BLAKE3's tree in the Bao verified-streaming form with a group size chosen per request and subtrees condensed into their parent hash (CLUSTER-DESIGN§3.1). The workspace already depends on `blake3` for content hashes. Options: (a) the `bao` crate (the reference implementation by BLAKE3's authors; fixed 1 KiB granularity, encoder and decoder for combined and outboard streams); (b) `bao-tree` (iroh's; configurable chunk groups and range requests, closer to the per-request group size); (c) build the outboard and verification on `blake3`'s lower-level tree API (`blake3::hazmat`), with no new dependency and the frontier logic our own. Criteria: `no_std` + `alloc` support for the device build (operations-design "Deployment targets"), group sizes from 1 KiB to the whole chunk, verifying out-of-order groups, and the dependency itself (a new workspace dependency is a maintainer decision). Blocks [CLUSTER10](cluster-implementation-plan.md).

## Future work

- Remote storage backends (SPECIFICATION-DESIGN§10.9), including an IPFS-style frontend developed outside Mettā and a namespace layer over IPFS.
