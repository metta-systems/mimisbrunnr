# B+ tree regions — implementation plan

Design: [btree-design.md](btree-design.md) · Decisions: [btree-decisions.md](btree-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Live tree machinery

- [ ] **BTREE4 — Separator keys for descent.** Inner nodes route by full separator keys with `NodePtr` values; descent checks that a loaded child's first key is not below its separator and reports corruption otherwise. Acceptance: a tree of term-dictionary keys (wider than 16 bytes) and one of chunk hashes descend correctly across three levels.
- [ ] **BTREE5 — Append-on-flush with whiteouts.** A flush writes one new run with the pending inserts and whiteouts at the region's next free block and writes nothing else. Acceptance: 1 000-entry tree, modify one entry, flush: the region grows by one index block and one data block; a delete survives flush and reopen.
- [ ] **BTREE7 — Compaction.** Compaction (more than 4 runs, more than 75 % of blocks, or no room for the next run) merges into a fresh region, splitting when needed, and the parent's pointer follows at the next checkpoint (BTREE-DESIGN§1.4). Engine hook: [ENGINE2](engine-implementation-plan.md). Acceptance: after compaction the node has one run; a crash between compaction and checkpoint reopens on the old region with no data lost.

