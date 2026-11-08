# Metadata tables — open decisions

Design: [metadata-design.md](metadata-design.md) · Plan: [metadata-implementation-plan.md](metadata-implementation-plan.md)

## METADATA-D3 — When index cleanup runs

SPECIFICATION-DESIGN§7.1 puts index cleanup (phase 2) in a background task after the tombstone. `Engine::delete_object` runs it synchronously in the same call, which costs O(assertions on the object) — microseconds for typical objects — and keeps tombstoned objects out of every index without a query-time state check. Options: (a) keep it synchronous and amend §7.1; (b) defer it to a reconcile work item, so queries must filter tombstoned ids until it runs. Shapes [METADATA6](metadata-implementation-plan.md).

## METADATA-D6 — Finding objects by content hash

Import reuses an object whose content already exists (UNIX-DESIGN "Import"), which needs objects found by `content_hash`; only chunks are indexed by hash (INDEX-DESIGN§4.3). Options: (a) a `ContentHash` tree keyed by a hash prefix, valued by object ids, maintained on content writes and deletion — about 16–24 bytes per object; (b) no index: deduplicate within one import run only, and across the pool through chunking where policy chunks objects; (c) a record-field column usable as a query filter, with no lookup structure — a scan. Shapes [UNIX10](unix-implementation-plan.md).

## Future work

- **A larger `ObjectRecord` for a future format.** A 64-byte locator makes the record 160 bytes (152 used, 8 reserved).
  - Solves without further work: full 32-bit generations on content extents (four `ReplicaRef` + `u32` pairs in 48 bytes) and on list heads (a full `NodePtr` up to R = 3 with the entry count kept in the record, or R = 4 without it), replacing the 16-bit tags (METADATA-DESIGN§2.1); inline objects up to 64 bytes; room to spare for the packed locator (BLOB-D4).
  - Costs: 25 records per 4 KiB page instead of 32 (96 bytes lost per page) and 1 200 per leaf instead of 1 536, changing the radix arithmetic (METADATA-DESIGN§1.4); about +0.3 GiB of object table at 10 M objects, held by every node and R times on each; 22 % fewer records per cached page for scans and per-object evaluation. Every object pays, while the gains go mostly to large objects and to closing the 16-bit tag compromise.
  - A 192-byte record (three cache lines) is the other round size: about +0.6 GiB at 10 M objects, with 32 bytes reserved that nothing uses yet.

- Full 32-bit generations on content extents in place of the 16-bit tags (METADATA-DESIGN§2.1): a 12-byte `ReplicaRef`, so `ExtentLocator` holds three replicas in 40 bytes and `ExtentEntry` grows to 56 bytes. Worth it if the extent layouts change for another reason and free bytes appear, or if size evaluations show the wider records are affordable.
