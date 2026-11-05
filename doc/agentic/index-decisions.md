# Indices — open decisions

Design: [index-design.md](index-design.md) · Plan: [index-implementation-plan.md](index-implementation-plan.md)

## INDEX-D1 — Bitmap representation for 64-bit ObjectIds

`roaring::RoaringBitmap` is 32-bit while `ObjectId` is 16-bit node + 48-bit local. Today bitmaps hold the low 32 bits of `local` and readers rebuild ids with `node = 0` (for example `crates/query/src/executor.rs`). Options:

- **(a)** `croaring` 64-bit bitmaps (one dependency change, C library).
- **(b)** Per-node sharding: `HashMap<NodeId, RoaringBitmap>`, keeping 32-bit bitmaps per node prefix.
- **(c)** A 48-bit wrapper over `roaring::RoaringTreemap` (high 32 bits keyed, 16 + 32 split).

The choice changes every persisted bitmap's on-disk form (INDEX-DESIGN§3.2). Blocks [INDEX5](index-implementation-plan.md).

## INDEX-D3 — Equality lookups under hash collisions

The format identifies an attribute value by its `value_hash` throughout: a KV bucket entry is `(tag_id, value_hash, bitmap_ref)` (INDEX-DESIGN§4.1), a forward-index `PackedAssertion` stores `value_hash` in place of the value (INDEX-DESIGN§2.1), and the value table that recovers a value is keyed by `value_hash` (INDEX-DESIGN§4.1, INDEX7). Two different values with the same hash therefore share one KV entry (a query for either returns both sets of objects), share one value-table entry (one of them reads back as the other), and are indistinguishable in forward entries. INDEX-DESIGN§1.1 also says values up to 96 bytes are inline "in the index leaf", which neither the KV bucket nor `PackedAssertion` has room for.

Likelihood: accidental collisions among *n* distinct values occur with probability about n²/2⁶⁵ (≈ 3 × 10⁻⁶ at 10 million values). A deliberate collision with a chosen victim value needs about 2⁶⁴ SipHash evaluations *and* the pool secret, which sits in the superblock (INDEX-DESIGN§1.2), so it requires raw read access to the device.

Options:

- **(a) Verify candidates on lookup.** Not possible with the specified format: forward entries hold only the hash. It would need each object's actual value stored per object (16 B `PackedAssertion` grows to hold a value or a reference), and costs one forward-index probe per candidate — microseconds warm, a sector read each cold (≈0.3–1 ms on an SD card, so ≈30–100 ms for 100 candidates).
- **(b) Values in KV entries.** A KV bucket entry grows from 28 B to 28 B plus the value's encoding (typically 16–40 B), so a 4 KiB bucket holds about 60–80 entries instead of 144 (roughly one more level of directory depth), bucket entries become variable-length, and a value over the inline limit needs a second read to compare. Lookups stay one bucket read. Fixes equality queries only; the value table and forward entries remain hash-identified.
- **(c) Collision flag.** Setting the flag at insert needs a witness of the existing value, i.e. (b)'s stored value or a probe of the value table; the read path then verifies flagged entries only, but by (a)'s mechanism, which the format does not support.
- **(d) Hash as identity, enforced on write.** Every distinct value is stored once in the value table, keyed by `value_hash` (the inline/spill split of INDEX-DESIGN§1.1 becomes the table's inline/out-of-line value storage). Writing an attribute probes the table for its hash — the write path inserts there anyway — and a different stored value under that hash is a collision: the write fails with a defined error (or, in a later variant, the value is stored under the next free secondary hash, with the probe sequence specified). Reads never verify: a hash names exactly one value pool-wide. Cost: one value-table probe per attribute write introducing a new hash; nothing on reads; storage of one copy per distinct value, which display and faceting need regardless.
- **(e) 128-bit `value_hash`.** Makes collisions infeasible without changing the model; costs 8 more bytes in every KV entry and every `PackedAssertion` (16 → 24 B, about +50 % forward-index size).

Blocks [INDEX11](index-implementation-plan.md); resolves the INDEX-DESIGN§1.1 inline-value contradiction together with [INDEX7](index-implementation-plan.md).

## Future work

- None recorded.
