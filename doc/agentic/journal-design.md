# Journal (WAL) — design

Scope: `crates/wal`, the WAL projection in `crates/engine/src/wal_proj.rs`, checkpoint cadence in `crates/engine`.
Governing spec: JOURNAL-DESIGN§1 (§1.1 journal location, §1.2 journal sets, §1.3 ops, §1.4 pins and reclaim, §1.5 commits and checkpoints, §1.6 retention, §1.7 mirroring, §1.8 data paths), SPECIFICATION-DESIGN§6.4.

Plan: [journal-implementation-plan.md](journal-implementation-plan.md) · Open decisions: [journal-decisions.md](journal-decisions.md) · Completed: [journal-completed.md](journal-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

The journal is the durability point for every mutation on any medium — SD card, SSD, NVMe, HDD — and for every write shape from tiny files to long streams; B+ tree nodes and object data are written behind or beside it, and history past the trim point is kept only when a pool asks for it (JOURNAL-DESIGN§1.6).

## Selected design

- **Journal buckets, write-once journal sets.** The journal is an ordered list of allocator buckets named in the superblock. Each commit writes one new *journal set* (jset) — a header and a batch of entries — that is never rewritten, so a torn write loses only the set not yet acknowledged.
- **Compact entries.** One 64-byte header, one checksum, one compression unit and one AEAD tag per jset; 8 bytes of header per entry. An `AddTag` costs 24 bytes.
- **The root travels in the journal.** A checkpoint is a `Checkpoint` entry carrying the new root list (ENGINE-DESIGN§1.2); the superblock holds only a hint naming the jset of the newest checkpoint, rewritten on clean unmount and when the journal's bucket list changes.
- **Adaptive group commit.** Idle commits seal and write at once; under load, commits join the next set and sets grow toward 256 KiB. Sets are pipelined and acknowledged in sequence. FUA where the device offers it, a cache flush otherwise; one flush per commit, with data extents verified by checksum on recovery instead of a second flush.
- **Caller-chosen durability.** `Sync` returns once durable; `Buffered { max_delay }` becomes durable within a bound.
- **Data beside the journal.** File data goes to blob extents; only tiny writes ride inside the journal, to be packed later. Streams are append-only objects made durable by periodic `ExtendObject` entries.
- **Optional retained history.** Journal buckets may be kept past the trim point within a per-pool cap (default none); subscriptions do not depend on it — they keep their own pending events.
- **Journal pins.** Each `LoadedNode` holds one `JournalPin { lsn_min, lsn_max, count }` over its pending journal; the trim point is the lowest live pin or the last checkpoint. `JournalPin` lives in `mimisbrunnr-storage`, beside `LoadedNode`.
- **The engine starts no thread.** It exposes `commit`, `checkpoint` and a non-blocking query of what is due; the embedder drives them — a `std`-gated helper thread on the host, the filesystem server's own thread or event loop on Vesper.

## Invariants

- No mutation is reported durable before the jset holding it, and every jset before it, is durable.
- A jset is never overwritten while any retained history or pending node needs it.
- No durable jset references blob data that did not land: recovery verifies extents named in the unflushed tail and drops entries whose data fails.
- Replay from the committed root reproduces the committed state.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Journal

The journal is the **redo log** of every mutation: each entry records one logical change (add
tag, set attribute, write extent, bucket transition, …). B+ tree nodes are **not** rewritten
per mutation — they are flushed lazily as appended runs or copy-on-write regions
(JOURNAL-DESIGN§1.4) — and object data is written to blob extents beside the journal
(JOURNAL-DESIGN§1.8). The journal is therefore the **source of truth** for any state newer than
the committed root.

It lives on the pool's fastest tier (`StorageTier::Hot` when present, SPECIFICATION-DESIGN§8.1) and is
designed to behave well on every medium:

| Medium | What the design relies on |
|---|---|
| SSD / NVMe | write-once sets (no read-modify-write of partial flash pages); sets up to 256 KiB; pipelining across the queue; FUA writes |
| SD card / eMMC | strictly sequential, append-only writes within whole buckets; one flush per commit |
| HDD | contiguous journal buckets, so a set write and recovery's scan are sequential; batching amortises seeks |

Per-mutation cost is an 8-byte entry head plus its binary payload (an `AddTag` is 24 bytes),
plus a share of its set's 64-byte header. Tree rewrites are amortised across all mutations that
touched each node since its last flush.

### 1.1 Journal location

The journal is an ordered list of buckets of allocator data type `Journal`
(ALLOCATION-DESIGN§1.2), recorded in the superblock of each disk that carries journal buckets:

```rust
// Superblock fields (ENGINE-DESIGN§1.1)
journal_bucket_count: u16,                   // buckets in the list; 0 = this disk carries no journal
journal_buckets:      [u32; 256],            // bucket numbers on this disk, in journal order
```

- The buckets are written in list order, each from its start; a bucket is a sequence of jsets
  (JOURNAL-DESIGN§1.2) followed by unwritten space.
- The allocator places journal buckets contiguously when it can, so on an HDD the journal is one
  sequential region. Typical sizes: a few MiB on a device, 64 MiB or more on a host.
- Growing, shrinking or moving the journal rewrites the bucket list — the only reason the
  journal ever causes a superblock write. Freed journal buckets are discarded (TRIM) by the
  allocator (ALLOCATION-DESIGN§1.7).
- Recovery reads the first jset of every listed bucket to find the newest, then walks forward
  from the trim point (JOURNAL-DESIGN§1.5).

### 1.2 Journal sets

Every commit writes **one new journal set** (jset): a header, then the batch of entries, padded
with zeros to a multiple of 4 KiB, at the next unwritten position of the current journal bucket.
A jset is never rewritten. A torn or lost write can therefore destroy only the jset being
written — which no caller has been told is durable — never an earlier one. A jset never spans
two buckets and is at most 256 KiB; a commit whose batch exceeds either bound is split into
consecutive jsets acknowledged together.

```rust
#[repr(C)]
struct JsetHeader {                          // 64 bytes
    magic:        u32,                       //  [0..4]   "JSET"
    version:      u16,                       //  [4..6]   jset format version
    flags:        u16,                       //  [6..8]   JSET_FLAG_*
    seq:          u64,                       //  [8..16]  monotonic jset sequence number
    last_seq:     u64,                       // [16..24]  oldest seq still needed (JOURNAL-DESIGN§1.4)
    hlc_base:     HybridTimestamp,           // [24..40]  entry timestamps are offsets from this
    payload_len:  u32,                       // [40..44]  bytes of entries as stored (after compression)
    entry_count:  u32,                       // [44..48]
    plain_len:    u32,                       // [48..52]  bytes of entries before compression
    _pad:         u32,                       // [52..56]
    checksum:     u64,                       // [56..64]  over the whole jset with this field zero
}
// followed by: entries (payload_len bytes), then, if JSET_FLAG_ENCRYPTED, a 16-byte AEAD tag;
// then zero padding to a multiple of 4 KiB.

const JSET_FLAG_ENCRYPTED:  u16 = 1 << 0;    // entries are AEAD-encrypted (ENCRYPTION-DESIGN§1)
const JSET_FLAG_COMPRESSED: u16 = 1 << 1;    // entries are compressed before encryption

#[repr(C)]
struct EntryHead {                           // 8 bytes, 8-aligned within the plaintext entries
    op_kind:   u8,                           // [0..1]  WalOpKind
    flags:     u8,                           // [1..2]  ENTRY_FLAG_*
    len:       u16,                          // [2..4]  payload bytes that follow (excluding padding)
    hlc_delta: u32,                          // [4..8]  nanoseconds after hlc_base.physical_ns
}
// followed by the op's payload record (JOURNAL-DESIGN§1.3), zero-padded to a multiple of 8.

const ENTRY_FLAG_HLC_EXPLICIT: u8 = 1 << 0;  // a full 16-byte HybridTimestamp precedes the payload
                                             //   (ops applied from another node keep their HLC)
```

**LSN.** An entry's LSN is `seq << 16 | index`, where `index` is its position in the jset
(< 2¹⁶). Every LSN in the format — run `journal_seq`, the root list's `lsn`, subscription cursors,
dictionary entries' `last_modify_lsn` — is this value; LSNs increase with seq and position.

**Timestamps.** An entry's HLC is `hlc_base` with `physical_ns + hlc_delta`, `node_id` from the
base, and `logical` equal to `hlc_base.logical` plus the number of earlier entries in the set
with the same physical time; `ENTRY_FLAG_HLC_EXPLICIT` entries carry their own. A batch spanning
more than 2³² ns (≈ 4.3 s) is sealed before the delta would overflow.

**Checksum, compression and encryption are per jset.** The checksum (algorithm per
OPERATIONS-D4) covers the header with `checksum` zeroed, the stored entries, the AEAD tag if
any, and the padding. With `JSET_FLAG_COMPRESSED` the entries are compressed as one unit
(algorithm per BLOB-D3). With `JSET_FLAG_ENCRYPTED` they are then encrypted with the key
hierarchy's journal key (ENCRYPTION-DESIGN§1): nonce = 64-bit `seq` ‖ 32 zero bits, AAD = the
header with `checksum` zeroed. Writers compress, then encrypt; readers verify the checksum, then
decrypt and authenticate, then decompress, then read entries in order.

**Validity.** A jset is valid when its magic, version and checksum hold and its `seq` is the
successor of the previous jset's. Replay stops at the first invalid or out-of-sequence jset; a
device that dropped acknowledged writes surfaces as a shorter journal, never as wrong data.

```rust
// HybridTimestamp is the cluster-wide hybrid logical clock for total ordering
// without coordination (SPECIFICATION-DESIGN§10.4). Sort order: physical_ns → logical → node_id.
// Resolution is nanoseconds (matches every other *_ns field in the format —
// ObjectRecord.created_ns/modified_ns, SnapshotNode.created_ns,
// Superblock.creation_timestamp_ns, etc.). At ns granularity, two events colliding
// inside one tick is rare, so a 16-bit `logical` counter is plenty.
#[repr(C)]
struct HybridTimestamp {                     // 16 bytes
    physical_ns: i64,                        // [0..8]   monotonic wall-clock nanoseconds
    logical:     u16,                        // [8..10]  same-tick disambiguation
    node_id:     u16,                        // [10..12] originating node (NodeId)
    _pad:        u32,                        // [12..16] tail pad to multiple-of-8 (FORMAT-DESIGN§1.1)
}

```

Payloads are binary records (FORMAT-DESIGN§1.6), not CBOR: every op's shape is known from
`op_kind`, so a record carries no field names or type information, its bytes are fixed by its
field values alone (what AEAD authentication needs), and replay reads fields in place. The only
CBOR inside a payload is an attribute `Value` (INDEX-DESIGN§1.1). Jsets are versioned by
`JsetHeader.version`; the journal is replayed and trimmed, never migrated in place.

### 1.3 Op payloads

Each listing below is normative for its op's payload record, laid out by the FORMAT-DESIGN§1.6
record rules:

- Fixed-size fields in the listed order, each at its natural alignment, with explicit zero
  padding; the fixed head is padded to a multiple of 8 bytes. `BlockRef` is its 16-byte image
  (ENGINE-DESIGN§1.2); `[T; N]` is N consecutive `T`.
- A variable part follows the head, at most one per op: an array of fixed records (`[T; N]`
  with N not fixed by the type) is a `u32` count in the head and the records after it; a byte
  string (`bytes`, `String`, a packed key) is a `u32` length in the head and the bytes after
  it, zero-padded to a multiple of 8; `Value` is a `u32` length and its CBOR encoding.
  `Option<String>` uses length `u32::MAX` for `None`.
- `EntryHead.len` covers the head and the variable part (not the trailing padding); a reader
  rejects a payload whose length disagrees with its head.
- An extent checksum (`extent_sum`) is the OPERATIONS-D4 checksum of the extent's bytes as
  written; recovery uses it to verify data written in the same commit (JOURNAL-DESIGN§1.5).
- A `ReplicaSet` (72 bytes) is `replica_count: u8`, 7 bytes of padding and
  `replicas: [BlockRef; 4]`, slots `[replica_count..4)` zero: the copies of one extent or block
  written together (METADATA-DESIGN§2.1).

**Op kinds.** `EntryHead.op_kind` takes these values, numbered contiguously and grouped by
subsystem; a reader rejects any other value.

| Value | Op | Group |
|---|---|---|
| 0 | `Checkpoint` | commit |
| 1 | `CreateObject` | objects and terms |
| 2 | `DeleteObject` | objects and terms |
| 3 | `AddTag` | objects and terms |
| 4 | `RemoveTag` | objects and terms |
| 5 | `SetAttr` | objects and terms |
| 6 | `RemoveAttr` | objects and terms |
| 7 | `AddRelation` | objects and terms |
| 8 | `RemoveRelation` | objects and terms |
| 9 | `SetOrder` | collections |
| 10 | `RenumberSequence` | collections |
| 11 | `WriteBlob` | content |
| 12 | `WriteInline` | content |
| 13 | `ExtendObject` | content |
| 14 | `PackObjects` | content |
| 15 | `FinalizeObject` | content |
| 16 | `ChunkInsertBatch` | chunked content |
| 17 | `ChunkListAppend` | chunked content |
| 18 | `ChunkListReplace` | chunked content |
| 19 | `ChunkListShrink` | chunked content |
| 20 | `ReclaimTerms` | terms |
| 21 | `OntologyPut` | ontology |
| 22 | `OntologyDelete` | ontology |
| 23 | `SubscriptionPut` | subscriptions |
| 24 | `SubscriptionDelete` | subscriptions |
| 25 | `SubscriptionAck` | subscriptions |
| 26 | `BucketAlloc` | buckets |
| 27 | `BucketGenBump` | buckets |
| 28 | `BucketDiscard` | buckets |
| 29 | `BackpointerInsert` | backpointers |
| 30 | `BackpointerRemove` | backpointers |
| 31 | `SnapshotCreate` | snapshots |
| 32 | `SnapshotSwitch` | snapshots |
| 33 | `SnapshotDelete` | snapshots |
| 34 | `SnapshotUnlink` | snapshots |
| 35 | `SnapshotDepthUpdate` | snapshots |
| 36 | `ReconcileEnqueue` | reconcile |
| 37 | `ReconcileDequeue` | reconcile |
| 38 | `ReconcileMove` | reconcile |
| 39 | `ReconcileReplicate` | reconcile |
| 40 | `ReconcileDropReplica` | reconcile |
| 41 | `ReconcileScanStep` | reconcile |

Changes to pool administration trees have no ops; they commit by checkpoint (POOL-DESIGN§1.2).
Postings, pending subscription events and `live_sectors` have none either: replay derives them
from the ops that cause them.

```rust
CreateObject     : { oid: u64, created_ns: i64, state: u8 }    // raises the origin node's next_local;
                   //   state is Active (content appears whole at FinalizeObject or WriteBlob)
                   //   or Writing (a stream, readable up to its durable length, JOURNAL-DESIGN§1.8)
DeleteObject     : { oid: u64, lsn: u64 }
AddTag           : { oid: u64, tag: u32 }                     // a direct tag (INDEX-DESIGN§2.4)
RemoveTag        : { oid: u64, tag: u32 }
SetAttr          : { oid: u64, key: u32, value: Value }       // Value tagged-union (INDEX-DESIGN§1)
RemoveAttr       : { oid: u64, term_id: u64 }                   // the attribute term (INDEX-DESIGN§1.2)
AddRelation      : { oid: u64, predicate: u32, target: u64 }
RemoveRelation   : { oid: u64, predicate: u32, target: u64 }
WriteBlob        : { oid: u64, content_hash: [u8;32], size: u64, extent_sum: u64,
                     replicas: ReplicaSet }
                   // One-extent objects only: sets the record's Extent locator with every
                   //   replica written (METADATA-DESIGN§1.2); a backpointer per replica follows
                   //   (METADATA-DESIGN§1.2). Larger non-chunked objects are written
                   //   through ExtendObject into an extent list (METADATA-DESIGN§1.3).
                   //   Chunked objects (INDEX-DESIGN§4.3) use the
                   // ChunkInsertBatch / ChunkListAppend / FinalizeObject flow
                   // described in JOURNAL-DESIGN§1.3.1.

// FastCDC chunked objects (INDEX-DESIGN§4.3, JOURNAL-DESIGN§1.3.1)
ChunkInsertBatch    : { chunks: [(chunk_hash: [u8;32], extent: BlockRef, length: u32, extent_sum: u64); N] }
                   // N ≤ 1 000, chosen by the producer; one entry fits a jset (JOURNAL-DESIGN§1.2);
                   //   replay inserts each (hash → extent) into ChunkIndex (or bumps
                   //   ref_count if the hash already exists). Idempotent under LSN.
ChunkListAppend     : { oid: u64, start_offset: u64, chunks: [(chunk_hash: [u8;32], length: u32); N] }
                   // Puts N ChunkList keys (INDEX-DESIGN§4.3) for oid, the first covering
                   //   [start_offset, start_offset + length₀), each next one following.
                   //   N ≤ 1 000; replay adds one ChunkIndex reference per chunk.
ChunkListReplace    : { oid: u64, start_offset: u64, end_offset: u64,
                        chunks: [(chunk_hash: [u8;32], length: u32); N] }
                   // In-place patch: the keys covering [start_offset, end_offset) — chunk
                   //   boundaries, as FastCDC re-chunks whole chunks — are replaced by N
                   //   keys whose lengths sum to the new range. Replay removes the old keys
                   //   written at the current snapshot and whiteouts those of an ancestor
                   //   snapshot; ref_count rises for each new key and falls only for each
                   //   key physically removed (SNAPSHOTS-DESIGN§1.6).
ChunkListShrink     : { oid: u64, new_length: u64 }
                   // Truncate oid's chunks to the first new_length plaintext bytes, which
                   //   fall on a chunk boundary. Replay removes or whiteouts the later keys
                   //   as ChunkListReplace does, with the same ref_count rule.
FinalizeObject      : { oid: u64, content_hash: [u8;32], total_length: u64,
                        chunk_params: ChunkParamsRecord }
                   // Closes a write: sets content_hash and length = total_length; for a
                   //   stream, moves Writing → Active. chunk_params is all zero for an
                   //   object that is not chunked; otherwise it sets content_kind = ChunkList
                   //   and the locator's params (METADATA-DESIGN§1.2). A re-chunk ends with
                   //   one to record the new params (INDEX-DESIGN§4.3). An Active object
                   //   written through the chunked flow shows its content only after this op.
                   //   Converting between chunked and not chunked ends with one that
                   //   switches content_kind and range-deletes the old representation's keys
                   //   (oid, *) at the current snapshot (INDEX-DESIGN§4.3).

// Bucket lifecycle (ALLOCATION-DESIGN§1)
BucketAlloc      : { disk_id: u16, bucket_no: u32, kind: u8, generation: u32 }  // bucket opened
BucketGenBump    : { disk_id: u16, bucket_no: u32, new_generation: u32 }  // bucket freed for reuse
// live_sectors has no op: replay derives it from the entries that add and drop references
//   (ALLOCATION-DESIGN§1.2).
BucketDiscard    : { disk_id: u16, bucket_no: u32 }                       // TRIM issued

// Backpointers (METADATA-DESIGN§2.2)
BackpointerInsert: { key: BackpointerKey, value: BackpointerValue }
BackpointerRemove: { key: BackpointerKey }                                // explicit removal
// Implicit removal: a BucketGenBump invalidates all of that bucket's backpointers lazily
// (stale entries detected by gen mismatch on the next scan, no per-bp WAL op needed).


// Snapshot lifecycle (SNAPSHOTS-DESIGN§1)
SnapshotCreate   : { new_id: u32, parent_id: u32, current_replacement: u32, label: Option<String> }
                   // label capped at 256 bytes
SnapshotSwitch   : { target: u32, new_head: u32 }
                   // Switches the head to live snapshot `target` (SNAPSHOTS-DESIGN§1.8): links
                   //   new_head as target's first child, retags the current pointer to it,
                   //   starts the object-table re-head scan, and marks every subscription
                   //   Overflowed with switched_to = target, dropping its pending events.
SnapshotDelete   : { id: u32 }                                            // marks for async cleanup
// Note: snapshot deletion does NOT emit per-key WAL ops. The actual
// re-tag / drop happens via a reconcile-driven scan whose progress is
// checkpointed via ReconcileScanStep (SNAPSHOTS-DESIGN§1.5, RECONCILE-DESIGN§1.3). The scan is
// idempotent: a re-tagged leaf no longer matches the deleted snapshot
// id, so resuming from a stale cursor after crash is safe.

// Snapshot-tree topology mutations (SNAPSHOTS-DESIGN§1.5 SnapshotTreeReorg pass)
SnapshotUnlink     : { id: u32, parent: u32, prev_sibling: u32 }
                     // Remove `id` from the snapshots btree. If prev_sibling != 0,
                     //   set prev_sibling.next_sibling = id.next_sibling; otherwise set
                     //   parent.first_child = id.next_sibling. Emitted at the end of
                     //   a snapshot cleanup pass (SNAPSHOTS-DESIGN§1.5) once all keys at `id` have
                     //   been re-tagged or dropped.
SnapshotDepthUpdate: { id: u32, new_depth: u16, new_skiplist: [u32; 3], new_ancestor_bitmap: u128 }
                     // Recompute ancestry data for a snapshot node whose effective
                     //   parent changed because an ancestor was unlinked. Emitted in
                     //   batches by the SnapshotTreeReorg pass — one per affected
                     //   descendant. Replay updates the snapshot's entry in the snapshots btree.
// The reorg decomposes into N × SnapshotDepthUpdate followed by one SnapshotUnlink per
// removed snapshot. Each op is small (max ~50 B), and replay is local to the snapshots btree.

// Reconcile (RECONCILE-DESIGN§1)
ReconcileEnqueue : { key: WorkKey, value: WorkValue, pending: bool }   // into ReconcileWork, or
                   //   ReconcilePending when `pending` (RECONCILE-DESIGN§1.2)
ReconcileDequeue : { key: WorkKey, pending: bool }                     // completion or cancel
ReconcileMove    : { from_loc: BlockRef, to_loc: BlockRef, owner_key: bytes }
                   // atomic location-update for move-path completion;
                   //   owner_key capped at 16 bytes (METADATA-DESIGN§2.2)
ReconcileReplicate: { existing: BlockRef, new_loc: BlockRef, owner_key: bytes }
                   // A new copy of the data at `existing` was written at new_loc (replica
                   //   repair, a raised Replicate rule): adds it to the owner's replicas and
                   //   inserts its backpointer (RECONCILE-DESIGN§1.5)
ReconcileDropReplica: { loc: BlockRef, owner_key: bytes }
                   // Removes one replica from its owner and its backpointer (over-replicated,
                   //   or a copy on a draining disk once enough others exist); never the last
ReconcileScanStep: { scan_id: u64, record: ScanRecord }  // a scan started, advanced or finished
                   //   (empty record); emitted at checkpoints only (RECONCILE-DESIGN§1.3);
                   //   cursor_key capped at 256 bytes

Checkpoint       : { gc_reserve_buckets: u32, roots: RootList }   // RootListHead + RootEntry × N
                   //   (ENGINE-DESIGN§1.2)
                   // Commits a root (JOURNAL-DESIGN§1.5): every tree flushed up to this
                   //   entry's LSN is reachable from its root list. Recovery starts from the newest
                   //   valid Checkpoint and replays the entries after it.

// Data paths (JOURNAL-DESIGN§1.8)
WriteInline      : { oid: u64, content_hash: [u8;32], data: bytes }
                   // A whole object of at most INLINE_WRITE_MAX bytes, carried in the
                   //   journal itself. A completed write of at most 40 bytes sets the
                   //   record's Inline locator (METADATA-DESIGN§1.2) and is never packed;
                   //   a larger one is packed into a small-object block by PackObjects.
PackObjects      : { block: ReplicaSet, slots: [(oid: u64, slot: [u8; 8]); N] }
                   // Small-object block written with these objects' bytes: sets each
                   //   object's Packed locator to its slot (slot address layout per BLOB-D4)
                   //   and inserts the block's PackedBlock backpointer per replica; the
                   //   WriteInline entries it packs no longer pin the journal.
ExtendObject     : { oid: u64, new_length: u64, extents: [(length: u32, _pad: u32,
                     extent_sum: u64, replicas: ReplicaSet); N] }
                   // Appends extents to an object in the Writing state and makes its first
                   //   new_length bytes durable. The first extent sets an Extent locator;
                   //   from the second on, replay puts one extent-tree key per extent
                   //   (METADATA-DESIGN§1.3), the Extent locator's extent becoming the first key.

// Term maintenance (INDEX-DESIGN§1.2)
ReclaimTerms     : { term_ids: [u64; N] }
                   // Removes each listed term's dictionary entries and term-table record.
                   //   The TermId counter is untouched (ENGINE-DESIGN§1.2).

// Ontology (ONTOLOGY-DESIGN§1)
OntologyPut      : { record_kind: u8, a: u64, b: u64, record: bytes }
                   // Inserts or replaces one ontology record (TagRecord, RuleRecord or
                   //   ModuleRecord, positional CBOR). An active rule or constraint record
                   //   advances rules_epoch and its postings follow in the same commit; one in
                   //   state installing or removing starts a resumable rework instead
                   //   (INDEX-DESIGN§2.4). Replay raises the Tag or Rule id counter past
                   //   the record's id.
OntologyDelete   : { record_kind: u8, a: u64, b: u64 }
                   // Removes one ontology record; the id counters are untouched.

// Subscriptions (SUBSCRIPTIONS-DESIGN§1)
SubscriptionPut  : { id: u64, record: bytes }
                   // Inserts or replaces one SubscriptionRecord (positional CBOR); a new id
                   //   seeds its membership from the current query result.
SubscriptionDelete: { id: u64 }
                   // Removes the record, its membership and its pending events.
SubscriptionAck  : { id: u64, lsn: u64, cursor_ns: i64 }
                   // Removes the pending events up to lsn, applies them to the stored
                   //   membership and sets the cursor (SUBSCRIPTIONS-DESIGN "Delivery").

// Ordered and ranked collections (INDEX-DESIGN§3.3)
SetOrder         : { oid: u64, term_id: u64, order: u64 }
                   // Sets the object's order slot for a collection term it carries and moves
                   //   its Sequences key. The shape of `order` (sparse u64 position, or a
                   //   variable-length position) is CLUSTER-D6.
RenumberSequence : { term_id: u64 }
                   // Respaces the term's positions; deterministic, so replay recomputes every
                   //   member's new position and order slot from the tree alone.
```

Replay applies entries strictly in LSN order. Each in-memory mutation is idempotent under
`(lsn ≤ structure.lsn)` shortcutting, so replay is safe across crashes mid-replay.

#### 1.3.1 Chunked-object write sequence

Chunked objects (SPECIFICATION-DESIGN§9.6, INDEX-DESIGN§4.3) are the only logical mutation that splits
into a multi-op flow. The `WriteBlob` op covers atomic non-chunked writes; chunked workloads
need finer granularity because a single object can be gigabytes and per-fsync durability
during long transfers matters.

**Bulk write (e.g. a 32 GiB VM image, no dedup).** Before chunking begins, the producer
resolves the per-object chunking parameters per SPECIFICATION-DESIGN§3.5 (ontology effective policy →
matching `PlacementRule::Chunk` → superblock default). Then:

1. Splits plaintext using the resolved algorithm/params (e.g. FastCDC at the resolved
   `avg_size`). For each chunk: probes `ChunkIndex`; on a miss writes the chunk extent to a
   fresh blob bucket and adds `(hash, extent, length)` to a pending `ChunkInsertBatch`; either
   way adds `(hash, length)` to a pending `ChunkListAppend`.
2. When the pending entries reach the producer's chosen size cap (at most 1 000 chunks per
   entry), or on fsync, emits the `ChunkInsertBatch` and then the `ChunkListAppend`
   `{ oid, start_offset, chunks }`; the ChunkList keys are ordinary tree puts, flushed with
   every other tree at checkpoint.
