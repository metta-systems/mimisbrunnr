# Engine — completed

- Every migrated tree persists in its own B+ tree region in the index zone — `crates/engine/src/disk_engine.rs` (`save_index_state`, `load_index_state`).
- `RootPointer.*_root` slots are seeded at create and on every commit, and trees load from their slots (`BlockRef::ZERO` loads empty); trees stay at fixed offsets — `crates/engine/src/disk_engine.rs`, 3 integration tests.
- `Engine::ensure_oid_exists` public — `crates/engine/src/engine.rs`.
- `DiskEngine::read_blob` reads from the in-memory blob map — `crates/engine/src/disk_engine.rs`.
