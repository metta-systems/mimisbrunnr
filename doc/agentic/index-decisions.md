# Indices — open decisions

Design: [index-design.md](index-design.md) · Plan: [index-implementation-plan.md](index-implementation-plan.md)

## INDEX-D1 — Bitmap representation for 64-bit ObjectIds

`roaring::RoaringBitmap` is 32-bit while `ObjectId` is 16-bit node + 48-bit local. Today bitmaps hold the low 32 bits of `local` and readers rebuild ids with `node = 0` (for example `crates/query/src/executor.rs`). Options:

- **(a)** `croaring` 64-bit bitmaps (one dependency change, C library).
- **(b)** Per-node sharding: `HashMap<NodeId, RoaringBitmap>`, keeping 32-bit bitmaps per node prefix.
- **(c)** A 48-bit wrapper over `roaring::RoaringTreemap` (high 32 bits keyed, 16 + 32 split).

The choice changes every persisted bitmap's on-disk form (INDEX-DESIGN§3.2). Blocks [INDEX5](index-implementation-plan.md).

## Future work

- **128-bit `value_hash`, to test.** Widening the hash makes collisions infeasible even with the secret known, so the write-time collision error (INDEX-DESIGN§1.1) could never fire. Cost to measure: 8 more bytes per KV bucket entry (28 → 36 B, 144 → 112 entries per bucket) and per forward-index `PackedAssertion` (16 → 24 B, about +50 % forward-index size), against the collision rate actually seen.
- **Secondary hash slots on collision.** Instead of rejecting a colliding write, store the value under the next free hash in a specified probe sequence, so the write succeeds.
