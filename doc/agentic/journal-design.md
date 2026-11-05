# Journal (WAL) — design

Scope: `crates/wal`, the WAL projection in `crates/engine/src/wal_proj.rs`, checkpoint cadence in `crates/engine`.
Governing spec: JOURNAL-DESIGN§1 (JOURNAL-DESIGN§1.2 entry format, JOURNAL-DESIGN§1.3 ops, JOURNAL-DESIGN§1.4 journal pins and deferred flushes, JOURNAL-DESIGN§1.5 checkpoints), SPECIFICATION-DESIGN§6.4.

Plan: [journal-implementation-plan.md](journal-implementation-plan.md) · Open decisions: [journal-decisions.md](journal-decisions.md) · Completed: [journal-completed.md](journal-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

The WAL is the durability point for every mutation; B+ tree pages are flushed lazily behind it, and the WAL is trimmed only past entries that every in-memory node has persisted.

## Selected design

- **Journal pins.** Each `LoadedNode` holds one `JournalPin { lsn_min, lsn_max, count }` over its pending journal. The checkpoint advances `read_cursor` only to `min(checkpoint_lsn, lowest live lsn_min)`.
- **`JournalPin` lives in `mimisbrunnr-storage`**, beside `LoadedNode`; `mimisbrunnr-wal` depends on storage and uses that type. One definition only.
- **Checkpoint cadence** is driven by WAL fill level (JOURNAL-DESIGN§1.4 thresholds), accumulated dirty bytes and elapsed time (30 s). The engine exposes `checkpoint` and a query of which band is due; it starts no thread. The embedder drives it — a `std`-gated helper thread on the host, the filesystem server's own thread or event loop on Vesper (sync-only contract: no async runtime). `commit()` remains as a forced flush for tests and foreground sync points.
- **`FormatPromote`** (`WalOpKind::FormatPromote = 30`) journals a sorted-run format widening before the widened run is written; replay re-applies it before decoding later runs.
- Compression and encryption flags on WAL entries follow JOURNAL-DESIGN§1.2: compress, then encrypt ([encryption-design.md](encryption-design.md)).

## Invariants

- No mutation is reported durable before its WAL entry is written.
- WAL trimming never passes an entry a pending node still needs.
- Replay from the last checkpoint reproduces the committed state.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Write-Ahead Log

The WAL is the **btree-update journal**: a 64 MiB circular log (configurable; must be a multiple
of 4 KiB), hosted on the pool's **hot-tier** disks (`StorageTier::Hot` — NVMe / SSD; see
SPECIFICATION-DESIGN§8.1). On disks that don't carry a ring, `Superblock.wal_offset` and `wal_size` are
simply `0` — no allocation, no reservation. Adding a ring later (e.g. a cold disk being
promoted to hot, or an operator widening the mirror) is a routine allocation: 64 MiB is a
trivial reservation on cold-tier capacities, so the format doesn't pre-allocate the slot.

Each entry records one key-level mutation (add tag, write extent, bucket transition, …).
Btree nodes on disk are **not** rewritten per mutation — they are rewritten lazily when
journal reclaim or memory pressure demands it (JOURNAL-DESIGN§1.4). The journal is therefore the **source
of truth** for any btree state newer than each node's `BlockHeader.lsn`.

**Mirroring across hot disks.** WAL appends fan out to every hot-tier disk's ring as the same
byte sequence under a single shared LSN — typically the fastest disk as primary plus one
mirror (SPECIFICATION-DESIGN§8.2 illustrates NVMe primary + SSD mirror, with the HDD carrying no ring).
Cold and Glacier-tier disks are deliberately excluded: their latency would dominate fsync
and they exist to hold cold blob extents, not hot mutation state. A user fsync waits for the
append to land on at least the configured replica count of hot rings (default 2) before
returning; recovery picks whichever ring has the highest valid LSN and uses the others to
fill any missing entries. Per-disk rings share an LSN namespace because each is the same
logical journal, just persisted in parallel.

