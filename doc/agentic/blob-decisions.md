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

## Future work

- None recorded.
