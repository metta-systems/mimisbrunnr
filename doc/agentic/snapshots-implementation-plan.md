# Snapshots — implementation plan

Design: [snapshots-design.md](snapshots-design.md) · Decisions: [snapshots-decisions.md](snapshots-decisions.md) · Order: [operations-plan.md](operations-plan.md)

Prerequisites for the whole scope: [ENGINE3](engine-implementation-plan.md), [METADATA1](metadata-implementation-plan.md), [INDEX13](index-implementation-plan.md), [BLOB2](blob-implementation-plan.md).

- [ ] **SNAPSHOT1 — Snapshot tree.** `SnapshotNode`, ancestor bitmap and skiplist, labels as the value tail (SNAPSHOTS-DESIGN§1.1). Acceptance: a labelled snapshot's label round-trips; ancestry checks agree with a parent walk for 1 000 random pairs.
- [ ] **SNAPSHOT2 — Snapshot-aware key positions.** Thread the snapshot id through every snapshot-aware tree (SNAPSHOTS-DESIGN§1.2).
- [ ] **SNAPSHOT3 — Record versions and history sidecar.** `ObjectRecord.snapshot`; push-down of a frozen-visible version into `ObjectHistory` on the first write after a snapshot; reads at a snapshot by the SNAPSHOTS-DESIGN§1.2 rule; a version keeps the extents and lists its locator names allocated until it goes. Acceptance: write, snapshot, write twice: one `ObjectHistory` entry; the snapshot reads the first version and the head the third; a head read touches no sidecar block (counted).
- [ ] **SNAPSHOT4 — Visibility, retention and rollback.** SNAPSHOTS-DESIGN§1.3, SNAPSHOTS-DESIGN§1.6 (moves update every version naming the extent), SNAPSHOTS-DESIGN§1.7, SNAPSHOTS-DESIGN§1.8 (re-heading the object table by background scan). Acceptance: copy-GC of an extent shared by the head record and a snapshot version leaves both readable; after a rollback the head reads the target snapshot's records before and after the re-head scan completes.
- [ ] **SNAPSHOT5 — Snapshot WAL ops.** Emit and replay `SnapshotCreate`, `SnapshotDelete`, `SnapshotUnlink`, `SnapshotDepthUpdate` (currently replay-skipped).
- [ ] **SNAPSHOT6 — Snapshot API and queries.** `DiskEngine::snapshot_create` (currently `NotImplemented`), queries with `snapshot_id`, blob reads resolved from the snapshot. Acceptance: end-to-end test creating a snapshot, mutating, and querying the snapshot returns the old graph and old blob bytes ([acceptance scenario](operations-design.md#acceptance-scenarios)).