If the pool has only one hot-tier disk, the WAL runs unmirrored on that disk and the user is
responsible for accepting the durability tradeoff (or adding a second hot disk).

Per-mutation cost is one ≈ 80-byte journal append. Btree page rewrites are amortised across all
mutations that touched each node since its last flush. A tag mutation that touches four index
trees costs four journal entries; the corresponding page rewrites land later, batched.

Internal layout is segmented to allow parallel truncation and replay.

### 1.1 WAL headers (two 4 KiB blocks at `wal_offset`)

```rust
#[repr(C, packed)]
struct WalHeader {                           // 4096 bytes (one block)
    header: BlockHeader,                     // [0..32]   kind = WalSegment, format_version = 1
    next_lsn: u64,                           // [32..40]
    write_cursor: u64,                       // [40..48]  byte offset within WAL ring
    read_cursor: u64,                        // [48..56]  oldest entry not yet checkpointed
    used_bytes: u64,                         // [56..64]
    last_checkpoint_lsn: u64,                // [64..72]  newest Checkpoint entry's LSN;
                                             //           recovery scans forward from there
    segment_size: u32,                       // [72..76]  typically 1 MiB
    _pad: u32,                               // [76..80]  align to u64
    encryption_keyid: [u8; 16],              // [80..96]
    _reserved: [u8; 3996],                   // [96..4092]
    // trailing CRC32C at [4092..4096] inside BlockHeader's frame
}
```

The header is updated using the same A/B alternation as the superblock root — the WAL header
lives in two adjacent blocks; the active one is the copy with the larger
`BlockHeader.generation` whose CRC validates. (The `BlockHeader.generation` field is the same
monotonic counter that drives every COW block update; no separate `seq` is needed.)

### 1.2 WAL entry

Entries are byte-packed and **never cross a 4 KiB sector boundary**: each entry is one fully-formed
`WalEntryHeader`+payload+CRC frame within a single sector. The bound on `payload_length` is
`4096 − WalEntryHeader (40) − framing CRC32C (4) = 4052 B` plaintext; encrypted entries pay an
additional 16 B GCM tag and so cap at 4036 B. An attempted append exceeding the bound is a
programming error: the producer must spill bulk data through the blob zone (INDEX-DESIGN§1.1) and reference it
by hash, not embed it in a WAL op. Sector-aligned framing makes recovery trivial — a torn 4 KiB
write loses at most one entry, and replay never has to reassemble fragments across sectors.

```rust
#[repr(C, packed)]
struct WalEntryHeader {                      // 40 bytes
    magic: u32,                              // "WALR"
    op_kind: u8,                             // WalOpKind
    format_version: u8,
    flags: u16,                              // WAL_ENTRY_FLAG_*
    lsn: u64,
    timestamp: HybridTimestamp,              // 16 bytes — defined below
    payload_length: u32,
    payload_crc: u32,                        // CRC32C of payload (post-compression/encryption)
}

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

// WalEntryHeader.flags bits
const WAL_ENTRY_FLAG_ENCRYPTED:  u16 = 1 << 0;  // payload encrypted (AES-256-GCM)
const WAL_ENTRY_FLAG_COMPRESSED: u16 = 1 << 1;  // payload compressed (zstd) before encryption
```

**Plaintext framing** (`WAL_ENTRY_FLAG_ENCRYPTED == 0`):

```
WalEntryHeader (40 B) | payload (payload_length B) | framing CRC32C (4 B)
```

`payload_crc` covers the payload bytes; the trailing 4-byte CRC32C covers `WalEntryHeader ||
payload` for end-to-end framing detection.

**Encrypted framing** (`WAL_ENTRY_FLAG_ENCRYPTED == 1`):

```
WalEntryHeader (40 B) | ciphertext (payload_length B) | GCM tag (16 B) | framing CRC32C (4 B)
```