3. At end-of-write, emits `FinalizeObject { oid, content_hash, total_length, chunk_params }`;
   the locator's `ChunkParamsRecord` is the durable record of the parameters this object was
   written with.

**Chunked streams** (an object created `Writing`) follow the same steps, except that the first
`ChunkListAppend` sets `content_kind = ChunkList` and each one advances the durable `length` to
its end, so the prefix is readable before `FinalizeObject` and survives a crash (JOURNAL-DESIGN§1.8).

**Small in-place edit** (e.g. 4 KiB write at offset 1 GiB into a VM disk → FastCDC re-chunks
the affected window, replacing 1–3 chunks):

1. New chunks → `ChunkInsertBatch { chunks: [1..3] }`.
2. `ChunkListReplace { oid, start_offset, end_offset, chunks: [1..3] }` over the replaced
   chunks' range.

No `FinalizeObject` is needed for in-place edits — the object is already published; the
replace op modifies its existing ChunkList, and a length change follows from the replaced
range. Any edit also changes the object's `content_hash`; how that hash is maintained without
re-reading the whole object — likely a Merkle-style hash over the chunk hashes — is BLOB-D5.

**Sync receive / resumable transfer.** Identical to bulk-write but driven by stream arrival
order. After crash, the receiver reads the object's last ChunkList key and resumes from its
`end_offset` — the replayed `ChunkListAppend` entries *are* the durable progress record.

