# Journal (WAL) — design

Scope: `crates/wal`, the WAL projection in `crates/engine/src/wal_proj.rs`, checkpoint cadence in `crates/engine`.
Governing spec: JOURNAL-DESIGN§1 (§1.1 journal location, §1.2 journal sets, §1.3 ops, §1.4 pins and reclaim, §1.5 commits and checkpoints, §1.6 retention, §1.7 mirroring, §1.8 data paths), SPECIFICATION-DESIGN§6.4.

Plan: [journal-implementation-plan.md](journal-implementation-plan.md) · Open decisions: [journal-decisions.md](journal-decisions.md) · Completed: [journal-completed.md](journal-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

The journal is the durability point for every mutation on any medium — SD card, SSD, NVMe, HDD — and for every write shape from tiny files to long streams; B+ tree nodes and object data are written behind or beside it, and its retained history is the oplog.

## Selected design

- **Journal buckets, write-once journal sets.** The journal is an ordered list of allocator buckets named in the superblock. Each commit writes one new *journal set* (jset) — a header and a batch of entries — that is never rewritten, so a torn write loses only the set not yet acknowledged.
- **Compact entries.** One 64-byte header, one checksum, one compression unit and one AEAD tag per jset; 8 bytes of header per entry. An `AddTag` costs 24 bytes.
- **The root travels in the journal.** A checkpoint is a `Checkpoint` entry carrying the new `RootPointer`; the superblock's root slots are a mount hint, rewritten only on clean unmount and when the journal's bucket list changes.
- **Adaptive group commit.** Idle commits seal and write at once; under load, commits join the next set and sets grow toward 256 KiB. Sets are pipelined and acknowledged in sequence. FUA where the device offers it, a cache flush otherwise; one flush per commit, with data extents verified by checksum on recovery instead of a second flush.
- **Caller-chosen durability.** `Sync` returns once durable; `Buffered { max_delay }` becomes durable within a bound.
- **Data beside the journal.** File data goes to blob extents; only tiny writes ride inside the journal, to be packed later. Streams are append-only objects made durable by periodic `ExtendObject` entries.
- **Retention is the oplog.** Journal buckets are kept past the trim point while subscriptions need history, within a cap.
- **Journal pins.** Each `LoadedNode` holds one `JournalPin { lsn_min, lsn_max, count }` over its pending journal; the trim point is the lowest live pin or the last checkpoint. `JournalPin` lives in `mimisbrunnr-storage`, beside `LoadedNode`.
- **The engine starts no thread.** It exposes `commit`, `checkpoint` and a non-blocking query of what is due; the embedder drives them — a `std`-gated helper thread on the host, the filesystem server's own thread or event loop on Vesper.
- **`FormatPromote`** (`WalOpKind::FormatPromote = 30`) journals a sorted-run format widening before the widened run is written; replay re-applies it before decoding later runs.

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
(< 2¹⁶). Every LSN in the format — `BlockHeader.lsn`, `RootPointer.lsn`, subscription cursors,
`last_modify_lsn` — is this value; LSNs increase with seq and position.

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
#[repr(C, packed)]
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

```rust
CreateObject     : { oid: u64, generation: u32, created_ns: i64 }
DeleteObject     : { oid: u64, lsn: u64 }
AddTag           : { oid: u64, tag: u32 }                     // a direct tag (INDEX-DESIGN§2.4)
RemoveTag        : { oid: u64, tag: u32 }
SetAttr          : { oid: u64, key: u32, value: Value }       // Value tagged-union (INDEX-DESIGN§1)
RemoveAttr       : { oid: u64, term_id: u64 }                   // the attribute term (INDEX-DESIGN§1.2)
AddRelation      : { oid: u64, predicate: u32, target: u64 }
RemoveRelation   : { oid: u64, predicate: u32, target: u64 }
WriteBlob        : { oid: u64, content_hash: [u8;32], extent: BlockRef, size: u64, extent_sum: u64 }
                   // Non-chunked objects only. Chunked objects (INDEX-DESIGN§4.3) use the
                   // ChunkInsertBatch / ChunkListAppend / ChunkObjectFinalize flow
                   // described in JOURNAL-DESIGN§1.3.1.

// FastCDC chunked objects (INDEX-DESIGN§4.3, JOURNAL-DESIGN§1.3.1)
ChunkInsertBatch    : { chunks: [(chunk_hash: [u8;32], extent: BlockRef, length: u32, extent_sum: u64); N] }
                   // N ≤ 1 000, chosen by the producer; one entry fits a jset (JOURNAL-DESIGN§1.2);
                   //   replay inserts each (hash → extent) into ChunkIndex (or bumps
                   //   ref_count if the hash already exists). Idempotent under LSN.
ChunkListAppend     : { oid: u64, position_start: u64, region: BlockRef, count: u32 }
                   // The region — a freshly written INDEX-DESIGN§4.3 ChunkList region — already
                   //   holds `count` chunk hashes; the op records its placement in
                   //   oid's ChunkList chain at logical positions
                   //   [position_start, position_start + count). Hashes are not
                   //   inlined; replay reads them from the region.
ChunkListReplace    : { oid: u64, position: u64, new_hashes: [[u8;32]; N] }
                   // In-place patch: replace N entries starting at `position`.
                   //   N capped by the sector budget (~125 hashes at 32 B each).
                   //   Replay rewrites the affected ChunkList region(s) and adjusts
                   //   ChunkIndex.ref_count (decrement old, increment new).
ChunkListShrink     : { oid: u64, new_length: u64 }
                   // Truncate oid's ChunkList to new_length entries. Replay walks the
                   //   tail and decrements ref_count for each dropped chunk; fully-
                   //   emptied regions are freed via bucket-gen bump.
ChunkObjectFinalize : { oid: u64, content_hash: [u8;32], total_length: u64,
                        list_head: BlockRef }
                   // Publishes a chunked object: sets ObjectLocation.flags |=
                   //   LOCATION_FLAG_CHUNKED, replicas[0] = list_head;
                   //   ObjectRecord.content_hash = whole-plaintext hash;
                   //   ObjectRecord.blob_length = total_length. Readers see a fully
                   //   assembled chunked object only after this op replays.

// Bucket lifecycle (ALLOCATION-DESIGN§1)
BucketAlloc      : { disk_id: u16, bucket_no: u32, data_type: u8, generation: u32 }
BucketWrite      : { disk_id: u16, bucket_no: u32, sectors_added: u16 }   // dirty_sectors delta
BucketGenBump    : { disk_id: u16, bucket_no: u32, new_generation: u32 }  // bucket reused
BucketDiscard    : { disk_id: u16, bucket_no: u32 }                       // TRIM issued

// Backpointers (METADATA-DESIGN§2.2)
BackpointerInsert: { key: BackpointerKey, value: BackpointerValue }
BackpointerRemove: { key: BackpointerKey }                                // explicit removal
// Implicit removal: a BucketGenBump invalidates all of that bucket's backpointers lazily
// (stale entries detected by gen mismatch on the next scan, no per-bp WAL op needed).

// Postings need no ops of their own: AddTag / SetAttr / AddRelation and their removals
// change term postings, which live in ordinary B+ tree entries (INDEX-DESIGN§3.1) and
// replay like any other entry. Op kinds 20 and 21 are unassigned.

// Snapshot lifecycle (SNAPSHOTS-DESIGN§1)
SnapshotCreate   : { new_id: u32, parent_id: u32, current_replacement: u32, label: Option<String> }
                   // label capped at 256 bytes
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
                     //   a SnapshotCleanup scan (SNAPSHOTS-DESIGN§1.5) once all keys at `id` have
                     //   been re-tagged or dropped.
SnapshotDepthUpdate: { id: u32, new_depth: u16, new_skiplist: [u32; 3], new_ancestor_bitmap: u128 }
                     // Recompute ancestry data for a snapshot node whose effective
                     //   parent changed because an ancestor was unlinked. Emitted in
                     //   batches by the SnapshotTreeReorg pass — one per affected
                     //   descendant. Replay updates the snapshots btree leaf in place.
// The reorg decomposes into N × SnapshotDepthUpdate followed by one SnapshotUnlink per
// removed snapshot. Each op fits comfortably in one sector (max ~50 B), and replay is
// local to the snapshots btree.

// Reconcile (RECONCILE-DESIGN§1)
ReconcileEnqueue : { work: WorkItem, high_prio: bool, phys_index: bool }
ReconcileDequeue : { target_kind: u8, owner_key: bytes, work_kind: u8 }   // completion or cancel;
                   //   owner_key capped at 16 bytes (METADATA-DESIGN§2.2)
ReconcileMove    : { from_loc: BlockRef, to_loc: BlockRef, owner_key: bytes }
                   // atomic location-update for move-path completion;
                   //   owner_key capped at 16 bytes (METADATA-DESIGN§2.2)
ReconcileScanStep: { scan_id: u64, btree: BtreeKind, cursor_key: bytes }  // resumable progress;
                   //   cursor_key capped at 256 bytes

// Sorted-run format mutations (BTREE-DESIGN§1.6)
FormatPromote    : { node_ref: BlockRef, sorted_run_seq: u32, new_format: SortedRunKeyFormat }

Checkpoint       : { new_root: RootPointer, gc_reserve_buckets: u32 }
                   // Commits a root (JOURNAL-DESIGN§1.5): every tree flushed up to this
                   //   entry's LSN is reachable from new_root. Recovery starts from the newest
                   //   valid Checkpoint and replays the entries after it.

// Data paths (JOURNAL-DESIGN§1.8)
WriteInline      : { oid: u64, content_hash: [u8;32], data: bytes }      // WalOpKind 32
                   // A whole object of at most INLINE_WRITE_MAX bytes, carried in the
                   //   journal itself; a later checkpoint moves it into a small-object bucket.
ExtendObject     : { oid: u64, new_length: u64, extents: [(extent: BlockRef, length: u32,
                     extent_sum: u64); N] }                               // WalOpKind 33
                   // Appends extents to an object in the Writing state and makes its first
                   //   new_length bytes durable.
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

1. Allocates a fresh `ChunkList` head region; sets `BTREE_NODE_FLAG_HEAD_OF_CHAIN` and
   writes the resolved `ChunkParamsRecord` immediately after the header (INDEX-DESIGN§4.3). The
   region holds 6 551 entries.
2. Splits plaintext using the resolved algorithm/params (e.g. FastCDC at the resolved
   `avg_size`). For each chunk: writes the chunk extent to a fresh blob bucket; appends
   the `(hash, extent, length)` triple to a pending `ChunkInsertBatch` and the hash to
   the region's entry array.
3. When the in-flight `ChunkInsertBatch` reaches the producer's chosen size cap (bounded
   by 1 000 chunks per entry), emits the batch as a journal entry.
4. When the head region fills (or fsync arrives), commits it and allocates a chained
   non-head region (no `ChunkParamsRecord`). Each region commit emits
   `ChunkListAppend { oid, position_start, region, count }`.
5. At end-of-write, emits `ChunkObjectFinalize { oid, content_hash, total_length, list_head }`.
   `list_head` points at the head region whose `ChunkParamsRecord` is now the durable
   record of the parameters this object was written with.

**Small in-place edit** (e.g. 4 KiB write at offset 1 GiB into a VM disk → FastCDC re-chunks
the affected window, replacing 1–3 chunks):

1. New chunks → `ChunkInsertBatch { chunks: [1..3] }`.
2. `ChunkListReplace { oid, position, new_hashes: [hash; 1..3] }`.

No `ChunkObjectFinalize` is needed for in-place edits — the object is already published; the
replace op modifies its existing ChunkList. The total `content_hash` and `total_length` only
change if the edit shifts logical length, in which case a follow-up `ChunkObjectFinalize` (or
`ChunkListShrink` for truncation) updates them.

**Sync receive / resumable transfer.** Identical to bulk-write but driven by stream arrival
order. After crash, the receiver queries the persisted ChunkList chain length and resumes
from there — the `ChunkListAppend` chain *is* the durable progress record.

**Truncate / delete.** `ChunkListShrink { oid, new_length: 0 }` drops the chain;
`DeleteObject` then removes the object record. ChunkIndex `ref_count` decrements happen during
shrink-replay; chunks reaching `ref_count = 0` are freed.

**Crash semantics.** Each step is independently durable: chunks written to fresh buckets but
not yet referenced from any `ChunkInsertBatch` get reclaimed by bucket-gen GC. ChunkList
regions written but not yet referenced from a `ChunkListAppend` get reclaimed similarly. A
chunked object that crashed before `ChunkObjectFinalize` is invisible to readers — its oid's
`ObjectLocation.replicas[0]` was never set — so no partial state is observable.

**Throughput model — 32 GiB bulk write, no dedup, across the FastCDC parameter space.**

FastCDC's avg-chunk-size knob spans ~3 orders of magnitude in real deployments (SPECIFICATION-DESIGN§9.6):
4 KiB for fine-grained edits at the small end, up to 8 MiB for resumability-only large
transfers at the other. The five-regime table below shows how the design scales:

| Avg chunk | Typical workload                   | Chunks      | `ChunkInsertBatch` entries (≤ 1 000 chunks each) | `ChunkListAppend` entries (one per 6 551-entry region) | Journal bytes (64 B per chunk) | Journal append rate at 530 MiB/s plaintext |
| --------- | ---------------------------------- | ----------- | ------------------------------------------------ | ------------------------------------------------------ | ------------------------------ | ------------------------------------------ |
| **4 KiB** | small extreme — fine-grained edits | 8 388 608   | 8 389                                            | 1 281                                                  | ~512 MiB (1.6 % of plaintext)  | ~8.3 MiB/s                                 |
| 16 KiB    | VM disks, databases                | 2 097 152   | 2 098                                            | 321                                                    | ~128 MiB (0.39 %)              | ~2.1 MiB/s                                 |
| 256 KiB   | generic FastCDC default            | 131 072     | 132                                              | 21                                                     | ~8 MiB (0.024 %)               | ~130 KiB/s                                 |
| 1 MiB     | backup-tool default (restic-class) | 32 768      | 33                                               | 6                                                      | ~2 MiB (0.006 %)               | ~33 KiB/s                                  |
| **8 MiB** | large extreme — pure resumability  | 4 096       | 5                                                | 1                                                      | ~256 KiB (0.0008 %)            | ~4 KiB/s                                   |

The spread is roughly **2 000×** across regimes on every axis — the same op set covers all
of it, only the constants change.

**Cumulative ≠ simultaneous.** Steady-state journal fill is bounded by *reclaim latency*, not
by total traffic over the operation: buckets are freed as soon as the trim point passes them
(JOURNAL-DESIGN§1.4). Reclaim's job is flushing ChunkIndex leaves (256 KiB regions packing
4 678 chunks each); the leaf-flush rate it must sustain is `chunks_per_second / 4 678`
leaves/s — equivalently, `(producer_plaintext_rate / avg_chunk_size) / 4 678` leaves/s. At
530 MiB/s plaintext:

- **4 KiB regime:** ~135 K chunks/s ÷ 4 678 = ~29 leaves/s = ~7.4 MiB/s of leaf rewrites.
  Comfortably within NVMe write bandwidth (~1–3 GiB/s typical) but no longer trivial; this
  is where reclaim genuinely has to keep up. Cumulative journal traffic exceeds a 64 MiB
  journal by ~8×, so its buckets cycle continuously.
- **16 KiB regime (canonical VM / DB workload):** ~7 leaves/s ≈ 1.8 MiB/s leaf rewrites.
  A 64 MiB journal cycles ~2× over the 60-second write. JOURNAL-DESIGN§1.4's *idle* /
  *background-flush* bands cover the producer comfortably.
- **256 KiB and above:** journal traffic is in the noise; reclaim is essentially never the
  bottleneck.

The design therefore scales from "fine-grained dedup workload that genuinely stresses
reclaim" to "barely-noticed bookkeeping" without changing op shapes. The producer's choice
of FastCDC parameters trades dedup granularity (small chunks → more shared chunks across
edits) against per-chunk overhead (small chunks → more journal bytes, more ChunkIndex leaves,
larger per-object ChunkList chain).

**HDD-only pools.** A pool whose only writable disk is rotational clamps the producer to
whatever reclaim sustains there. At the 4 KiB extreme this would be a real bottleneck (HDD
write throughput is far below the 7.5 MiB/s leaf-flush demand); at 1 MiB or above it is
unnoticeable. Chunked workloads typically imply hot-tier (NVMe/SSD) presence anyway, so
this is a corner case.

**Per-object footprint** (orthogonal to WAL but worth noting at the small extreme): a 32 GiB
object's `ChunkList` chain is `chunks ÷ 6 551` regions × 256 KiB. At 4 KiB chunks that's
~320 MiB of ChunkList per object (1 % of plaintext); at 8 MiB chunks it's a single 256 KiB
region. ChunkIndex footprint scales similarly. The very-small-chunk regime is therefore
viable but pays for itself in dedup hit-rate, not in raw efficiency — fine for VM-disk
workloads with high common-chunk ratios across snapshots, less so for one-shot writes of
unique data.

### 1.4 Journal pins and deferred btree flushes

Each in-memory dirty btree node carries a **journal pin** — the LSN of the oldest journal entry
whose update has not yet been merged into the on-disk node:

```rust
struct DirtyNode {                           // in-memory only
    block_ref: BlockRef,                     // current on-disk location
    last_persisted_lsn: u64,                 // BlockHeader.lsn of the on-disk version
    pending_lsn_min: u64,                    // oldest WAL entry pinning this node
    pending_lsn_max: u64,                    // newest WAL entry pinning this node
    pending_count: u32,                      // entries waiting to be merged
    in_memory: NodeContent,                  // merged live state (sorted)
}
```

**Trim point.** Each jset records `last_seq`, the oldest jset replay still needs: the seq of the
newest `Checkpoint`, or of the lowest live `pending_lsn_min` if that is older, or of the oldest
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
| 60 – 85 %  | Aggressive: rank dirty nodes by `(pending_lsn_max − pending_lsn_min)` × `pending_count` and flush the top fraction. |
| > 85 %     | New mutations stall until reclaim catches up.                               |

A flush rewrites the affected leaf via the COW path (METADATA-DESIGN§1), then each parent up to the root,
updating `BlockHeader.lsn = pending_lsn_max` and clearing the journal pin. The next `Checkpoint`
(JOURNAL-DESIGN§1.5) commits the new roots atomically. "Stall" means a mutation waits for, or
performs inline, the reclaim the embedder's driver has not yet run; the engine owns no thread.

**Memory reclaim.** A node may be evicted from the in-memory btree cache only after its pending
journal entries have been merged and the resulting page persisted. Clean nodes (no pending
updates) can be discarded immediately.

**Read path.** Lookups consult the in-memory dirty-node mirror first. Cold reads load the node
from disk, then the engine **replays** journal entries in `[node.last_persisted_lsn,
journal_head]` whose key range intersects the node, producing the merged live view. This replay
is bounded by journal size (~800 K entries worst-case), but the reclaim thresholds keep typical
lag under ~10 K entries per node.

**Locking model.** Two independent lock domains govern btree state. The mutation pipeline in
ENGINE-DESIGN§2 and the flush thread cooperate through them without a shared coarse mutex:

- **Per-tree write mutex** (mutation path). One mutex per snapshot-aware btree, plus one for
  the open jset. Writers reserve space in the open jset, copy their entry in and take its LSN
  under that lock, then update the in-memory mirror — the steps that must be atomic across
  concurrent writers. It is released before any per-node bookkeeping or I/O.
- **Per-DirtyNode rwlock** (flush path). One rwlock per loaded node. Because rewrites are
  full-node COW (a fresh page at a fresh location), the flush thread holds only the *shared*
  lock on the source DirtyNode while it builds the new page contents. The *exclusive* lock
  is taken only at the moment the parent `BlockRef` is swapped — milliseconds, regardless of
  node size. The mutation path also takes the per-DirtyNode exclusive lock briefly, after
  releasing the per-tree mutex, to bump `pending_lsn_max` / `pending_count` on affected
  nodes; this is uncontended in steady state because the flush thread holds shared most of
  the time.

The two domains are independent: a flush of node A runs concurrently with a mutation hitting
node B, and a mutation on node A only contends with a flush of node A at the brief
exclusive-swap moment. **Readers** consult the arc-swap'd index handle and are never blocked
on either lock — never on the per-tree mutex (writers release it before any disk I/O), never
on a DirtyNode lock (readers go through the arc-swap'd snapshot, not the live DirtyNode).

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
the new `RootPointer` (ENGINE-DESIGN§1.2). It writes nothing else: the superblock's root slots are
rewritten only on clean unmount and when the journal bucket list changes, as a hint that lets a
clean mount skip the journal scan.

**Recovery:**

1. Read the first jset of every journal bucket (JOURNAL-DESIGN§1.1); order buckets by their first
   `seq`; walk forward from the newest durable `last_seq` through valid, consecutive jsets.
2. The committed root is the newer of the superblock's root hint and the last `Checkpoint`
   entry found.
3. Verify every extent referenced by entries after that root, in the last jsets written before
   the tail (those whose commit may have been cut between data and barrier), against its
   `extent_sum`. An entry whose extent fails is dropped together with every later entry of its
   commit; the objects it touched stay at their previous state.
4. Replay the remaining entries after the root's LSN, in LSN order. Each in-memory mutation is
   idempotent under `lsn ≤ structure.lsn`, so a crash during replay is safe.

Recovery reads one jset per bucket plus the unflushed tail, and verifies data proportional to
that tail, never to the pool.

### 1.6 Retention

Jsets are self-describing, so history is kept by not freeing journal buckets yet. The trim point
(JOURNAL-DESIGN§1.4) is held back to the oldest subscription cursor that wants op replay
(SUBSCRIPTIONS-DESIGN "Offline catch-up"), within a cap on retained bytes and age set per pool.
A subscription whose cursor falls behind the cap catches up by result-set diff instead. The
retained journal is the oplog: nothing else records mutation history, and cluster sync reads
snapshot diffs, never the journal (CLUSTER-DESIGN).

### 1.7 Mirroring

When several hot-tier disks carry journal buckets, each commit writes the same jset — same
`seq`, same bytes — to the current journal bucket of each, and is acknowledged once the
configured number of copies is durable. Recovery merges the disks' journals by `seq`, taking any
copy that validates. Whether and how the journal mirrors by default is JOURNAL-D1.

### 1.8 Data paths

Object bytes do not go through the journal, with one exception:

| Write | Path |
|---|---|
| Tiny: whole object ≤ `INLINE_WRITE_MAX` (2 KiB by default, set per pool) | `WriteInline` carries the bytes; one jset write makes the object durable. A later checkpoint moves inline objects into small-object buckets that pack many objects per block, so a 200-byte file does not occupy a 4 KiB blob sector ([BLOB-DESIGN](blob-design.md)). |
| Small to large | Bytes go to blob extents at the stream's write point (ALLOCATION-DESIGN§1.5); `WriteBlob` or the chunked flow (JOURNAL-DESIGN§1.3.1) records them, with `extent_sum`s. |
| Streaming (camera recording, logs, downloads) | The object is created in the `Writing` state (METADATA-DESIGN§1.1); bytes are appended to extents taken whole bucket by bucket from a per-stream write point, so the stream is physically sequential; `ExtendObject` (or `ChunkListAppend` for chunked streams) makes each prefix durable, typically under `Buffered` durability once per second or on request. Closing the object sets its final length and content hash, computed incrementally while streaming, and moves it to `Active`. |

After a crash a `Writing` object ends at its last journaled length; bytes written beyond it are
reclaimed. Readers may follow a `Writing` object up to its durable length.

---

