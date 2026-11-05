# Indices — open decisions

Design: [index-design.md](index-design.md) · Plan: [index-implementation-plan.md](index-implementation-plan.md)

## INDEX-D1 — Bitmap representation for 64-bit ObjectIds

`roaring::RoaringBitmap` is 32-bit while `ObjectId` is 16-bit node + 48-bit local. Today bitmaps hold the low 32 bits of `local` and readers rebuild ids with `node = 0` (for example `crates/query/src/executor.rs`). Options:

- **(a)** `croaring` 64-bit bitmaps (one dependency change, C library).
- **(b)** Per-node sharding: `HashMap<NodeId, RoaringBitmap>`, keeping 32-bit bitmaps per node prefix.
- **(c)** A 48-bit wrapper over `roaring::RoaringTreemap` (high 32 bits keyed, 16 + 32 split).

The choice changes every persisted bitmap's on-disk form (INDEX-DESIGN§3.2). Blocks [INDEX5](index-implementation-plan.md).

## INDEX-D2 — Where the `value_hash` secret lives

`value_hash` is keyed by a 16-byte per-pool secret (INDEX-DESIGN§1.2), fixed for the pool's lifetime because hashes are persisted, and needed by every reader that looks up a value, the device included. Today every call site passes zeros, and `KvIndex` carries the secret as a serialised field. The spec's "superblock-adjacent metadata" is not defined. Options:

- **(a)** A field of `PoolStateRoot` (POOL-DESIGN§1): one pool-wide copy, committed with the root.
- **(b)** A reserved 16-byte field in every member's superblock, identical across members: readable before anything else mounts, but repeated per disk.
- **(c)** (a) or (b) in plaintext for unencrypted pools; for encrypted pools the stored value is wrapped by a pool-level key of the key hierarchy (ENCRYPTION-DESIGN), never by a per-disk `DiskKey`, since one secret serves a pool-wide index.

Generation: random bytes at format time from the host's RNG, or from entropy the embedder supplies on the device. Blocks [INDEX10](index-implementation-plan.md); shapes [ENCRYPTION6](encryption-implementation-plan.md).

## INDEX-D3 — Equality lookups under hash collisions

The KV index keys entries by `(tag, value_hash)` and stores only a bitmap, and `QueryExecutor` returns that bitmap for `HasAttr { op: Eq }` unchecked (`crates/query/src/executor.rs`). Two different values with the same 64-bit hash share one entry, so a query for either returns both sets of objects. Accidental collisions are negligible; deliberate ones are not when the secret is known (today it is zero). Options:

- **(a) Verify on lookup.** Every candidate's forward entry is checked for an `Attr` equal to the queried value; cost is one forward-index probe per candidate.
- **(b) Keep values apart in the index.** KV entries carry the value (inline up to the INDEX-DESIGN§1.1 limit, or its value-spill reference), colliding values get distinct entries in the same bucket, and lookup compares values; no per-candidate work, larger entries.
- **(c) Verify only when needed.** (b)'s entries carry a collision flag set when a second distinct value lands on the same key; lookups verify as in (a) only for flagged entries.

Blocks [INDEX11](index-implementation-plan.md).

## Future work

- None recorded.
