# B+ tree regions — completed

- Packed-key codec persists elided value-prefix bytes inline in `SortedRunKeyFormat` — `crates/storage/src/btree/pack.rs`.
- Packed-key codec supports `FIXED` and `VARINT` value sizes with auto-selection and a force-`VARINT` hint — `crates/storage/src/btree/pack.rs`.
- `LoadedNode` carries a `JournalPin` updated on journal push and cleared on flush — `crates/storage/src/btree.rs`, test `journal_pin_covers_pending_journal_until_flush`.
- BTREE1: packed-codec region API (`write_full_packed`, `write_full_packed_force_varint`, `read_packed`, `append_sorted_run_packed`) with `&self` device access, used by the chunk, forward and tag indices — `crates/storage/src/btree.rs`, packed round-trip tests in `crates/storage/src/btree/tests.rs`.
- BTREE2: `BtreeRegion::read` decodes header and CBOR runs, rejects a `BtreeKind` mismatch and drops runs from a torn (CRC-failing) run onward — tests `region_write_full_then_read_round_trip`, `region_kind_mismatch_on_read_errors`, `read_torn_*`.
- BTREE3: workspace builds; `cargo test --workspace`, `cargo clippy --workspace` and `cargo clippy --workspace --all-targets` are clean.
