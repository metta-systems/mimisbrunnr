# Journal (WAL) — open decisions

Design: [journal-design.md](journal-design.md) · Plan: [journal-implementation-plan.md](journal-implementation-plan.md)

## JOURNAL-D1 — WAL mirroring across hot disks

SPECIFICATION-DESIGN§6.4 describes mirroring the WAL across hot disks; JOURNAL-DESIGN§1 defines a single-disk ring. Decide the mirroring model (synchronous dual append, primary plus asynchronous mirror, or deferring mirroring to cluster replication) and the recovery rule when mirrors disagree. Blocks [JOURNAL6](journal-implementation-plan.md).

## Future work

- None recorded.
