# Ontology — implementation plan

Design: [ontology-design.md](ontology-design.md) · Decisions: [ontology-decisions.md](ontology-decisions.md) · Order: [operations-plan.md](operations-plan.md)

- [ ] **ONTOLOGY1 — Single tag-id allocator.** Drop `Engine.next_tag_id`; `Engine::register_tag` goes through `OntologyState::install`; `next_tag_id` derivation lives in `OntologyState`. Prerequisite: ONTOLOGY-D1. Acceptance: tags registered ad hoc and through modules never collide across reopen; the legacy blob's `next_tag_id` is unused ([ENGINE7](engine-implementation-plan.md)).
- [ ] **ONTOLOGY2 — `OntologyError::Storage`.** Add `Storage(#[from] StorageError)` and replace the lossy mapping of region errors into `OntologyError::Cbor`. Acceptance: a region-full failure surfaces as the storage variant in a test.
- [ ] **ONTOLOGY3 — Adopt orphan tags.** `OntologyState::adopt_orphan` and `mimir ontology adopt <tag> --into <module>`. Prerequisite: ONTOLOGY-D1.
- [ ] **ONTOLOGY4 — Module syntax migration.** Prerequisite: ONTOLOGY-D2 selecting a change.