**Truncate / delete.** `ChunkListShrink { oid, new_length: 0 }` drops every key;
`DeleteObject` then removes the object record. ChunkIndex `ref_count` decrements happen during
shrink-replay; chunks reaching `ref_count = 0` are freed.

**Crash semantics.** Each step is independently durable: chunks written to fresh buckets but
not yet referenced from any `ChunkInsertBatch` get reclaimed by bucket-gen GC. An `Active`
object written through the chunked flow that crashed before `FinalizeObject` shows no content —
its locator was never set — so no partial state is observable; a chunked stream keeps its
durable prefix.

**Throughput model — 32 GiB bulk write, no dedup, across the FastCDC parameter space.**

FastCDC's avg-chunk-size knob spans ~3 orders of magnitude in real deployments (SPECIFICATION-DESIGN§9.6):
4 KiB for fine-grained edits at the small end, up to 8 MiB for resumability-only large
transfers at the other. The five-regime table below shows how the design scales:

| Avg chunk | Typical workload                   | Chunks      | `ChunkInsertBatch` and `ChunkListAppend` entries (≤ 1 000 chunks each, one of each per batch) | Journal bytes (~100 B per chunk: 64 in the batch, 36 in the append) | Journal append rate at 530 MiB/s plaintext |
| --------- | ---------------------------------- | ----------- | --------------------------------------------------------------------- | ------------------------------- | ------------------------------------------ |
| **4 KiB** | small extreme — fine-grained edits | 8 388 608   | 8 389 of each                                                         | ~800 MiB (2.4 % of plaintext)   | ~12.9 MiB/s                                |
| 16 KiB    | VM disks, databases                | 2 097 152   | 2 098 of each                                                         | ~200 MiB (0.61 %)               | ~3.2 MiB/s                                 |
| 256 KiB   | generic FastCDC default            | 131 072     | 132 of each                                                           | ~12.5 MiB (0.038 %)             | ~207 KiB/s                                 |
| 1 MiB     | backup-tool default (restic-class) | 32 768      | 33 of each                                                            | ~3.1 MiB (0.0095 %)             | ~52 KiB/s                                  |
| **8 MiB** | large extreme — pure resumability  | 4 096       | 5 of each                                                             | ~400 KiB (0.0012 %)             | ~6.5 KiB/s                                 |

