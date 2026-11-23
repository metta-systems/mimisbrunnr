# Ontology — completed

- ONTOLOGY2: `OntologyError::Storage(#[from] StorageError)`; region errors keep their storage variant instead of becoming `Cbor` strings — `crates/ontology/src/error.rs`, `crates/ontology/src/state.rs` (tests `a_full_region_surfaces_as_a_storage_error`, `ontology_region_kind_mismatch_errors`). A value larger than its region is `RegionFull`, checked before index-block capacity — `crates/storage/src/region.rs` (`a_value_larger_than_the_region_is_reported_as_a_full_region`).
- Ad-hoc tag registration and module install never reuse a tag id held by the ontology, including after reopen (stopgap until ONTOLOGY1's single allocator) — `crates/engine/src/engine.rs` `next_free_tag_id`, test `tag_ids_never_reused_after_reopen`.
- Ontology persisted as one CBOR image per snapshot in a `BtreeKind::Ontology` region referenced by `RootPointer.ontology_root` — `crates/ontology/src/state.rs`.
