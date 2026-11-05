# Indices — open decisions

Design: [index-design.md](index-design.md) · Plan: [index-implementation-plan.md](index-implementation-plan.md)

## INDEX-D1 — Bitmap representation for 64-bit ObjectIds

`roaring::RoaringBitmap` is 32-bit while `ObjectId` is 16-bit node + 48-bit local. Today bitmaps hold the low 32 bits of `local` and readers rebuild ids with `node = 0` (for example `crates/query/src/executor.rs`). Options:

- **(a)** `croaring` 64-bit bitmaps (one dependency change, C library).
- **(b)** Per-node sharding: `HashMap<NodeId, RoaringBitmap>`, keeping 32-bit bitmaps per node prefix.
- **(c)** A 48-bit wrapper over `roaring::RoaringTreemap` (high 32 bits keyed, 16 + 32 split).

The choice changes every persisted bitmap's on-disk form (INDEX-DESIGN§3.2). Blocks [INDEX5](index-implementation-plan.md).

## Future work

- None recorded.
