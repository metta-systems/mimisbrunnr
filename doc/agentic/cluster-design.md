# Cluster support — design

Scope: the filesystem's side of multi-node operation — the cluster-support API a cluster manager drives: change export and apply, merge, peer cursors, content transfer with durable progress, the hybrid clock and node identity. Transport, wire format, peer discovery, scheduling and the sync protocol belong to the cluster manager, not to the filesystem.
Governing spec: SPECIFICATION-DESIGN§10, CLUSTER-DESIGN§1–§4, target-system role and boundary in [operations-design.md](operations-design.md#target-system).

Plan: [cluster-implementation-plan.md](cluster-implementation-plan.md) · Open decisions: [cluster-decisions.md](cluster-decisions.md) · Completed: [cluster-completed.md](cluster-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Each node runs its own Mímisbrunnr, is authoritative for its own writes, answers queries from local metadata with no network I/O, and converges with its peers. Four properties drive the design:

1. **Resilient to delay and absence.** A node may work alone for any length of time, then rejoin and exchange everything that changed on either side, with no change lost on either.
2. **Observable.** What is syncing, with which peer, and how far it has got can be shown to users.
3. **Resumable.** A crash or a dropped link loses at most the work since the last durable step: a 20 GB transfer over a slow link resumes at the first piece it does not yet hold.
4. **Driven from outside.** A separate cluster manager moves the traffic; the filesystem supplies the state and operations that make that possible, behind a privileged interface ordinary users do not see.

## Selected design

- **Division of labour.** The filesystem does no networking. A **cluster manager** — a separate component holding the cluster-support capability — chooses peers, carries data in whatever wire format and transport it uses, schedules exchanges and publishes sync state. The filesystem keeps everything that must survive a crash of either: what each peer has seen, which content is wanted, and how far every transfer has got.
- **Convergence is eventual.** There is no consensus, quorum or linearisable cross-node write; ordering across nodes uses `HybridTimestamp` (SPECIFICATION-DESIGN§10.4), and ObjectIds carry the writing node's `NodeId` in their top 16 bits.
- **Sync ships state, not history.** Exporting for a peer yields what changed in direct state since the peer's cursor (CLUSTER-DESIGN§1): object records without their content locator, forward entries (with collection order slots, INDEX-DESIGN§2.1) and ontology records. No operation log is kept, so a peer absent for any time catches up by one export of everything it has not seen, and nothing on either side expires while it is away.
- **Only direct state travels.** Derived terms, postings, the dictionary and roll-ups are computed on each node from the synced state and ontology (INDEX-DESIGN§2.4), so peers never exchange them and cannot disagree about them.
- **Local ids stay on their node.** Exported changes name tags by `TagUid` and terms by value — a tag uid, an attribute key's uid and a `Value`, a predicate's uid and a target `ObjectId` — never by `TagId` or `TermId`; applying them resolves uids to local ids and interns values locally.
- **Applying is idempotent and local.** Applied changes are journalled as ordinary local mutations, keeping each change's origin timestamp, so indexes, rules and subscriptions treat them exactly as local writes. The merge is a function of the states merged (CLUSTER-D2), so applying a batch twice — after a cluster-manager crash, or because two peers relayed it — changes nothing the second time. Constraints are not enforced on applied changes; violations are reported (ONTOLOGY-DESIGN "Rules and constraints").
- **The ontology goes first.** An export lists changed ontology records ahead of object changes; declarations edited on two nodes merge by union of provenance, other fields last-writer-wins by hybrid timestamp (ONTOLOGY-DESIGN "Cluster propagation").
- **Peer cursors are the filesystem's.** Each known peer has a record (CLUSTER-DESIGN§2) holding what the filesystem has exported to it and had acknowledged, what it has applied from it, and when. The cursors bound tombstone expiry (METADATA-DESIGN "Object deletion") and whatever the filesystem retains for diffing (CLUSTER-D7); a cluster manager that crashes or is replaced resumes from them.
- **Content moves as pieces.** Every object's content is a list of hashed pieces (METADATA-DESIGN§1.3), so every transfer is "these pieces of this object": the receiver learns the object's piece list from the sender, records it with every piece absent (CLUSTER-DESIGN§3), fetches absent pieces in any order and from any peer that holds them, and verifies each against its hash before storing it. A stored piece is durable at once; after a crash the absent pieces are exactly what remains to fetch.
- **Transfers are durable, visible records.** Every content transfer, in either direction, is a record (CLUSTER-DESIGN§4) with its state and progress. The filesystem creates inbound records itself when it wants content — an open of remote content, a hydration or pin rule, a resilver — and advances progress as pieces land; the cluster manager services them, creates records for what it serves to others, and pauses, resumes or cancels them. The records are what a UI shows; how they reach UI applications is [CLUSTER-D11](cluster-decisions.md).
- **Bytes on the wire are ciphertext where keys allow.** Pieces are encrypted by owner (SPECIFICATION-DESIGN§9.3) and moves never re-encrypt, so between nodes holding the same keys a piece is read and written as stored; the cluster manager seals the channel. Who holds which keys is [ENCRYPTION-D4](encryption-decisions.md)/[ENCRYPTION-D5](encryption-decisions.md).
- **The interface is privileged.** The cluster-support operations (CLUSTER-DESIGN§5) are reachable only through their own capability, separate from user sessions ([ACCESS-D4](access-decisions.md), [CLUSTER-D12](cluster-decisions.md)).
- **Open:** merge rules for object state ([CLUSTER-D2](cluster-decisions.md)); collection order under concurrent inserts ([CLUSTER-D6](cluster-decisions.md)); what a cursor holds ([CLUSTER-D7](cluster-decisions.md)); which nodes hold which content ([CLUSTER-D10](cluster-decisions.md)); reads of partially present objects ([CLUSTER-D3](cluster-decisions.md)).

## Rejected alternatives

- **The filesystem as a sync daemon** with its own wire format, transport binding and peer protocol. Transport and scheduling are policy that changes with deployment; keeping them in a separate manager leaves the filesystem a state store with a stable interface.
- **An operation log replicated between nodes.** It must be kept until every peer has it, grows with absence, and lets derived state drift; state deltas need neither.
- **Per-chunk presence lists** (`present_on` on every chunk-index entry). Every copy or eviction would change sync state per piece; see CLUSTER-D10 for the options kept open.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Change export from snapshot diffs

With key-level snapshots, the changes between two of the exporter's snapshots `A` and `B` (now)
are the keys whose `snapshot` is in `ancestors(B) \ ancestors(A)`, read from the trees of direct
state:

```
let new_only = ancestors(B) - ancestors(A);     // snapshot ids unique to B's lineage

for tree in [ObjectTable /* radix records and ObjectHistory */, Forward, Ontology] {
    for entry in tree.entries_with_snapshot_in(&new_only) {   // blocks skipped by snapshot range,
        emit(entry);                                          //   BTREE-DESIGN§1.2
    }
}
```

The scan reads only blocks whose snapshot range meets `new_only`, so its cost follows the
changes, not the pool. Each emitted entry becomes an object change: the object's record fields
and its direct terms added and removed between `A` and `B`, translated to uids and values. This
is the selection when a cursor names a snapshot; whether cursors name snapshots or per-origin
timestamps is CLUSTER-D7.

An export is a sequence of bounded batches, each with the cursor reached after it, so the
cluster manager can ship, acknowledge and resume an export of any size. A new peer's first export
diffs from the empty snapshot: the exporter's whole direct state.

The change types are interface types, not an on-disk format:

```text
ObjectChange = { oid, hlc: HybridTimestamp,
                 record: { state, length, content_hash, created_ns, modified_ns } | none,
                 added: [Term], removed: [Term],
                 ordered: [(Term, order)] }       // order slots set or changed (INDEX-DESIGN§2.1)

Term = Tag(tag_uid) | Attr(key_uid, Value) | Relation(predicate_uid, target: ObjectId)
```

**Resilver.** A node returning with a disk missing repairs replicas through reconcile
(RECONCILE-DESIGN§1): the disk-state change starts a scan over the disk's backpointers
(METADATA-DESIGN§2.2). Pieces with no surviving local replica become wanted content — inbound
transfer records (CLUSTER-DESIGN§4) the cluster manager fills from peers.

## 2. Peer records

`RootPointer.cluster_peers_root` (ENGINE-DESIGN§1.2) roots a B+ tree of
`BtreeKind::ClusterPeers`, snapshot-agnostic, keyed by the peer's `NodeId` (`u16`), with
positional, versioned CBOR values (FORMAT-DESIGN§1.6), schema version 1:

```text
PeerRecord = [version,
              node_uid: bytes(16),       // the peer's stable identity
              name: text,                // for display
              exported: Cursor,          // what has been exported to the peer and acknowledged
              applied: Cursor,           // what has been applied from the peer
              last_export_ns: int,       // last acknowledged export
              last_apply_ns: int,        // last applied batch
              last_error: text / null]   // last failure the cluster manager reported
```

`Cursor` is the representation CLUSTER-D7 selects. Peer records change by the cluster-support
operations and commit like any mutation; adding and removing a peer commit by checkpoint as
administrative changes (POOL-DESIGN§1.2).

## 3. Absent pieces

A piece whose hash and length are known but whose bytes are not local is a content-map key
(METADATA-DESIGN§1.3) with `PIECE_FLAG_ABSENT` set and the shared-piece shape (hash, length,
flags; no location). Recording an object's piece list writes every piece absent; storing a
fetched piece, after its hash verifies, replaces the absent key with a private or shared piece
in one `ReplacePieces` (JOURNAL-DESIGN§1.3). An absent piece holds no space and no reference.
Which reads succeed on an object with absent pieces, and how the record's content kind marks it,
is CLUSTER-D3.

## 4. Transfers

`RootPointer.transfers_root` roots a B+ tree of `BtreeKind::Transfers`, snapshot-agnostic, keyed
by `(oid: u64, direction: u8, peer: u16)` — direction 0 inbound, 1 outbound; `peer` is 0 for an
inbound transfer any peer may serve — with positional, versioned CBOR values, schema version 1:

```text
TransferRecord = [version,
                  state: uint,           // 0 wanted, 1 active, 2 paused, 3 done, 4 failed, 5 cancelled
                  cause: uint,           // 0 open, 1 hydration rule, 2 pin, 3 resilver, 4 peer request
                  priority: uint,
                  pieces_total: uint, pieces_done: uint,
                  bytes_total: uint, bytes_done: uint,
                  created_ns: int, updated_ns: int,
                  last_error: text / null]
```

- An **inbound** record's progress follows from the content map: `pieces_done` and `bytes_done`
  are the object's present pieces, advanced in the commit that stores each piece and recomputed
  from the content map at mount, so they never run ahead of what is durable.
- An **outbound** record's progress is what the cluster manager reports it has sent; it is
  advisory and survives a crash only to the last report.
- `done`, `failed` and `cancelled` records are kept until the cluster manager removes them, so
  a UI can show recent history.

## 5. Cluster-support operations

The operations the cluster manager calls, through the cluster-support capability:

| Group | Operations |
|---|---|
| Identity and time | the node's `NodeId` and uid; the current hybrid timestamp; observe a peer's timestamp |
| Peers | put, get, list and remove peer records |
| Changes | export the next batch for a peer from its cursor; acknowledge a batch; apply a batch of changes from a peer, returning what was applied and the constraint violations reported |
| Content | an object's piece list; read a stored piece by hash or by `(oid, end_offset)`; record a piece list as absent; store a fetched piece (verified by hash); list the pieces an object still lacks |
| Transfers | list transfers by state, priority, object or peer; create, pause, resume, cancel and remove; report outbound progress |
| Retention | what each peer's cursor holds back (snapshots, tombstones) |

Every operation is a mutation or a read like any other: mutations are journalled and durable at
commit, and none depends on state held only by the cluster manager.