The spread is roughly **2 000×** across regimes on every axis — the same op set covers all
of it, only the constants change.

**Cumulative ≠ simultaneous.** Steady-state journal fill is bounded by *reclaim latency*, not
by total traffic over the operation: buckets are freed as soon as the trim point passes them
(JOURNAL-DESIGN§1.4). Reclaim's job is flushing ChunkIndex leaves (256 KiB nodes holding about
4 200 chunks each) and ChunkList leaves (about 5 400 chunks each, INDEX-DESIGN§4.3); the
leaf-fill rate it must sustain is `chunks_per_second × (1/4 200 + 1/5 400)` ≈
`chunks_per_second / 2 360` leaves/s. At 530 MiB/s plaintext:

- **4 KiB regime:** ~135 K chunks/s ÷ 2 360 = ~57 leaves/s = ~14.4 MiB/s of leaf writes.
  Comfortably within NVMe write bandwidth (~1–3 GiB/s typical) but no longer trivial; this
  is where reclaim genuinely has to keep up. Cumulative journal traffic exceeds a 64 MiB
  journal by ~12×, so its buckets cycle continuously.
- **16 KiB regime (canonical VM / DB workload):** ~14 leaves/s ≈ 3.6 MiB/s of leaf writes.
  A 64 MiB journal cycles ~3× over the 60-second write. JOURNAL-DESIGN§1.4's *idle* /
  *background-flush* bands cover the producer comfortably.
