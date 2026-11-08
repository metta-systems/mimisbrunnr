# Allocation — design

Scope: `crates/storage/src/allocator.rs`, `alloc.rs`, `freespace.rs`, `alloc_bitmap.rs`; format-time seeding in `crates/pool`.
Governing spec: ALLOCATION-DESIGN§1 (ALLOCATION-DESIGN§1.1 buckets, ALLOCATION-DESIGN§1.2 `BucketAllocEntry`, ALLOCATION-DESIGN§1.3 generation-checked pointers, ALLOCATION-DESIGN§1.4 freespace LRU, ALLOCATION-DESIGN§1.5 write points, ALLOCATION-DESIGN§1.6 copy GC, ALLOCATION-DESIGN§1.7 discard), BTREE-DESIGN§1.5 bucket packing.

Plan: [allocation-implementation-plan.md](allocation-implementation-plan.md) · Open decisions: [allocation-decisions.md](allocation-decisions.md) · Completed: [allocation-completed.md](allocation-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Every on-disk structure except the superblock copies lives in an allocated bucket, addressed by a generation-checked `BlockRef`, so trees can be copied on write, old copies freed, and buckets reclaimed by copy GC.

## Selected design

- `BucketAllocator { alloc, free, validate, seed_freelist }` claims a free bucket — whose `generation` was bumped when it was freed (`BucketGenBump`, ALLOCATION-DESIGN§1.6) — stamps its kind, and returns bucket-relative `BlockRef`s (ENGINE-DESIGN§1.2) into it.
- **Bucket kinds** say what a bucket holds — journal, tree nodes, data extents, packed small objects — and nothing else classifies space; there are no zones.
- **Open and closed buckets.** A bucket is *open* while a write point owns it and *closed* otherwise. A closed bucket becomes free when its live-sector count reaches zero; open-ness is in memory only, and every bucket is closed after a crash.
- **Live sectors are derived.** The counts change with the journal entries that add or drop references and with checkpoint node rewrites; replay recomputes them, so no per-write accounting entry exists.
- **Generations are resident.** Each disk's bucket generations are held in memory as a `u32` array (16 MiB for 4 M buckets); a dereference compares against it without a tree lookup.
- **A stale committed pointer is corruption.** A generation mismatch on a pointer reachable from the committed root is reported as an error; only backpointers and cached replicas are invalidated lazily by generation.
- **Region packing.** A 256 KiB region occupies a quarter of a 1 MiB bucket; the allocator hands out successive regions of one open bucket before claiming the next (BTREE-DESIGN§1.5).
- **Per-disk roots.** Each disk's bucket-alloc table and freespace LRU are rooted in its disk descriptor (POOL-DESIGN§1); their keys carry no `disk_id`.

## Invariants

- A bucket is never handed out twice without a generation bump.
- A bucket is reused only after every reference into it is gone from the committed root.
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
  generation; a mismatch is detected on dereference.
- **Native fit for SMR / zoned drives.** Buckets map one-to-one to zones; write-once-then-erase
  is exactly the semantics zoned media require. Glacier-tier media can be addressed without an
  intervening FTL.
- **Filesystem-scoped FTL.** On SSDs and SD cards, the filesystem becomes the FTL with full
  visibility into what is live — more predictable than the drive's own FTL under our access
  patterns.

### 1.1 Buckets

A bucket is identified by `(disk_id, bucket_no)`. Bucket size is encoded as
`Superblock.bucket_size_log2`:

| `bucket_size_log2` | Bucket size | Buckets per 16 TiB disk | Alloc table size |
| ------------------ | ----------- | ----------------------- | ---------------- |
| 18                 | 256 KiB     | 64 M                    | ~640 MiB         |
| 20 (default)       | 1 MiB       | 16 M                    | ~160 MiB         |
| 22                 | 4 MiB       | 4 M                     | ~40 MiB          |

The buckets holding the superblock copies (the first bucket and the last, ENGINE-DESIGN§1.1)
are never handed out; `Superblock.first_usable_bucket` and the disk's `bucket_count` bound the
allocatable range.

### 1.2 Bucket alloc table

One **BTREE-DESIGN§1 B+ tree** per disk, rooted in its disk descriptor, keyed by
`bucket_no: u32`. Sequential bucket numbers compress to ~2 B per key with the packed-key codec
(BTREE-DESIGN§1.6), so a 16 M-bucket disk fits in **depth 1**:

```rust
#[repr(C)]
struct BucketAllocEntry {                    // 8 bytes
    generation: u32,                         // [0..4]  monotonic; matched by BlockRef.generation
    kind: u8,                                // [4..5]  BucketKind
    flags: u8,                               // [5..6]  BUCKET_FLAG_*
    live_sectors: u16,                       // [6..8]  live 4 KiB sectors in this bucket
}

// BucketAllocEntry.flags bits
const BUCKET_FLAG_NEEDS_DISCARD: u8 = 1 << 0;  // freed, TRIM not yet issued (ALLOCATION-DESIGN§1.7)

#[repr(u8)]
enum BucketKind {
    Free      = 0,                           // not in use; freespace LRU candidate
    Journal   = 1,                           // journal buckets (JOURNAL-DESIGN§1.1)
    Node      = 2,                           // B+ tree nodes and positional regions (BTREE-DESIGN§1)
    Data      = 3,                           // object extents and chunks (BLOB-DESIGN)
    Packed    = 4,                           // small-object blocks (BLOB-D4)
    Stripe    = 5,                           // erasure-coding stripes (future)
    Reserved  = 6,                           // copygc forward-progress reserve
}
```

**Live-sector accounting.** `live_sectors` changes only through entries that already exist for
other reasons, so replay recomputes it without accounting entries of its own:

- data: `WriteBlob`, `ExtendObject`, `WriteInline` packing, `ChunkInsertBatch` and chunk-list
  ops add the sectors they reference; deletion reclaim, chunk reference drops and
  `ReconcileMove` subtract or move them (JOURNAL-DESIGN§1.3);
- tree nodes: a checkpoint adds the sectors of the regions its root newly reaches and
  subtracts those of the regions it no longer reaches. Node sectors written after the last
  checkpoint are not live: a crash leaves them unreferenced.

The alloc table is housed in `Node` buckets like every other tree and committed by the same
checkpoint (ENGINE-DESIGN§1.2).

### 1.3 Generation-checked pointers

A `BlockRef` (ENGINE-DESIGN§1.2) is bucket-relative — `(disk_id, sector, bucket_no)` plus the
bucket's `generation` at write time. Dereference protocol:

1. Compare `ref.generation` with the disk's resident generation array at `bucket_no`, loaded
   from the alloc table at mount and maintained by `BucketAlloc` / `BucketGenBump` replay.
2. On mismatch:
   - a pointer reached from the committed root (tree child, content locator, chunk index) is
     **corrupt**: the read fails with `StaleBlockRef` and scrub reports it;
   - a backpointer or a cached-replica reference is **stale**: it is skipped and dropped
     lazily.

Generation comparison also resolves torn-write detection on freed blocks and stale-replica
handling. The per-run and per-block checksums catch in-bucket corruption independently.

### 1.4 Freespace LRU

A second **BTREE-DESIGN§1 B+ tree** per disk, rooted in its disk descriptor, keyed by
`(fragmentation_band, bucket_no)`:

```
fragmentation_band: u8     // ⌈255 × live_sectors / sectors_per_bucket⌉
                           //   = 0   iff live_sectors == 0   (empty / full-free)
                           //   = 255 when live is at or near full (formula saturates
                           //         for the last few sectors because of the ceiling)
                           //   ∈ 1..=254 in between
bucket_no: u32
```

where `sectors_per_bucket = 1 << (Superblock.bucket_size_log2 - 12)` (the count of 4 KiB
sectors in one bucket — 256 for the default 1 MiB bucket, 1024 for the 4 MiB maximum). Only
closed buckets are listed. Band 0 is the allocator's fast path and is reached *only* when
`live_sectors == 0`. Band 255 is the most-fragmented end and absorbs the last sector or two of
occupancy via the ceiling. Bands are recomputed lazily when `live_sectors` crosses an 8-sector
boundary, keeping freespace-LRU churn proportional to allocation pressure rather than to write
volume.

Used by:

- **Foreground allocator**: takes band-0 buckets for new write points.
- **Copy GC**: scans the most-fragmented non-empty buckets to reclaim space (ALLOCATION-DESIGN§1.6).
- **Cache eviction**: cached-replica buckets carry their own LRU, layered on top.

### 1.5 Allocator behaviour

Per-disk **write points** own the open buckets, segregated by:

- **Bucket kind** — journal, node, data and packed buckets never mix.
- **Stream tag** — separate write points for placement-target groups, foreground vs.
  background workloads, per-stream appends (JOURNAL-DESIGN§1.8) and per-tag pinning, so that
  unrelated I/O patterns do not co-mingle and produce correlated fragmentation.

Allocation on the fast path is a bump: the open bucket's write cursor advances by the requested
sectors. When a bucket fills, its write point releases it (it is now closed) and opens a fresh
band-0 bucket; `BucketAlloc` journals the open. After a crash every bucket is closed: space past
an open bucket's last referenced sector is not live and is reclaimed by copy GC with the rest
of the bucket.

### 1.6 Copy GC

Copy GC is one work kind in the reconcile subsystem (RECONCILE-DESIGN§1, `WorkKind::Copygc`). When the
freespace LRU's empty-band count drops below the configured reserve
(`Superblock.copygc_reserve_pct`, default 8 %), reconcile picks the
most-fragmented closed buckets.

Each bucket is processed via the standard move path (RECONCILE-DESIGN§1.5):

1. **Range-scan the backpointers btree (METADATA-DESIGN§2.2) at prefix `(disk_id, bucket_no, *)`** to
   enumerate live extents in the bucket. Entries with stale `bucket_gen` are skipped.
2. Read each live extent from disk and verify it (block checksum; BLAKE3 at the object level if
   the owner is `BlobExtent`).
3. Write to a fresh bucket via the move path.
4. Emit a single `ReconcileMove { from_loc, to_loc, owner_key }` WAL op (JOURNAL-DESIGN§1.3, RECONCILE-DESIGN§1.5). Replay
   projects this into `BackpointerRemove(old_key) ∘ BackpointerInsert(new_key, new_value) ∘
   owner-update` applied as a single LSN's worth of btree mutations.
5. Once a checkpoint has committed every move out of the bucket, bump its generation
   (`BucketGenBump`) and mark it free with `BUCKET_FLAG_NEEDS_DISCARD`. The generation bump
   invalidates any backpointers missed during the scan.

Copy GC cost is proportional to **fragmentation**, not pool size. The bucket-prefix scan is
~1 large-node load per fragmented bucket. The reserve guarantees forward progress: allocation
can never block on GC because at least one fully-empty bucket is always available.

### 1.7 Discard / TRIM

Freed buckets carry `BUCKET_FLAG_NEEDS_DISCARD` until TRIM is issued (mount option
`discard=true|async|off`); a flagged bucket is allocatable, and allocating it clears the flag
without a TRIM. On rotational media no flag is set.

### 1.8 Crash recovery

The bucket alloc table is a B+ tree under the standard COW + journal commit machinery (ENGINE-DESIGN§1.2).
On unclean shutdown:

1. Each disk's alloc-table root is loaded from the committed root.
2. Journal replay applies `BucketAlloc`, `BucketGenBump` and `BucketDiscard` in LSN order and
   recomputes `live_sectors` from the data entries after the checkpoint (ALLOCATION-DESIGN§1.2).
3. Every bucket is closed; the resident generation arrays are rebuilt from the result.

Recovery is proportional to journal size, not pool size.

---
