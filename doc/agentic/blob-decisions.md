# Blob storage and content paths — open decisions

Design: [blob-design.md](blob-design.md) · Plan: [blob-implementation-plan.md](blob-implementation-plan.md)

## BLOB-D1 — LZ4 compression

LZ4 needs a new workspace dependency (`lz4_flex`). Decide whether to add LZ4 next to zstd. Blocks [BLOB7](blob-implementation-plan.md).

## BLOB-D2 — Compression granularity

A compressed extent holds at most `encoded_extent_max` plaintext bytes (pool-wide, default 128 KiB) and is decoded whole, so the extent is the compression unit and needs no frame table (METADATA-DESIGN§1.3); a chunk is decoded whole too, its size set by the chunking parameters. Still open: whether the cap should vary per storage class (smaller for random-access databases, larger for archives) instead of one pool-wide value, and the on-disk framing of a compressed extent (algorithm id, stored length, any header) beyond what the record's `compression` field and the `ExtentEntry` already say. Blocks [SPEC7](specification-implementation-plan.md).

## BLOB-D3 — Compression on the device

Objects are stored uncompressed until this is decided. Writing on the device (game saves) needs compression and decompression in `no_std` + `alloc` on a soft-float `aarch64` target without NEON; `zstd` binds the C library. Investigate candidates for ratio, speed on that target (or QEMU `raspi3b`), memory use and `no_std` support in both directions:

- zstd: `ruzstd` (pure Rust; decoder mature, encoder limited) versus the C library on the host.
- LZ4: `lz4_flex` (pure Rust, `no_std` with `alloc`, both directions; see BLOB-D1).
- Others with `no_std` encoders and decoders (for example `miniz_oxide` / DEFLATE).

The outcome sets which algorithms the host writes, which the device reads and writes, and whether one algorithm serves both. Shapes BLOB-D1 and [BLOB7](blob-implementation-plan.md).

## BLOB-D4 — Small-object bucket layout

Tiny objects are packed into small-object buckets at checkpoint (JOURNAL-DESIGN§1.8). Decide the layout: (a) slotted 4 KiB blocks — a slot directory of `(offset, length)` per block, objects addressed as `(block, slot)`, freed slots reused when the block is rewritten; (b) size classes — fixed-size slots per bucket (64 B, 128 B, … 2 KiB), addressed by index, with a free bitmap; (c) append-only log buckets compacted by copy GC like other buckets. The choice sets the `Packed` content locator (at most 40 bytes for all replicas, each with its 16-bit generation tag, METADATA-DESIGN§1.2 and METADATA-DESIGN§2.1) and how space is reclaimed on delete. Whatever the layout, each packed block names the `ObjectId` of each of its slots, so one `OwnerKind::PackedBlock` backpointer per block lets copygc find and update every object in it (METADATA-DESIGN§2.2). Blocks [BLOB8](blob-implementation-plan.md).

## BLOB-D5 — Content hash of chunked objects

`content_hash` is BLAKE3 of the whole plaintext (METADATA-DESIGN§1.1). For a chunked object an in-place edit (JOURNAL-DESIGN§1.3.1) changes the hash, and recomputing it means reading the whole object — 32 GiB for a 4 KiB edit of a VM image. Direction under consideration: a Merkle-style hash for chunked objects, computed over the ordered chunk hashes (and lengths) instead of the bytes, so an edit recomputes it from the `ChunkList` keys alone. Open: the exact construction (flat hash over the chunk list, or a tree so a range edit updates O(log n) nodes), whether exact-content deduplication (METADATA-D6) and sync compare it with byte hashes of non-chunked objects (the same bytes chunked differently would hash differently), and whether non-chunked objects keep the byte hash. Blocks the hash maintenance of `ChunkListReplace` and `ChunkListShrink`.

## BLOB-D6 — Re-chunk window size

Re-chunking walks an object in windows ending on old chunk boundaries (INDEX-DESIGN§4.3), so the last new chunk of each window is cut at the window edge instead of where the chunker would cut it, losing a little deduplication per window; a window is also what the concurrent-write check redoes. Larger windows mean fewer forced cuts and longer redos. Options: a pool-state value (e.g. 64 MiB default), a fixed multiple of the new `max_size`, or left to the implementation and not pinned. Shapes [BLOB3](blob-implementation-plan.md).

## BLOB-D7 — Replicas of chunks

A `ChunkIndex` entry names one location (`blob: BlobRef`, INDEX-DESIGN§4.3), so every chunk has exactly one copy: a `Replicate` rule asking for more does nothing for a chunked object, the replica ops (`ReconcileReplicate`, `ReconcileDropReplica`) do not apply to chunks (RECONCILE-DESIGN§1.5), and losing the disk holding a chunk breaks every object that uses it — through deduplication, possibly many. Non-chunked extents and packed blocks do not have this gap.

The difficulty is sharing: one chunk serves objects with different policies, so its copy count must satisfy the most demanding of them, and can only fall when that object goes.

Options:

- (a) **A replica set per `ChunkIndex` entry** — `replica_count`, up to four `ReplicaRef`s and their generation tags in place of the single `BlobRef` (the entry grows from 56 to about 88 bytes). A chunk's target count is the highest `Replicate` requirement among the objects referencing it: raised when a more demanding object starts using it, lowered by a reconcile pass when one stops; the replica ops then apply to `OwnerKind::Chunk` like any owner.
- (b) **One pool-wide copy count for all chunks** — every chunk gets the pool's chunk replication factor regardless of the objects using it. No per-chunk policy tracking, but over-replicates chunks only cheap objects use, and still needs (a)'s replica set in the entry.
- (c) **Redundancy below the chunk** — chunks live in erasure-coded or mirrored buckets (`BucketKind::Stripe`), so the single `BlobRef` names a location the bucket layer already protects. Keeps the entry, but depends on an erasure-coding design that does not exist yet.

Blocks correct `Replicate` behaviour for chunked objects ([BLOB3](blob-implementation-plan.md), [RECONCILE4](reconcile-implementation-plan.md)).

## Future work

- None recorded.