- **256 KiB and above:** journal traffic is in the noise; reclaim is essentially never the
  bottleneck.

The design therefore scales from "fine-grained dedup workload that genuinely stresses
reclaim" to "barely-noticed bookkeeping" without changing op shapes. The producer's choice
of FastCDC parameters trades dedup granularity (small chunks → more shared chunks across
edits) against per-chunk overhead (small chunks → more journal bytes, more ChunkIndex and
ChunkList leaves).

**HDD-only pools.** A pool whose only writable disk is rotational clamps the producer to
whatever reclaim sustains there. At the 4 KiB extreme this would be a real bottleneck (HDD
write throughput is far below the ~14 MiB/s leaf-write demand); at 1 MiB or above it is
unnoticeable. Chunked workloads typically imply hot-tier (NVMe/SSD) presence anyway, so
this is a corner case.

**Per-object footprint** (orthogonal to WAL but worth noting at the small extreme): a 32 GiB
object's `ChunkList` keys cost about 45 B per chunk. At 4 KiB chunks that's ~360 MiB per
object (1.1 % of plaintext); at 8 MiB chunks it's ~180 KiB, sharing leaves with other objects.
ChunkIndex footprint scales similarly. The very-small-chunk regime is therefore
viable but pays for itself in dedup hit-rate, not in raw efficiency — fine for VM-disk
workloads with high common-chunk ratios across snapshots, less so for one-shot writes of
unique data.

