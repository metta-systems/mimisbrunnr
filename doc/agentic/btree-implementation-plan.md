# B+ tree regions — implementation plan

Design: [btree-design.md](btree-design.md) · Decisions: [btree-decisions.md](btree-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Region format

- [ ] **BTREE10 — Positional page regions.** Radix leaves and inner nodes and forward overflow as `logical_pages` pages written in page runs; backward-linked chains (BTREE-DESIGN§1.7), in `crates/storage/src/region.rs` beside the keyed runs. The forward-overflow chain (`crates/index/src/forward_index.rs`) and the object and location tables move onto them; the legacy `btree_node::BtreeNodeHeader`, `object_leaf.rs` and `location_leaf.rs` go. Acceptance: changing one record of a full radix leaf writes 8 KiB; a never-written page reads as zeros; a chain of three regions is appended without rewriting the first two.

- [ ] **BTREE12 — Footprint figures for the write-once run format.** The per-run capacities `validation/` pins (forward, tag-directory, range and chunk-index entries per run, inner fan-out) and the figures the design documents quote for them are recomputed for index blocks, fixed-width keys and 4 KiB data blocks (BTREE-DESIGN§1.2, BTREE-DESIGN§1.6); the legacy `SortedRunHeader` and bit-width `FieldFormat` mirrors in `validation/` go. Acceptance: every capacity constant in `validation/` is derived from the region structs, and each figure it pins appears in its design section.

## Live tree machinery

- [ ] **BTREE4 — Separator keys for descent.** Inner nodes route by full separator keys with `NodePtr` values; descent checks that a loaded child's first key is not below its separator and reports corruption otherwise. Acceptance: a tree of term-dictionary keys (wider than 16 bytes) and one of chunk hashes descend correctly across three levels.
- [ ] **BTREE5 — Append-on-flush with whiteouts.** A flush writes one new run with the pending inserts and whiteouts at the region's next free block and writes nothing else. Acceptance: 1 000-entry tree, modify one entry, flush: the region grows by one index block and one data block; a delete survives flush and reopen.
- [ ] **BTREE7 — Compaction.** Compaction (more than 4 runs, more than 75 % of blocks, or no room for the next run) merges into a fresh region, splitting when needed, and the parent's pointer follows at the next checkpoint (BTREE-DESIGN§1.4). Engine hook: [ENGINE2](engine-implementation-plan.md). Acceptance: after compaction the node has one run; a crash between compaction and checkpoint reopens on the old region with no data lost.

