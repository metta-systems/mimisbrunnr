# Allocation — design

Scope: `crates/storage/src/allocator.rs`, `alloc.rs`, `freespace.rs`, `alloc_bitmap.rs`; format-time seeding in `crates/pool`.
Governing spec: ALLOCATION-DESIGN§1 (ALLOCATION-DESIGN§1.1 buckets, ALLOCATION-DESIGN§1.2 `BucketAllocEntry`, ALLOCATION-DESIGN§1.3 generation-checked pointers, ALLOCATION-DESIGN§1.4 freespace LRU, ALLOCATION-DESIGN§1.5 write points, ALLOCATION-DESIGN§1.6 copy GC, ALLOCATION-DESIGN§1.7 discard), BTREE-DESIGN§1.5 bucket packing.

Plan: [allocation-implementation-plan.md](allocation-implementation-plan.md) · Open decisions: [allocation-decisions.md](allocation-decisions.md) · Completed: [allocation-completed.md](allocation-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Every on-disk structure outside the fixed bootstrap area lives in an allocated bucket, addressed by a generation-checked `BlockRef`, so trees can be copied on write, old copies freed, and buckets reclaimed by copy GC.

## Selected design

- `BucketAllocator { alloc, free, validate, seed_freelist, block_ref_offset }` claims a free bucket, bumps its `generation`, stamps its `data_type`, and returns `BlockRef { disk_id, block_no = bucket_no × sectors_per_bucket, generation }`.
- The leading `bootstrap_buckets` (superblock, WAL header, alloc-table roots) are never handed out and are excluded from the freespace LRU.
- **Region packing.** A 256 KiB region occupies a quarter of a 1 MiB bucket; the allocator hands out successive sub-extents of one bucket before claiming the next (BTREE-DESIGN§1.5).
- **Dereference protocol.** Reading through a `BlockRef` checks the bucket's current `generation`; a mismatch is `StaleBlockRef`.
- **Per-disk roots.** Each disk's bucket-alloc table and freespace LRU are rooted at `DiskDescriptorOnDisk.buckets_root` / `freespace_root`; their keys carry no `disk_id`.

## Invariants

- A bucket is never handed out twice without a generation bump.
- A `BlockRef` held across a free is detected as stale on dereference.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Allocation Layer

Allocation is **bucket-based** with **generation numbers**, modelled on bcachefs. Each disk is
divided into fixed-size buckets (typically 1 MiB; configurable 256 KiB – 4 MiB at format time).
Within a bucket, writes are **append-only**: once opened, sectors are written sequentially and
never overwritten. A bucket is reused only after its generation counter has been incremented —
which atomically invalidates every pointer that referenced its previous contents.

This buys four properties simultaneously:

- **Constant-time bucket invalidation.** Bumping a bucket's generation invalidates every pointer
  that referenced the old contents — no scan required.
- **Crash-safe pointer staleness detection.** Every `BlockRef` carries the bucket's expected
  generation; mismatches are silently dropped on read.
- **Native fit for SMR / zoned drives.** Buckets map one-to-one to zones; write-once-then-erase
  is exactly the semantics zoned media require. Glacier-tier media can be addressed without an
  intervening FTL.
- **Filesystem-scoped FTL.** On SSDs, the filesystem becomes the FTL with full visibility into
  what is live — more predictable than the drive's own FTL under our access patterns.

### 1.1 Buckets

A bucket is identified by `(disk_id, bucket_no)`. Bucket size is encoded as
`Superblock.bucket_size_log2`:

| `bucket_size_log2` | Bucket size | Buckets per 16 TiB disk | Alloc table size |
| ------------------ | ----------- | ----------------------- | ---------------- |
| 18                 | 256 KiB     | 64 M                    | 1 GiB            |
| 20 (default)       | 1 MiB       | 16 M                    | 256 MiB          |
| 22                 | 4 MiB       | 4 M                     | 64 MiB           |

`Superblock.bootstrap_buckets` (typically 32–64) pins the leading buckets used for the
superblock, WAL header, and the root pages of the alloc table itself; these are excluded from the
freespace LRU.

### 1.2 Bucket alloc table

One **BTREE-DESIGN§1 B+ tree** per disk, rooted at `DiskDescriptorOnDisk.buckets_root`, keyed by
`bucket_no: u32`. With key packing (BTREE-DESIGN§1.6) sequential bucket numbers compress to ~2 B per key,
so a 256 KiB leaf packs ~14 600 BucketAllocEntry entries per sorted run; a 16 M-bucket disk fits in
**depth 1** (single inner node + ~1 100 leaves):

```rust
#[repr(C, packed)]
struct BucketAllocEntry {                      // 16 bytes
    generation: u32,                         // monotonic; matched by BlockRef.generation
    data_type: u8,                           // BucketDataType (below)
    flags: u8,                               // BUCKET_FLAG_*
    dirty_sectors: u16,                      // live 4 KiB sectors written in this bucket
    last_modify_lsn: u64,                    // for snapshot-diffing the alloc table itself
}

// BucketAllocEntry.flags bits
const BUCKET_FLAG_NEEDS_DISCARD:      u8 = 1 << 0;  // queued for TRIM (ALLOCATION-DESIGN§1.7)
const BUCKET_FLAG_PINNED_BY_SNAPSHOT: u8 = 1 << 1;  // contents reachable from a live snapshot

#[repr(u8)]
enum BucketDataType {
    Free        = 0,                         // not in use; freespace LRU candidate
    Wal         = 1,                         // WAL ring buckets
    Index       = 2,                         // index-zone btree pages, tag bitmaps
    Metadata    = 3,                         // object table, location table, forward index
    Blob        = 4,                         // user data blobs
    BtreeNode   = 5,                         // dedicated btree-node buckets
    Stripe      = 6,                         // erasure-coding stripes (future)
    NeedDiscard = 7,                         // freed, awaiting TRIM
    Reserved    = 8,                         // copygc forward-progress reserve
}
```

The B+ tree itself is housed in a self-bootstrapping subset of metadata buckets (tracked as
`BucketDataType::Metadata` with the pinned flag). Updates go through the journal and are
checkpointed in the same A/B atomic-root commit as everything else.

### 1.3 Generation-checked pointers

`BlockRef` (ENGINE-DESIGN§1.2) already carries a 64-bit generation. With bucket-based allocation:

```
bucket_no   = block_no >> (bucket_size_log2 - 12)        // e.g. block_no >> 8 for 1 MiB buckets
in_bucket   = block_no &  ((1 << (bucket_size_log2 - 12)) - 1)
```

Dereference protocol:

1. Compute `bucket_no` from `block_no`.
2. Look up `BucketAllocEntry` (hot buckets pinned in RAM).
3. If `key.generation != ref.generation`, the pointer is **stale**: silently dropped on reads,
   logged as a corruption signal during scrub.

Generation comparison alone resolves free-block tracking, torn-write detection on freed blocks,
and stale-replica handling — no separate bookkeeping is required. The per-block CRC32C catches
in-bucket corruption independently.

### 1.4 Freespace LRU

A second **BTREE-DESIGN§1 B+ tree** per disk (`DiskDescriptorOnDisk.freespace_root`), keyed by
`(fragmentation_band, bucket_no)`:

```
fragmentation_band: u8     // ⌈255 × dirty_sectors / sectors_per_bucket⌉
                           //   = 0   iff dirty_sectors == 0   (empty / full-free)
                           //   = 255 when dirty is at or near full (formula saturates
                           //         for the last few sectors because of the ceiling)
                           //   ∈ 1..=254 in between
bucket_no: u32
```

where `sectors_per_bucket = 1 << (Superblock.bucket_size_log2 - 12)` (the count of 4 KiB
sectors in one bucket — 256 for the default 1 MiB bucket, 1024 for the 4 MiB maximum).
Band 0 is reserved for the "fully free" allocator fast path (foreground allocator scans
`fragmentation_band == 0` only) and is reached *only* when `dirty_sectors == 0`. Band 255
is the most-fragmented end and absorbs the last sector or two of occupancy via the ceiling
— at the default 256 sectors, dirty ∈ {255, 256} both map to 255. Intermediate bands
scale roughly linearly. Bands are recomputed lazily when `dirty_sectors` crosses an
8-sector boundary, keeping freespace-LRU churn proportional to allocation pressure rather
than to write volume.

Used by:

- **Foreground allocator**: scans `fragmentation_band == 0` for fast bump-allocation of new
  write streams.
- **Copy GC**: scans the most-fragmented non-empty buckets to reclaim space (ALLOCATION-DESIGN§1.6).
- **Cache eviction**: cached-replica buckets carry their own LRU, layered on top.

### 1.5 Allocator behaviour

Per-disk **write points** track a small set of currently-open buckets, segregated by:

- **Data type** — index, metadata, blob, and WAL never share a bucket.
- **Stream tag** — separate write points for placement-target groups, foreground vs. background
  workloads, and per-tag pinning. This is the same trick bcachefs uses to keep unrelated I/O
  patterns from co-mingling and producing correlated fragmentation.

Allocation on the fast path is a bump: the open bucket's write cursor advances by the requested
sectors. When a bucket fills, it is closed (its `dirty_sectors` stops changing until copygc) and
a fresh bucket is opened from the freespace LRU.

Per-zone hints (ENGINE-DESIGN§1.1) are advisory rather than hard partitions: a zone defines the *preferred*
disk region for a data type, but the allocator can spill across zone boundaries when the zone
runs short. The authoritative classification is `BucketAllocEntry.data_type` per bucket.

### 1.6 Copy GC

Copy GC is one work kind in the reconcile subsystem (RECONCILE-DESIGN§1, `WorkKind::Copygc`). When the
freespace LRU's empty-band count drops below the configured reserve
(`Superblock.copygc_reserve_pct`, default 8 %), the reconcile engine enqueues `Copygc` items
for the most-fragmented buckets.

Each `Copygc` work item is processed via the standard move path (RECONCILE-DESIGN§1.5):

1. **Range-scan the backpointers btree (METADATA-DESIGN§2.2) at prefix `(disk_id, bucket_no, *)`** to
   enumerate live extents in the bucket. Entries with stale `bucket_gen` are skipped.
2. Read each live extent from disk (CRC32C validated; BLAKE3 verified at the object level if
   the owner is `BlobExtent`).
3. Write to a fresh bucket via the move path.
4. Emit a single `ReconcileMove { from_loc, to_loc, owner_key }` WAL op (JOURNAL-DESIGN§1.3, RECONCILE-DESIGN§1.5). Replay
   projects this into `BackpointerRemove(old_key) ∘ BackpointerInsert(new_key, new_value) ∘
   owner-update` applied as a single LSN's worth of btree mutations.
5. Bump the old bucket's generation, transition it to `NeedDiscard` (or directly to `Free`).
   The generation bump implicitly invalidates any backpointers missed during the scan — they
   become detectable-stale on the next pass.

Copy GC cost is proportional to **fragmentation**, not pool size. The bucket-prefix scan is
~1 large-node load per fragmented bucket. The reserve guarantees forward progress: allocation
can never block on GC because at least one fully-empty bucket is always available.

### 1.7 Discard / TRIM

Buckets transitioning to `Free` first sit in `NeedDiscard` until TRIM is issued (mount option
`discard=true|async|off`). On rotational media TRIM is a no-op and the transition is immediate.

### 1.8 Crash recovery

The bucket alloc table is a B+ tree under the standard COW + journal commit machinery (ENGINE-DESIGN§1.2).
On unclean shutdown:

1. Each disk's `buckets_root` is loaded from the last committed `RootPointer`.
2. WAL replay applies pending bucket-lifecycle entries (`BucketAlloc`, `BucketWrite`,
   `BucketGenBump`, `BucketDiscard`) in LSN order.
3. Any block whose `BlockRef.generation` does not match the recovered `BucketAllocEntry.generation`
   is treated as stale and ignored — exactly as it would be at runtime.

The combination of "WAL is authoritative for recent transitions" and "generation mismatches
invalidate pointers atomically" makes recovery proportional to journal size, not pool size.

---

