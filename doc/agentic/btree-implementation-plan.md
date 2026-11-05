# B+ tree regions — implementation plan

Design: [btree-design.md](btree-design.md) · Decisions: [btree-decisions.md](btree-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Restore the build

- [ ] **BTREE1 — Restore the packed-codec region API.** The `btree.rs` rewrite removed `write_full_packed`, `write_full_packed_force_varint`, `read_packed` and `append_sorted_run_packed`, while `btree/pack.rs` still holds the codec and `btree/tests.rs` and the index crate still call the API (`chunk_index.rs`, `forward_index.rs`). Bring the API back on top of the current `LoadedNode` / `JournalPin` structure, using the `&mut` device signatures consistently. Acceptance: `cargo test -p mimisbrunnr-storage` compiles and passes, including the packed round-trip tests.
- [ ] **BTREE2 — Implement `BtreeRegion::read_as_loaded_node`.** It currently ignores the bytes it reads and returns an empty `Forward` node (`btree.rs`, "TODO: Implement proper deserialization"). Decode header and runs, check the region's `BtreeKind` against the caller's, and return an error on mismatch or corruption. Acceptance: round-trip tests for at least two kinds; a kind-mismatch test; a corrupt-CRC test.
- [ ] **BTREE3 — Workspace green.** Update every consumer to the restored API and signatures (index, meta, pool, ontology, watch, engine, bins), clear the storage warnings (unused imports, unused `first_key` / `buf`, unused `align_up_to_sector`, elided lifetime in `merge_iter`, needless `mut`). Prerequisites: BTREE1, BTREE2, [JOURNAL1](journal-implementation-plan.md). Acceptance: `just build`, `just test`, `just clippy`, `cargo clippy --workspace --all-targets` all clean.

## Live tree machinery

- [ ] **BTREE4 — Populate `min_key` / `max_key`.** `flush_to_run`, `compact` and `append_sorted_run` keep the header range current (the current `flush_to_run` block is an empty placeholder); inner-node descent validates the child range after load and reports corruption on mismatch. Prerequisite: BTREE-D2 for keys wider than 16 B. Acceptance: test dumps header bytes and compares them with the smallest and largest key.
- [ ] **BTREE5 — Append-on-flush with whiteouts.** `flush_to_run` keeps deletes (it currently keeps only inserts); `append_sorted_run` writes only the new run and the header sector. Prerequisite: BTREE-D1. Acceptance: 1 000-entry tree, modify one entry, flush: the region grows by one small run; a delete survives flush and reopen.
- [ ] **BTREE6 — Emit `FormatPromote` on format widening.** When an appended run needs a wider `SortedRunKeyFormat` than the existing runs, journal `FormatPromote` before writing the run. Prerequisites: BTREE5. Replay: [JOURNAL3](journal-implementation-plan.md).
- [ ] **BTREE7 — Compaction and its crash recovery.** `should_compact` (payload > 75 % or > 4 runs) and `compact` into a fresh region; on boot, a region whose header carries `BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS` is re-compacted. Engine hook: [ENGINE2](engine-implementation-plan.md). Acceptance: after compaction `sorted_run_count == 1`; interrupted-compaction test recovers.

## Read path polish

- [ ] **BTREE8 — Codec-transparent region read.** A region read dispatches on the run flags (packed or CBOR) and returns `StorageError::MixedRunFormats { offset }` for mixed regions; drop the `UnsupportedFormatVersion(0)` misuse. Acceptance: a region written by the packed writer is readable through the generic read.
