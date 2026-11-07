# Metadata tables — open decisions

Design: [metadata-design.md](metadata-design.md) · Plan: [metadata-implementation-plan.md](metadata-implementation-plan.md)

## METADATA-D3 — When index cleanup runs

SPECIFICATION-DESIGN§7.1 puts index cleanup (phase 2) in a background task after the tombstone. `Engine::delete_object` runs it synchronously in the same call, which costs O(assertions on the object) — microseconds for typical objects — and keeps tombstoned objects out of every index without a query-time state check. Options: (a) keep it synchronous and amend §7.1; (b) defer it to a reconcile work item, so queries must filter tombstoned ids until it runs. Shapes [METADATA6](metadata-implementation-plan.md).

## METADATA-D4 — Content stored inside the record

`ContentKind` 5 is reserved for content held in the record's 40-byte locator itself. Objects of at most 40 bytes (markers, tiny settings values, empty-ish files) would need no blob space, no packed slot and no backpointer, and be read with the record. Options: (a) adopt it — `Inline = 5`, `length ≤ 40`, `stored_length = length`, no compression; (b) leave such objects to packed small-object slots (BLOB-D4) and keep the kind unassigned. (a) adds one read-path branch and makes the record leaf carry user bytes (encryption then has to cover the locator, SPECIFICATION-DESIGN§9.3). Shapes [METADATA7](metadata-implementation-plan.md).

## METADATA-D5 — What `modified_ns` tracks

Assertion changes no longer touch the object record, so `modified_ns` changes on content writes and state changes only, like a Unix mtime. A change time covering tags and attributes (like ctime, which FUSE `getattr` reports) then has no home. Options: (a) mtime only; FUSE reports ctime = mtime; (b) add `assertions_modified_ns` to `ForwardHead` (INDEX-DESIGN§2.1, +8 B per object), updated with the forward entry; (c) derive ctime from the newest `last_modify_lsn` of the object's forward entry through the journal's timestamp of that LSN, with no stored field. Shapes [METADATA7](metadata-implementation-plan.md) and [UNIX-D4](unix-decisions.md).

## Future work

- None recorded.
