# Journal (WAL) — open decisions

Design: [journal-design.md](journal-design.md) · Plan: [journal-implementation-plan.md](journal-implementation-plan.md)

## JOURNAL-D1 — Journal mirroring across hot disks

JOURNAL-DESIGN§1.7 defines the mechanism — the same jset written to a journal bucket on each hot disk, merged by `seq` at recovery. Decide the policy: synchronous mirroring with an acknowledgement count (and its default), a primary plus an asynchronous mirror, or no mirroring at all, leaving redundancy to cluster replication. Blocks [JOURNAL6](journal-implementation-plan.md).

## Future work

- None recorded.
