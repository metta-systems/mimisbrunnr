# Journal (WAL) — completed

- `WalOpKind::FormatPromote = 30` with its payload and CBOR (de)serialisation; replay acknowledges the op — `crates/wal/src/entry.rs`, `crates/engine/src/wal_proj.rs`.
- JOURNAL1: one `JournalPin`, defined in `crates/storage/src/btree.rs` and exported by `mimisbrunnr-storage`; `mimisbrunnr-wal` has no pin type of its own.
- Read-only WAL ring open that rejects mutations — `crates/wal/src/ring.rs`, test `open_read_only_rejects_mutations`.