### 1.4 Journal pins and deferred btree flushes

Each in-memory node with entries not yet written to its region — its `LoadedNode`
(BTREE-DESIGN§1.3) — holds a **journal pin** over those pending entries:

```rust
struct JournalPin {                          // in-memory only, one per LoadedNode
    lsn_min: u64,                            // oldest journal entry pending in this node
    lsn_max: u64,                            // newest journal entry pending in this node
    count: u32,                              // entries pending
}
```

The node's newest run records the newest merged LSN in its `journal_seq` (BTREE-DESIGN§1.2).

**Trim point.** Each jset records `last_seq`, the oldest jset replay still needs: the seq of the
newest `Checkpoint`, or of the lowest live pin's `lsn_min` if that is older, or of the oldest
retained history (JOURNAL-DESIGN§1.6) if that is older still. A journal bucket whose jsets are all
older than the newest durable `last_seq` is free; it is discarded and its slot reused for new
jsets. No header or cursor is written to trim. Implementations that checkpoint every dirty node
at each `Checkpoint` (the simplest correct policy, suited to small working sets) never hold pins
across a checkpoint, so their trim point is the last `Checkpoint`.

Reclaim scheduling follows journal fill — the share of journal buckets not free:

| Fill level | Reclaim behaviour                                                           |
| ---------- | --------------------------------------------------------------------------- |
| < 25 %     | Idle. No flushes scheduled.                                                 |
| 25 – 60 %  | Background flushes at low priority — flush nodes with highest pin pressure. |
| 60 – 85 %  | Aggressive: rank pinned nodes by `(lsn_max − lsn_min)` × `count` and flush the top fraction. |
| > 85 %     | New mutations stall until reclaim catches up.                               |

