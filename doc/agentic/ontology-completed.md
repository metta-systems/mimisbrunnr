# Ontology — completed

- Ad-hoc tag registration and module install never reuse a tag id held by the ontology, including after reopen (stopgap until ONTOLOGY1's single allocator) — `crates/engine/src/engine.rs` `next_free_tag_id`, test `tag_ids_never_reused_after_reopen`.
- Ontology persisted as one CBOR image per snapshot in a `BtreeKind::Ontology` region referenced by `RootPointer.ontology_root` — `crates/ontology/src/state.rs`.
