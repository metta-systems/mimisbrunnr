# Snapshots — open decisions

Design: [snapshots-design.md](snapshots-design.md) · Plan: [snapshots-implementation-plan.md](snapshots-implementation-plan.md)

## SNAPSHOT-D1 — Snapshotting strategy, and snapshots during a rule rework

A large rule change runs as a resumable rework, and while any rule is `installing` or `removing` `SnapshotCreate` fails with a defined error (INDEX-DESIGN§2.4, SNAPSHOTS-DESIGN§1.4). On a large pool the rework can run for a long time, and every snapshot taken automatically fails for all of it: retention schedules (SNAPSHOTS-DESIGN§1.7), backups, and cluster sync if it uses snapshots (CLUSTER-D7). The design has no overall snapshotting strategy that says who takes snapshots, how often, and what may delay or refuse them.

To decide:

- **A snapshotting strategy:** which subsystems create snapshots (users, retention schedules, backups, sync), at what cadence, and how a refused or late snapshot is handled for each — retried, skipped, or queued.
- **Whether the ban stays.** Alternatives:
  - (a) keep the ban, and have automatic requests **wait** for the rework to finish instead of failing;
  - (b) allow snapshots during a rework: a snapshot records the rules as of its creation and its pending reworks; reads of it apply only active rules, and its postings for the pending rules' terms stay partial until snapshot cleanup or a per-snapshot rework fixes them;
  - (c) bound reworks so the ban is short: pause the rework for a snapshot, then resume.
- **How big a rework realistically is.** The choice depends on measured rework durations for realistic rule changes on large pools — a new `Implies` over a tag on millions of objects, a `Bucket` on a key with millions of values, a rule removal with many suspects — taken by [ONTOLOGY18](ontology-implementation-plan.md).

Affects [ONTOLOGY6](ontology-implementation-plan.md), the retention policy (SNAPSHOTS-DESIGN§1.7) and CLUSTER-D7.

## Future work

- None recorded.
