# Metadata tables — completed

- Byte-exact 256 KiB object-table and location-table leaf layouts used by `flush_to_region` / `load_from_region` (single leaf) — `crates/meta/src/object_leaf.rs`, `crates/meta/src/location_leaf.rs`.
- Backpointer table persisted in a B+ tree region — `crates/meta/src/backpointer.rs`.
- `ObjectLocation::serialize` returning `Vec<u8>` — `crates/meta/src/location.rs`.
