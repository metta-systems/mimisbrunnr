# Blob zone and content paths — open decisions

Design: [blob-design.md](blob-design.md) · Plan: [blob-implementation-plan.md](blob-implementation-plan.md)

## BLOB-D1 — LZ4 compression

LZ4 needs a new workspace dependency (`lz4_flex`). Decide whether to add LZ4 next to zstd. Blocks [BLOB7](blob-implementation-plan.md).

## BLOB-D2 — Compression granularity

SPECIFICATION-DESIGN§9.2 says compression applies from file to block granularity without defining how the unit is chosen or framed on disk. Decide the unit per storage class and its framing. Blocks [SPEC7](specification-implementation-plan.md).

## BLOB-D3 — Compression on the device

Objects are stored uncompressed until this is decided. Writing on the device (game saves) needs compression and decompression in `no_std` + `alloc` on a soft-float `aarch64` target without NEON; `zstd` binds the C library. Investigate candidates for ratio, speed on that target (or QEMU `raspi3b`), memory use and `no_std` support in both directions:

- zstd: `ruzstd` (pure Rust; decoder mature, encoder limited) versus the C library on the host.
- LZ4: `lz4_flex` (pure Rust, `no_std` with `alloc`, both directions; see BLOB-D1).
- Others with `no_std` encoders and decoders (for example `miniz_oxide` / DEFLATE).

The outcome sets which algorithms the host writes, which the device reads and writes, and whether one algorithm serves both. Shapes BLOB-D1 and [BLOB7](blob-implementation-plan.md).

## BLOB-D4 — Small-object bucket layout

Tiny objects are packed into small-object buckets at checkpoint (JOURNAL-DESIGN§1.8). Decide the layout: (a) slotted 4 KiB blocks — a slot directory of `(offset, length)` per block, objects addressed as `(block, slot)`, freed slots reused when the block is rewritten; (b) size classes — fixed-size slots per bucket (64 B, 128 B, … 2 KiB), addressed by index, with a free bitmap; (c) append-only log buckets compacted by copy GC like other buckets. The choice sets `ObjectLocation`'s form for packed objects and how space is reclaimed on delete. Blocks [BLOB8](blob-implementation-plan.md).

## Future work

- None recorded.
