# Blob storage and content paths — open decisions

Design: [blob-design.md](blob-design.md) · Plan: [blob-implementation-plan.md](blob-implementation-plan.md)

## BLOB-D1 — LZ4 compression

LZ4 needs a new workspace dependency (`lz4_flex`). Decide whether to add LZ4 next to zstd. Blocks [BLOB7](blob-implementation-plan.md).

## BLOB-D2 — Compression granularity

A compressed chunk holds at most `encoded_extent_max` plaintext bytes (pool-wide, default 128 KiB) and is decoded whole, so the chunk is the compression unit and needs no frame table (METADATA-DESIGN§1.3, the content map's size limits). Still open: whether the cap should vary per storage class (smaller for random-access databases, larger for archives) instead of one pool-wide value, and the on-disk framing of a compressed chunk (algorithm id, stored length, any header) beyond what the record's `compression` field and the chunk's `ChunkEntry.stored_length` already say. Blocks [SPEC7](specification-implementation-plan.md).

## BLOB-D3 — Compression on the device

Objects are stored uncompressed until this is decided. Writing on the device (game saves) needs compression and decompression in `no_std` + `alloc` on a soft-float `aarch64` target without NEON; `zstd` binds the C library. Investigate candidates for ratio, speed on that target (or QEMU `raspi3b`), memory use and `no_std` support in both directions:

- zstd: `ruzstd` (pure Rust; decoder mature, encoder limited) versus the C library on the host.
- LZ4: `lz4_flex` (pure Rust, `no_std` with `alloc`, both directions; see BLOB-D1).
- Others with `no_std` encoders and decoders (for example `miniz_oxide` / DEFLATE).

The outcome sets which algorithms the host writes, which the device reads and writes, and whether one algorithm serves both. Shapes BLOB-D1 and [BLOB7](blob-implementation-plan.md).

## BLOB-D4 — Small-object bucket layout

Tiny objects are packed into small-object buckets at checkpoint (JOURNAL-DESIGN§1.8). Decide the layout: (a) slotted 4 KiB blocks — a slot directory of `(offset, length)` per block, objects addressed as `(block, slot)`, freed slots reused when the block is rewritten; (b) size classes — fixed-size slots per bucket (64 B, 128 B, … 2 KiB), addressed by index, with a free bitmap; (c) append-only log buckets compacted by copy GC like other buckets. The choice sets the `Packed` content locator (at most 40 bytes for all replicas, each with its 16-bit generation tag, METADATA-DESIGN§1.2 and METADATA-DESIGN§2.1) and how space is reclaimed on delete. Whatever the layout, each packed block names the `ObjectId` of each of its slots, so one `OwnerKind::PackedBlock` backpointer per block lets copygc find and update every object in it (METADATA-DESIGN§2.2). Blocks [BLOB8](blob-implementation-plan.md).

## BLOB-D5 — Content hash of objects made of chunks

`content_hash` is BLAKE3 of the whole plaintext (METADATA-DESIGN§1.1). An in-place edit of a `Chunks` object (JOURNAL-DESIGN§1.3.1, the chunk write sequence) changes the hash, and recomputing it means reading the whole object — 32 GiB for a 4 KiB edit of a VM image. Every chunk already has its own hash (METADATA-DESIGN§1.3, `ChunkEntry.hash`), so a Merkle-style hash over the ordered chunk hashes (and lengths) is available for every object and an edit recomputes it from the content-map keys alone. Open: the exact construction (flat hash over the chunk list, or a tree so a range edit updates O(log n) nodes), whether exact-content deduplication (METADATA-D6) and sync compare it with the byte hash of a one-chunk `Extent` object (the same bytes cut differently would hash differently), and whether `Extent` objects keep the byte hash. Blocks the hash maintenance of `ReplaceChunks` and `TruncateChunks`.

**Trust in transfers.** A node fetching an object from a peer (CLUSTER-DESIGN§3, absent chunks recorded from the sender's chunk list) verifies each chunk against its own hash, but the chunk *list* comes from the sending peer. With `content_hash` over the bytes, a wrong or tampered list is caught only after the whole object has been fetched and hashed; with a hash over the ordered chunk hashes and lengths, the receiver checks the list against the synced `content_hash` before fetching anything, and every chunk it then accepts is bound to the object. This argues for the Merkle-style construction for every object, `Extent` objects included.

## BLOB-D6 — Re-cut window size

Re-cutting walks an object in windows ending on old chunk boundaries (INDEX-DESIGN§4.3, chunk boundaries), so the last new chunk of each window is cut at the window edge instead of where the cutter would cut it, losing a little deduplication per window; a window is also what the concurrent-write check redoes. Larger windows mean fewer forced cuts and longer redos. Options: a pool-state value (e.g. 64 MiB default), a fixed multiple of the new `max_size`, or left to the implementation and not pinned. Shapes [BLOB3](blob-implementation-plan.md).

## BLOB-D8 — Chunks shared by very many objects

A deduplicated chunk has one backpointer per owner (METADATA-DESIGN§2.2, backpointers keyed by location and owner), so a chunk shared by N objects has N owners. Moving it (copy GC, evacuation, tiering) or changing its replica count updates every owner's content-map entry under one LSN (RECONCILE-DESIGN§1.5, the move path), and replaying that one small journal entry fans out into N tree mutations. For a chunk common to most of a pool — a licence text, a shared library, a block common to many VM images — N can be in the millions, so one move becomes an unbounded commit and an unbounded replay step. Holes already read as zeros and store nothing, which removes the all-zero chunk. Options:

- (a) **An owner cap per stored copy:** when a chunk's `ChunkIndex` holder (INDEX-DESIGN§4.3, the hash → one live holder lookup used when writing) already has K owners, a new write stores a fresh copy and becomes the holder for later writes, so no copy ever has more than K owners. Bounds every move at K updates; costs one extra copy per K owners of very common content.
- (b) **Indirection for hot chunks:** past K owners, entries point at a shared location record (a small tree keyed by chunk id) instead of naming replicas directly, so a move rewrites one record; reads of such chunks take one extra lookup.
- (c) **Batched moves:** a move of a chunk with many owners runs as a resumable reconcile scan over its backpointers (RECONCILE-DESIGN§1.3, scans with persisted cursors), both locations valid until the scan completes and the old one freed only then. No format change; a move is no longer one commit, and the old copy's space is held until it finishes.

Decide the option and K (or its policy). Shapes [RECONCILE10](reconcile-implementation-plan.md) (background deduplication) and the move path.

## Future work

- None recorded.
