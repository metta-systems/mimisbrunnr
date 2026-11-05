# Allocation — open decisions

Design: [allocation-design.md](allocation-design.md) · Plan: [allocation-implementation-plan.md](allocation-implementation-plan.md)

## ALLOCATION-D1 — Sealed-bucket state for packed regions

When all sub-extents of a bucket are claimed, the bucket must not be reused until its regions are freed and its generation bumped. Proposed: a new `BUCKET_FLAG_SEALED` whose `dirty_sectors` only decreases. Alternative: derive sealing from `dirty_sectors` and the sub-extent cursor without a new flag. Either way ALLOCATION-DESIGN§1.2 changes. Blocks [ALLOCATION1](allocation-implementation-plan.md).

## Future work

- None recorded.