A flush appends one run (or page run) holding the node's pending entries to its region
(BTREE-DESIGN§1.2, BTREE-DESIGN§1.7), with `journal_seq = lsn_max`, and clears the pin; a parent
changes only when compaction moves a node to a fresh region (BTREE-DESIGN§1.4). The next
`Checkpoint` (JOURNAL-DESIGN§1.5) commits the new root list atomically. "Stall" means a mutation waits for, or
performs inline, the reclaim the embedder's driver has not yet run; the engine owns no thread.

**Memory reclaim.** A node may be evicted from the in-memory btree cache only after its pending
journal entries have been written as a run. Clean nodes (no pending entries) and their cached
blocks can be discarded immediately.

**Read path.** Lookups consult the node's pending entries first, then its runs. Cold reads load
the node's header and run index blocks from disk, then the engine **replays** journal entries
newer than the newest run's `journal_seq` whose key range intersects the node. This replay
is bounded by journal size (~800 K entries worst-case), but the reclaim thresholds keep typical
lag under ~10 K entries per node.

**Locking model.** Two independent lock domains govern btree state. The mutation pipeline in
ENGINE-DESIGN§2 and the flush thread cooperate through them without a shared coarse mutex:

- **Per-tree write mutex** (mutation path). One mutex per snapshot-aware btree, plus one for
  the open jset. Writers reserve space in the open jset, copy their entry in and take its LSN
  under that lock, then update the in-memory state — the steps that must be atomic across
  concurrent writers. It is released before any per-node bookkeeping or I/O.
- **Per-node rwlock** (flush path). One rwlock per `LoadedNode`. A flush writes a new run
  after the node's existing blocks and never touches them, so it holds only the *shared* lock
  while it builds the run; the *exclusive* lock is taken only to move the flushed entries out
  of the pending set and clear the pin, and, at compaction, to swap the node to its fresh
  region. The mutation path also takes the exclusive lock briefly, after releasing the
  per-tree mutex, to add its entry to the pin of each affected node; this is uncontended in
  steady state.

The two domains are independent: a flush of node A runs concurrently with a mutation hitting
node B, and a mutation on node A only contends with a flush of node A at the brief exclusive
moment. **Readers** see the in-memory state as of the last completed mutation (ENGINE-DESIGN§2)
and are never blocked on the per-tree mutex (writers release it before any disk I/O). On a
single-threaded embedder the locks are uncontended by construction.

### 1.5 Commits, checkpoints and recovery

**Durability levels.** Every mutating call names one:

- `Sync` — the call returns once the jset holding its entries, and every earlier jset, is
  durable.
- `Buffered { max_delay }` — the call returns at once; its entries become durable no later than
  `max_delay` after it, through the embedder's driver (JOURNAL-DESIGN§1.4 "Stall", JOURNAL4).
  Streams, bulk imports and caches use this.

