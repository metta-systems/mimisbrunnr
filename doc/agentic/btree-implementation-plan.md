# B+ tree regions — implementation plan

Design: [btree-design.md](btree-design.md) · Decisions: [btree-decisions.md](btree-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Live tree machinery

- [ ] **BTREE4 — Populate `min_key` / `max_key`.** `flush_to_run`, `compact` and `append_sorted_run` keep the header range current (none of them sets it today); inner-node descent validates the child range after load and reports corruption on mismatch. Prerequisite: BTREE-D2 for keys wider than 16 B. Acceptance: test dumps header bytes and compares them with the smallest and largest key.
- [ ] **BTREE5 — Append-on-flush with whiteouts.** `flush_to_run` keeps deletes (it currently keeps only inserts); `append_sorted_run` writes only the new run and the header sector. Prerequisite: BTREE-D1. Acceptance: 1 000-entry tree, modify one entry, flush: the region grows by one small run; a delete survives flush and reopen.
- [ ] **BTREE6 — Emit `FormatPromote` on format widening.** When an appended run needs a wider `SortedRunKeyFormat` than the existing runs, journal `FormatPromote` before writing the run. Prerequisites: BTREE5. Replay: [JOURNAL3](journal-implementation-plan.md).
- [ ] **BTREE7 — Compaction and its crash recovery.** `should_compact` (payload > 75 % or > 4 runs) and `compact` into a fresh region; on boot, a region whose header carries `BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS` is re-compacted. Engine hook: [ENGINE2](engine-implementation-plan.md). Acceptance: after compaction `sorted_run_count == 1`; interrupted-compaction test recovers.

## Read path polish

- [ ] **BTREE8 — Codec-transparent region read.** A region read dispatches on the run flags (packed or CBOR) and returns `StorageError::MixedRunFormats { offset }` for mixed regions; drop the `UnsupportedFormatVersion(0)` misuse. Acceptance: a region written by the packed writer is readable through the generic read.
