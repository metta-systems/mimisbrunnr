# Indices — implementation plan

Design: [index-design.md](index-design.md) · Decisions: [index-decisions.md](index-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Remaining native layouts

- [ ] **INDEX1 — Four-field range key.** `RangeIndexKey { attr_id, value: NormalisedKey, oid, snapshot }` with a `PackableKey` impl and the value form of INDEX-DESIGN§4.2, replacing the current `(tag_id, norm_key, snapshot)` key with oids in a bitmap value. Acceptance: packed footprint 8–12 B per key; a range scan over `(attr_id, low, high)` returns the same oids as before; `RangeIndexValue` / `RoaringBitmapSerde` proxies removed.
- [ ] **INDEX2 — Tag index `OrderedStore`.** 4 KiB `SequencePage` envelope and chain (INDEX-DESIGN§3); flush no longer returns `UnsupportedStoreKind` for ordered tags.
- [ ] **INDEX3 — Tag index `RankedStore`.** 4 KiB `RankedPage` envelope and chain; flush no longer returns `UnsupportedStoreKind` for ranked tags.
- [ ] **INDEX7 — Value-spill table.** Values over the inline limit spill into the `BtreeKind::ValueSpill` tree keyed by `value_hash` (INDEX-DESIGN§1.1), rooted at `RootPointer.value_spill_root`; the forward index recovers an `Attr`'s full `Value` through it instead of the `Value::Int(0)` placeholder. Acceptance: an attribute with a 200-byte text value round-trips through the forward index after reopen.

- [ ] **INDEX9 — Positional `Value` encoding.** `crates/types` encodes `Value` as INDEX-DESIGN§1 pins (`[tag, payload]`, `[5, context, inner]`, shortest integers, 64-bit floats) with hand-written CBOR in place of the derived `Serialize`, and computes `value_hash` over that encoding (INDEX-DESIGN§1.2); the same module provides the positional-CBOR helpers (array framing, variant index, leading schema version, defaults for missing trailing fields) that FORMAT-DESIGN§1.6 records use. Acceptance: fixed expected bytes for one value of every variant and for nested-`Scoped` rejection; `value_hash` matches recorded vectors; KV lookups and forward-index attributes round-trip through reopen.

## Capacity

- [ ] **INDEX8 — Tag bitmaps beyond one page.** A tag whose roaring image exceeds one `TagBitmapPage` (4 036 bytes) is written as a `next_page` chain (INDEX-DESIGN§3.2) instead of failing with `TagBitmapPageError::Oversize`. Acceptance: a tag on 20 000 objects with scattered ids round-trips through flush and reopen; a corrupted middle page fails with the defined error.
- [ ] **INDEX4 — KV index full capacity.** Live bucket split on overflow, directory doubling, spillover region for `global_depth ≥ 8`, bitmap pages allocated outside the directory region (current practical cap ≈ 62 entries at `global_depth = 0`). Prerequisite: [ALLOCATION1](allocation-implementation-plan.md). Acceptance: 1 000 entries grow the directory monotonically; a split re-inserts correctly; `global_depth ≥ 8` allocates spillover; every bitmap survives reload.

## Bitmap representation

- [ ] **INDEX5 — Full-width ObjectIds in bitmaps.** Apply the representation chosen in INDEX-D1 across index, query (`evaluate_full`), fuse (faceted readdir) and watch (membership bitmaps). Prerequisite: INDEX-D1. Acceptance: objects with `local ≥ 2³²` and objects from two nodes round-trip through a query.

## Verification

- [ ] **INDEX6 — Footprint benchmark.** 1 M chunks, 100 K objects, 10 K tags: index-zone footprint is at least 30 % below a CBOR-envelope baseline and matches the FORMAT-DESIGN§16 estimate. Prerequisites: INDEX1–INDEX3.

Query-driven indices (substring, prefix, per-attribute, reverse relation): [query-implementation-plan.md](query-implementation-plan.md).