**Group commit.** Entries accumulate in the open jset. A commit seals it when a `Sync` caller
waits and no jset is in flight (idle: lowest latency), when it reaches 256 KiB, when a
`Buffered` deadline arrives, or when its HLC span would overflow (JOURNAL-DESIGN§1.2). While a
jset is in flight, newly arriving commits join the next one, so set size grows with load.
Several jsets may be in flight at once; a commit is acknowledged when every jset up to its own
is durable.

**Write barrier, one flush per commit.** A jset is written with FUA where the device offers it,
otherwise followed by one cache flush. Blob data written in the same commit is not flushed
separately first: entries that reference new extents carry each extent's `extent_sum`
(JOURNAL-DESIGN§1.3), and the commit issues its data writes, its jset write and then one barrier.
If power fails between the two, recovery detects it (below). On devices that offer neither FUA
nor a reliable flush, durability is whatever the device provides; sequence numbers and
checksums still turn lost writes into a shorter journal, never corrupt state.

**Checkpoints** are invoked when:

1. Btree topology changes (root split / merge / new tree depth).
2. Enough has been flushed that committing a root meaningfully advances the trim point.
   Default: whichever comes first — **30 s** of mutation activity, or **256 MiB** of flushed
   tree bytes since the last checkpoint.
3. Journal fill reaches the aggressive band (JOURNAL-DESIGN§1.4).
4. A snapshot is created, so its `RootPointer` materialises a coherent state.

A checkpoint flushes the dirty trees, then commits a jset containing a `Checkpoint` entry with
the new root list (ENGINE-DESIGN§1.2). It writes nothing else: the superblock's checkpoint hint is
rewritten only on clean unmount and when the journal bucket list changes, and lets a clean mount
read one jset instead of scanning the journal.

**Recovery:**

1. Read the first jset of every journal bucket (JOURNAL-DESIGN§1.1); order buckets by their first
   `seq`; walk forward from the newest durable `last_seq` through valid, consecutive jsets.
2. The committed root is the newest `Checkpoint` entry: the one in the jset the superblock's
   hint names, or a newer one found by the scan.
3. Verify every extent referenced by entries after that root, in the last jsets written before
   the tail (those whose commit may have been cut between data and barrier), against its
   `extent_sum`. An entry whose extent fails is dropped together with every later entry of its
   commit; the objects it touched stay at their previous state.
4. Replay the remaining entries after the root's LSN, in LSN order. Each in-memory mutation is
   idempotent under `lsn ≤ structure.lsn`, so a crash during replay is safe.

Recovery reads one jset per bucket plus the unflushed tail, and verifies data proportional to
that tail, never to the pool.

### 1.6 Retention

Jsets are self-describing, so history is kept by not freeing journal buckets yet. A pool may
retain journal history past the trim point (JOURNAL-DESIGN§1.4) within a cap on retained bytes
and age set per pool, by default none, for tools that read mutation history. Nothing in the
engine depends on retained history: subscriptions keep their own pending events
(SUBSCRIPTIONS-DESIGN "Delivery") and never hold the trim point, and cluster sync reads snapshot
diffs, never the journal (CLUSTER-DESIGN).

**Why none by default.** With nothing in the engine reading history, retained buckets would only
occupy journal space on the fastest tier; with none retained, a journal bucket is reused as soon
as checkpoints and pins allow, which keeps the journal small and, on an SD card, avoids keeping
and rewriting data no one reads. The two places that mention retained history work without it:

- term reclamation (INDEX-DESIGN§1.2) frees only terms older than the oldest retained entry;
  with no retention that point is the trim point;
- mutation history for tools (debugging, audit) exists only when a pool sets a cap.

A non-zero default (a few MiB, or a day) would let such tools read recent mutations at the cost
of holding journal buckets longer.

### 1.7 Mirroring

When several hot-tier disks carry journal buckets, each commit writes the same jset — same
`seq`, same bytes — to the current journal bucket of each, and is acknowledged once the
configured number of copies is durable. Recovery merges the disks' journals by `seq`, taking any
copy that validates. Whether and how the journal mirrors by default is JOURNAL-D1.

### 1.8 Data paths

Object bytes do not go through the journal, with one exception:

| Write | Path |
|---|---|
| Tiny: whole object ≤ `INLINE_WRITE_MAX` (2 KiB by default, set per pool) | `WriteInline` carries the bytes; one jset write makes the object durable. An object of at most 40 bytes is stored in its record's `Inline` locator; larger ones are later packed into small-object blocks, many per block (`PackObjects`), so a 200-byte file does not occupy a 4 KiB blob sector ([BLOB-DESIGN](blob-design.md)). |
| Small to large | Bytes go to blob extents at the stream's write point (ALLOCATION-DESIGN§1.5); `WriteBlob` or the chunked flow (JOURNAL-DESIGN§1.3.1) records them, with `extent_sum`s. |
| Streaming (camera recording, logs, downloads) | The object is created in the `Writing` state (METADATA-DESIGN§1.1); bytes are appended to extents taken whole bucket by bucket from a per-stream write point, so the stream is physically sequential; `ExtendObject` (or `ChunkListAppend` for chunked streams) makes each prefix durable, typically under `Buffered` durability once per second or on request. Closing the object (`FinalizeObject`) sets its final length and content hash, computed incrementally while streaming, and moves it to `Active`. |

**A stream survives an interrupted write.** Power loss, a crash or a writer that never closes
leaves the object `Writing` at its last journaled length, with every byte up to it intact and
readable; only bytes written beyond it are reclaimed. Nothing at mount discards or truncates a
`Writing` object: a video recording cut off by a dead battery reads back up to its last durable
second. A writer may reopen the stream and keep appending from its durable length, or close it;
closing a stream whose incremental hash state was lost reads the durable prefix once to compute
the content hash.

---

