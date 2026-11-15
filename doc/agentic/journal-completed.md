# Journal (WAL) — completed

- `WalOpKind::FormatPromote = 30` — removed by BTREE9, each run fixing its own key format; 30 is unassigned until JOURNAL8 renumbers.
- JOURNAL1: one `JournalPin`, defined in `crates/storage/src/journal_pin.rs` and exported by `mimisbrunnr-storage`; `mimisbrunnr-journal` has no pin type of its own.
- Read-only WAL ring open that rejects mutations — `crates/journal/src/ring.rs`, test `open_read_only_rejects_mutations`.
- JOURNAL7: XXH3-64 (seed 0) through one `checksum64` / `Checksum64` in `crates/storage/src/checksum.rs` (tests `xxh3_64_known_vector`, `incremental_equals_one_shot`, `one_flipped_bit_changes_the_checksum`) for every checksum, WAL included; structures with 32-bit fields store its low half until their layouts are rewritten; `crc32c` and `crc32fast` gone from the workspace.
