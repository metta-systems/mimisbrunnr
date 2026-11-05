# Metadata tables — implementation plan

Design: [metadata-design.md](metadata-design.md) · Decisions: [metadata-decisions.md](metadata-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Radix trees

- [ ] **METADATA1 — Multi-level object table.** Inner nodes, descent from `RootPointer.object_table_root`, COW flush of dirty leaves and ancestors, tree growth; lifts `MetaError::OidOutOfRange` at `oid_local ≥ 2044`. Prerequisites: [ALLOCATION1](allocation-implementation-plan.md), [BTREE4](btree-implementation-plan.md), [ENGINE3](engine-implementation-plan.md). Acceptance: 5 000 records across the oid space round-trip; on-disk leaf matches spec offsets; filling leaf 0 then writing a far oid grows `root_level`; occupancy bits for oids 5 and 2043 in one leaf survive reload; lookup resolves by radix descent.
- [ ] **METADATA2 — Multi-level location table.** Same as METADATA1 for `LocationTable` (leaf fanout 5 440, `OidOutOfRange` at 5 440). Prerequisite: METADATA1.

## Wire-form cleanup

- [ ] **METADATA3 — Packed-key backpointer table.** `BackpointerKey` / `BackpointerValue` persist through the packed-key codec; their manual byte-image `Serialize` impls go away.
- [ ] **METADATA4 — Remove CBOR proxies.** Drop `Serialize` / `Deserialize` on `ObjectRecord` and `ObjectLocation`, the `ObjectTableKey` / `ObjectTableValue` / `LocationTableKey` / `LocationTableValue` proxies, and `crates/meta/src/serde_pod_bytes.rs`. Prerequisites: METADATA2, METADATA3, [ALLOCATION6](allocation-implementation-plan.md). Acceptance: no `serialize_bytes(bytemuck::bytes_of` remains in `crates/`.

## Overflow records

- [ ] **METADATA5 — Overflow record encode, decode and chain walk.** Payload encoder and decoder, the `OverflowAttr` variants (inline and spilled), and chain navigation across 4 KiB blocks with `BLOCK_FLAG_CONTINUATION` (METADATA-DESIGN§1.2), so objects with more than four tags or any attributes keep them in their record's overflow chain. Prerequisites: [SPEC3](specification-implementation-plan.md), [ALLOCATION1](allocation-implementation-plan.md). Acceptance: an object with 20 tags and inline and spilled attributes round-trips through write, reopen and read.

## Deletion

- [ ] **METADATA6 — Blob reclaim and tombstone expiry.** Deletion keeps the location record until reclaim; reclaim moves the record to `BlobReclaim`, frees direct extents or decrements chunk references and clears the location in one commit; expiry moves it to `Cleared` and zeroes the rest (METADATA-DESIGN "Object deletion"). On a single node the peer-watermark condition is vacuous and only the grace period applies. Prerequisites: [BLOB1](blob-implementation-plan.md), [ENGINE5](engine-implementation-plan.md), METADATA-D3. Acceptance: delete an object, reopen: it is `Tombstoned` with its extents still allocated; after the grace period reclaim frees them and expiry clears the record; the id is never handed out again.

Backpointer engine wiring: [ENGINE5](engine-implementation-plan.md). Replica population: [BLOB5](blob-implementation-plan.md).
