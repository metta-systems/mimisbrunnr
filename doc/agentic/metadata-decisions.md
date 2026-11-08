# Metadata tables — open decisions

Design: [metadata-design.md](metadata-design.md) · Plan: [metadata-implementation-plan.md](metadata-implementation-plan.md)

## METADATA-D3 — When index cleanup runs

SPECIFICATION-DESIGN§7.1 puts index cleanup (phase 2) in a background task after the tombstone. `Engine::delete_object` runs it synchronously in the same call, which costs O(assertions on the object) — microseconds for typical objects — and keeps tombstoned objects out of every index without a query-time state check. Options: (a) keep it synchronous and amend §7.1; (b) defer it to a reconcile work item, so queries must filter tombstoned ids until it runs. Shapes [METADATA6](metadata-implementation-plan.md).

## METADATA-D6 — Finding objects by content hash

Import reuses an object whose content already exists (UNIX-DESIGN "Import"), which needs objects found by `content_hash`; only chunks are indexed by hash (INDEX-DESIGN§4.3). Options: (a) a `ContentHash` tree keyed by a hash prefix, valued by object ids, maintained on content writes and deletion — about 16–24 bytes per object; (b) no index: deduplicate within one import run only, and across the pool through chunking where policy chunks objects; (c) a record-field column usable as a query filter, with no lookup structure — a scan. Shapes [UNIX10](unix-implementation-plan.md).

## Future work

- Full 32-bit generations on content extents in place of the 16-bit tags (METADATA-DESIGN§2.1): a 12-byte `ReplicaRef`, so `ExtentLocator` holds three replicas in 40 bytes and `ExtentEntry` grows to 56 bytes. Worth it if the extent layouts change for another reason and free bytes appear, or if size evaluations show the wider records are affordable.
