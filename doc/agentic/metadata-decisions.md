# Metadata tables — open decisions

Design: [metadata-design.md](metadata-design.md) · Plan: [metadata-implementation-plan.md](metadata-implementation-plan.md)

## METADATA-D3 — When index cleanup runs

SPECIFICATION-DESIGN§7.1 puts index cleanup (phase 2) in a background task after the tombstone. `Engine::delete_object` runs it synchronously in the same call, which costs O(assertions on the object) — microseconds for typical objects — and keeps tombstoned objects out of every index without a query-time state check. Options: (a) keep it synchronous and amend §7.1; (b) defer it to a reconcile work item, so queries must filter tombstoned ids until it runs. Shapes [METADATA6](metadata-implementation-plan.md).

## Future work

- None recorded.
