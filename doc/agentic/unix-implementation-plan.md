# Unix interoperability and FUSE — implementation plan

Design: [unix-design.md](unix-design.md) · Decisions: [unix-decisions.md](unix-decisions.md) · Order: [operations-plan.md](operations-plan.md)

- [ ] **UNIX1 — Rebuild path contexts at boot.** `DiskEngine::open` scans the forward index for `Attr(unix-path, …)` and rebuilds `PathContextManager`; `path_contexts_bytes` leaves the legacy blob. Acceptance: import a tree, commit, reopen: `/ctx/` lists the same paths. Unblocks [ENGINE8](engine-implementation-plan.md).
- [ ] **UNIX2 — FUSE write operations.** Prerequisite: UNIX-D1.
- [ ] **UNIX3 — Real file attributes.** Prerequisite: UNIX-D2.
- [ ] **UNIX4 — Symlinks under `/ctx/`.** Model `ImportKind::Symlink` in `VfsEntryKind`. Prerequisite: UNIX-D1.
- [ ] **UNIX5 — FUSE mount integration tests.** A test target that mounts, lists and reads through FUSE where the host allows unprivileged mounts (`user_allow_other`), skipped with a reported reason elsewhere.
- [ ] **UNIX6 — Path lookup without a scan.** `PathProjection::reverse_lookup` resolves a path through an index on the path-projection attribute (UNIX-DESIGN§1) instead of scanning every projected path. Acceptance: lookup cost does not grow with the number of projected paths (benchmark or operation count).