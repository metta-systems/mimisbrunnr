# Reconcile — implementation plan

Design: [reconcile-design.md](reconcile-design.md) · Decisions: [reconcile-decisions.md](reconcile-decisions.md) · Order: [operations-plan.md](operations-plan.md)

Prerequisites for the whole scope: [ENGINE3](engine-implementation-plan.md), [JOURNAL4](journal-implementation-plan.md), [BLOB2](blob-implementation-plan.md), [ENGINE5](engine-implementation-plan.md).

- [ ] **RECONCILE1 — Work-item trees.** Persist and load the reconcile trees through `RootPointer`.
- [ ] **RECONCILE2 — `reconcile_step` consumer.** Drain work items (currently returns `Ok(0)`) through the move path (RECONCILE-DESIGN§1.5).
- [ ] **RECONCILE3 — Copy GC.** Fragmentation tracking, bucket-fill watermark producer, backpointer-driven evacuation (ALLOCATION-DESIGN§1.6). Prerequisite: [ALLOCATION5](allocation-implementation-plan.md).
- [ ] **RECONCILE4 — Replica repair.** Replica drift producer and resilver.
- [ ] **RECONCILE5 — Autotier.** Tier residence versus placement policy producer and moves.
- [ ] **RECONCILE6 — Snapshot cleanup.** Snapshot deletion scan. Prerequisite: [SNAPSHOT4](snapshots-implementation-plan.md).
- [ ] **RECONCILE7 — Self-healing.** RECONCILE-DESIGN§1.7.
- [ ] **RECONCILE8 — Operator interface.** `mimir reconcile status / pause / resume` (RECONCILE-DESIGN§1.9).
