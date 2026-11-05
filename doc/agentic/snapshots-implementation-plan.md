# Snapshots — implementation plan

Design: [snapshots-design.md](snapshots-design.md) · Decisions: [snapshots-decisions.md](snapshots-decisions.md) · Order: [operations-plan.md](operations-plan.md)

Prerequisites for the whole scope: [ENGINE3](engine-implementation-plan.md), [METADATA2](metadata-implementation-plan.md), [INDEX5](index-implementation-plan.md), [BLOB2](blob-implementation-plan.md).

- [ ] **SNAPSHOT1 — Snapshot tree.** `SnapshotNode`, ancestor bitmap and skiplist (SNAPSHOTS-DESIGN§1.1).
- [ ] **SNAPSHOT2 — Snapshot-aware key positions.** Thread the snapshot id through every snapshot-aware tree (SNAPSHOTS-DESIGN§1.2).
- [ ] **SNAPSHOT3 — History sidecars.** Populate `ObjectHistory` and `LocationHistory`.
- [ ] **SNAPSHOT4 — Visibility, retention and rollback.** SNAPSHOTS-DESIGN§1.3, SNAPSHOTS-DESIGN§1.6, SNAPSHOTS-DESIGN§1.7, SNAPSHOTS-DESIGN§1.8.
- [ ] **SNAPSHOT5 — Snapshot WAL ops.** Emit and replay `SnapshotCreate`, `SnapshotDelete`, `SnapshotUnlink`, `SnapshotDepthUpdate` (currently replay-skipped).
- [ ] **SNAPSHOT6 — Snapshot API and queries.** `DiskEngine::snapshot_create` (currently `NotImplemented`), queries with `snapshot_id`, blob reads resolved from the snapshot. Acceptance: end-to-end test creating a snapshot, mutating, and querying the snapshot returns the old graph and old blob bytes ([acceptance scenario](operations-design.md#acceptance-scenarios)).