The 16-byte AES-256-GCM tag sits between the ciphertext and the trailing framing CRC — it does
**not** replace `payload_crc`, which is too narrow (4 B) to hold it. `payload_length` is
unchanged because GCM is length-preserving; it counts ciphertext bytes, which equal plaintext
bytes (after optional zstd compression). Encrypted-entry overhead is 60 B (header + tag +
framing CRC) versus 44 B plaintext.

Encryption inputs (cipher details: ENCRYPTION-DESIGN§1):

- **Key**: `DiskKey`.
- **Nonce**: 96 bits = 64-bit `lsn` || 32-bit zero. Unique by LSN monotonicity (ENCRYPTION-DESIGN§1).
- **AAD**: the first 36 bytes of `WalEntryHeader` — every field except `payload_crc`. This
  authenticates magic, op_kind, format_version, flags, lsn, timestamp, and payload_length,
  preventing header-swap attacks. `payload_crc` is excluded because it is computed *after* the
  GCM call (over the ciphertext that GCM produces) and so cannot be an input to it.
- **Plaintext**: the op's binary payload record (JOURNAL-DESIGN§1.3), optionally zstd-compressed first when
  `WAL_ENTRY_FLAG_COMPRESSED` is also set. Order is *compress, then encrypt* — never the
  reverse, since GCM ciphertext is incompressible.

In encrypted mode `payload_crc` is CRC32C over the ciphertext (computed post-GCM). Readers
without the key can still validate I/O integrity via the two CRCs; readers with the key
additionally authenticate via the GCM tag.

**Write order.** (1) write the payload record → (2) optional zstd compress → (3) AES-256-GCM encrypt with
the AAD-and-nonce above → (4) compute `payload_crc` over ciphertext, fill in `WalEntryHeader` →
(5) compute trailing framing CRC over the assembled bytes → (6) issue a single sequential write
to the WAL ring.

**Read / replay order.** (1) verify trailing framing CRC (cheap, no key needed); reject the
entry on mismatch — torn write or media corruption. (2) verify `payload_crc` over ciphertext
(also key-free). (3) if `WAL_ENTRY_FLAG_ENCRYPTED`: AES-256-GCM-decrypt with header[0..36] as
AAD and the LSN-derived nonce; reject on tag mismatch (tampering or wrong key). (4) if
`WAL_ENTRY_FLAG_COMPRESSED`: zstd-decompress. (5) read the payload record for `op_kind`.

Payloads are binary records (FORMAT-DESIGN§1.6), not CBOR: every op's shape is known from
`op_kind`, so a record carries no field names or type information, its bytes are fixed by its
field values alone (what AEAD authentication needs), and replay reads fields in place. The only
CBOR inside a WAL payload is an attribute `Value` (INDEX-DESIGN§1). A WAL entry is versioned
by `WalEntryHeader.format_version`; the WAL is replayed and trimmed, never migrated in place.

### 1.3 WAL op payloads

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
- `payload_length` covers the head, the variable part and its padding; a reader rejects a
  payload whose length disagrees with its head.

