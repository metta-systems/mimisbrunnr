# Metadata tables — open decisions

Design: [metadata-design.md](metadata-design.md) · Plan: [metadata-implementation-plan.md](metadata-implementation-plan.md)

## METADATA-D1 — Cleared-record accumulation in the object table

Because ObjectIds are never recycled, long-lived pools accumulate leaves that hold only cleared records at the low end of the oid space (SPECIFICATION-DESIGN§6.2). Decide whether a compaction strategy is needed (for example run-length elision of fully cleared leaves as empty children) that keeps positional addressing intact.

## METADATA-D2 — Tag count maintenance under bulk deletion

SPECIFICATION-DESIGN§7.5 bulk deletion does not mention the per-object tag-count field in `ObjectRecord`. Deleting a widely used tag would update that field on every affected record. Decide whether the field is maintained eagerly, lazily (recomputed on read or by reconcile), or removed.

## METADATA-D3 — When index cleanup runs

SPECIFICATION-DESIGN§7.1 puts index cleanup (phase 2) in a background task after the tombstone. `Engine::delete_object` runs it synchronously in the same call, which costs O(assertions on the object) — microseconds for typical objects — and keeps tombstoned objects out of every index without a query-time state check. Options: (a) keep it synchronous and amend §7.1; (b) defer it to a reconcile work item, so queries must filter tombstoned ids until it runs. Shapes [METADATA6](metadata-implementation-plan.md).

## Future work

- None recorded.
