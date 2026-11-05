# B+ tree regions — completed

- Packed-key codec persists elided value-prefix bytes inline in `SortedRunKeyFormat` — `crates/storage/src/btree/pack.rs`.
- Packed-key codec supports `FIXED` and `VARINT` value sizes with auto-selection and a force-`VARINT` hint — `crates/storage/src/btree/pack.rs`.
- `LoadedNode` carries a `JournalPin` updated on journal push and cleared on flush — `crates/storage/src/btree.rs`, tests `loaded_node_has_pin`, `pin_updates_on_journal_entry`, `pin_clears_on_flush`.