```rust
CreateObject     : { oid: u64, generation: u32, created_ns: i64 }
DeleteObject     : { oid: u64, lsn: u64 }
AddTag           : { oid: u64, tag: u32, origin: u8 }
RemoveTag        : { oid: u64, tag: u32 }
SetAttr          : { oid: u64, key: u32, value: Value }       // Value tagged-union (INDEX-DESIGN§1)
RemoveAttr       : { oid: u64, key: u32, value_hash: u64 }
AddRelation      : { oid: u64, predicate: u32, target: u64 }
RemoveRelation   : { oid: u64, predicate: u32, target: u64 }
WriteBlob        : { oid: u64, content_hash: [u8;32], extent: BlockRef, size: u64 }
                   // Non-chunked objects only. Chunked objects (INDEX-DESIGN§4.3) use the
                   // ChunkInsertBatch / ChunkListAppend / ChunkObjectFinalize flow
                   // described in JOURNAL-DESIGN§1.3.1.

// FastCDC chunked objects (INDEX-DESIGN§4.3, JOURNAL-DESIGN§1.3.1)
ChunkInsertBatch    : { chunks: [(chunk_hash: [u8;32], extent: BlockRef, length: u32); N] }
                   // N chosen by producer s.t. encoded op fits the JOURNAL-DESIGN§1.2 sector cap;
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

// Tag bitmap allocation lifecycle (INDEX-DESIGN§3.1, INDEX-DESIGN§3.2)
TagBitmapGrow    : { tag_id: u32, snapshot: u32, store_kind: u8, new_root: BlockRef }
                   // A tag's bitmap was allocated, relocated, or extended into a new
                   //   region. Replay sets the matching TagDirectory entry's store_root
                   //   (and store_kind) to new_root. Emitted on first AddTag for a new
                   //   (tag, snapshot) pair, on bitmap region growth past 4 KiB, and
                   //   after reconcile-driven moves of a tag-bitmap extent.
TagBitmapShrink  : { tag_id: u32, snapshot: u32 }
                   // A tag's bitmap is now empty (cardinality dropped to 0). Replay
                   //   removes the TagDirectory entry; the bitmap region is reclaimed
                   //   via bucket-gen bump. Emitted by the last RemoveTag for the
                   //   (tag, snapshot) pair.
// AddTag / RemoveTag (above) carry membership changes; TagBitmapGrow / TagBitmapShrink
// carry pointer / lifecycle changes. The split decouples WAL replay from the
// TagDirectory leaf shape — recovery never has to mimic the runtime allocation policy.

// Snapshot lifecycle (SNAPSHOTS-DESIGN§1)
SnapshotCreate   : { new_id: u32, parent_id: u32, current_replacement: u32, label: Option<String> }
                   // label capped at 256 bytes — one-sector WAL invariant (JOURNAL-DESIGN§1.2)
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
                   //   cursor_key capped at 256 bytes — one-sector WAL invariant (JOURNAL-DESIGN§1.2)

// Sorted-run format mutations (BTREE-DESIGN§1.6)
FormatPromote    : { node_ref: BlockRef, sorted_run_seq: u32, new_format: SortedRunKeyFormat }

Checkpoint       : { new_root: RootPointer, gc_reserve_buckets: u32 }
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
   by `WAL_OP_MAX_PAYLOAD`), emits the batch as a WAL op.
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

| Avg chunk | Typical workload                   | Chunks      | `ChunkInsertBatch` ops (≤ 76/op) | `ChunkListAppend` ops (one per 6 551-entry region) | Cumulative WAL bytes (worst case: 1 sector/op) | WAL append rate at 530 MiB/s plaintext |
| --------- | ---------------------------------- | ----------- | -------------------------------- | -------------------------------------------------- | ---------------------------------------------- | -------------------------------------- |
| **4 KiB** | small extreme — fine-grained edits | 8 388 608   | 110 377                          | 1 281                                              | ~436 MiB (1.3 % of plaintext)                  | ~7.1 MiB/s                             |
| 16 KiB    | VM disks, databases                | 2 097 152   | 27 595                           | 321                                                | ~109 MiB (0.33 %)                              | ~1.8 MiB/s                             |
| 256 KiB   | generic FastCDC default            | 131 072     | 1 725                            | 21                                                 | ~6.8 MiB (0.020 %)                             | ~114 KiB/s                             |
| 1 MiB     | backup-tool default (restic-class) | 32 768      | 432                              | 6                                                  | ~1.7 MiB (0.0052 %)                            | ~29 KiB/s                              |
| **8 MiB** | large extreme — pure resumability  | 4 096       | 54                               | 1                                                  | ~220 KiB (0.00067 %)                           | ~3.6 KiB/s                             |

The spread is roughly **2 000×** across regimes on every axis — the same op set covers all
of it, only the constants change.

**Cumulative ≠ simultaneous.** The 64 MiB ring is circular. Steady-state ring fill is bounded
by *reclaim latency*, not by total traffic over the operation. Reclaim's job is flushing
ChunkIndex leaves (256 KiB regions packing 4 678 chunks each); the leaf-flush rate it must
sustain to keep the ring drained is `chunks_per_second / 4 678` leaves/s — equivalently,
`(producer_plaintext_rate / avg_chunk_size) / 4 678` leaves/s. At 530 MiB/s plaintext:

- **4 KiB regime:** ~135 K chunks/s ÷ 4 678 = ~29 leaves/s = ~7.4 MiB/s of leaf rewrites.
  Comfortably within NVMe write bandwidth (~1–3 GiB/s typical) but no longer trivial; this
  is where reclaim genuinely has to keep up. Cumulative WAL exceeds the 64 MiB ring by ~7×,
  so the ring cycles continuously.
- **16 KiB regime (canonical VM / DB workload):** ~7 leaves/s ≈ 1.8 MiB/s leaf rewrites.
  Ring cycles ~1.7× over the 60-second write. JOURNAL-DESIGN§1.4's *idle* / *background-flush* bands
  cover the producer comfortably.
- **256 KiB and above:** WAL traffic is in the noise; reclaim is essentially never the
  bottleneck. The ring barely fills.

The design therefore scales from "fine-grained dedup workload that genuinely stresses
reclaim" to "barely-noticed bookkeeping" without changing op shapes. The producer's choice
of FastCDC parameters trades dedup granularity (small chunks → more shared chunks across
edits) against per-chunk overhead (small chunks → more WAL ops, more ChunkIndex leaves,
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

Each in-memory dirty btree node carries a **journal pin** — the LSN of the oldest WAL entry
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

**Journal reclaim invariant.** `WalHeader.read_cursor` (the LSN below which entries may be
overwritten) cannot advance past `min(all_dirty_nodes.pending_lsn_min)`. The reclaim thread
monitors WAL fill level:

| Fill level | Reclaim behaviour                                                           |
| ---------- | --------------------------------------------------------------------------- |
| < 25 %     | Idle. No flushes scheduled.                                                 |
| 25 – 60 %  | Background flushes at low priority — flush nodes with highest pin pressure. |
| 60 – 85 %  | Aggressive: rank dirty nodes by `(pending_lsn_max − pending_lsn_min)` × `pending_count` and flush the top fraction. |
| > 85 %     | New mutations stall until reclaim catches up.                               |

A flush rewrites the affected leaf via the COW path (METADATA-DESIGN§1), then each parent up to the root,
updating `BlockHeader.lsn = pending_lsn_max` and clearing the journal pin. The next checkpoint
(ENGINE-DESIGN§1.2) commits the new root pointers atomically.

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
  the WAL ring. It serialises LSN allocation, the WAL append, and the in-memory-mirror
  update — the steps that must be atomic across concurrent writers. It is released before
  any per-node bookkeeping.
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

**Durability semantics.**

- A user fsync triggers a journal flush (an fsync of the WAL ring) — the mutation is durable
  even though its btree page may not yet be persisted.
- Crash recovery reads the active root pointer, then replays all WAL entries with
  `lsn > root.lsn` against the loaded btree.
- `RootPointer.lsn` therefore lags `WalHeader.next_lsn`; the gap is the unmaterialised journal
  tail, bounded by reclaim policy.

### 1.5 Atomic root commit cadence

The ENGINE-DESIGN§1.2 commit protocol is invoked when:

1. Btree topology actually changes (root split / merge / new tree depth).
2. Sufficient flushes have accumulated that committing a fresh root meaningfully advances
   `read_cursor`. Default: whichever of the two thresholds is hit first —
   - **30 s** of mutation activity, or
   - **256 MiB** of **flushed btree-page bytes**: cumulative bytes the journal-reclaim
     thread has rewritten to fresh buckets since the last checkpoint. Not WAL bytes — the
     WAL ring itself is only 64 MiB.
3. A snapshot is created — snapshot creation forces a checkpoint so the snapshot's
   `RootPointer` materialises a coherent btree state.

Routine mutations do **not** flip the superblock root.

---

