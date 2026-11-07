# Ontology — open decisions

Design: [ontology-design.md](ontology-design.md) · Plan: [ontology-implementation-plan.md](ontology-implementation-plan.md)

## ONTOLOGY-D1 — How ad-hoc tags enter the ontology

`Engine::register_tag` bypasses `OntologyState::install`. Proposed: wrap each ad-hoc tag in a synthetic single-tag module named `_engine_adhoc`. Alternatives: a dedicated orphan-tag pool inside `OntologyState`, adoptable later by `mimir ontology adopt`. Blocks [ONTOLOGY1](ontology-implementation-plan.md) and shapes [ONTOLOGY3](ontology-implementation-plan.md).

## ONTOLOGY-D2 — Module syntax: TOML or KDL

SPECIFICATION-DESIGN§4.2 uses TOML `[[tags]]` tables. A KDL form (`module`, `tags { "artist" text attribute }`, `implications { from "rock" to "genre" }`) reads more compactly. Decide the distribution format; a change rewrites the loader and the shipped modules.

## ONTOLOGY-D3 — Category versus format tags

All mp3 files are `audio`, but not all are `music` (SPECIFICATION-DESIGN§11.2-inspired example). Record the modelling rule that separates format-derived implications from content categories (a solution was worked out in an earlier discussion and needs to be written down).

## ONTOLOGY-D4 — Hierarchy versus intersecting categories in examples

SPECIFICATION-DESIGN§13.2 builds an implication hierarchy on top of a non-hierarchical system. Decide whether the example (and the recommended modelling style) should use intersecting low-height categories instead.

## ONTOLOGY-D5 — Storage policy priority

SPECIFICATION-DESIGN§8.3 calls the chunking policy an admin override but ranks it below the ontology setting; SPECIFICATION-DESIGN§9.6 adds further chunking rules. Decide the precedence order of admin, ontology and default policies for each storage axis. Blocks [SPEC4](specification-implementation-plan.md).

## ONTOLOGY-D6 — `mimir ontology` command surface

SPECIFICATION-DESIGN Appendix A lists `list`, `install`, `remove`, `orphans`, `adopt`; `bin/mimir` also has `show <tag>`. Candidate additions: `status` (modules with owning apps and tag counts, orphan summary), `graph [--tag <t>]` (implication DAG), `conflicts`, `upgrade <module>`, `remove --unique | --purge` (scrub policy, ONTOLOGY-DESIGN "Removal and scrub policies"), `migrate` (reassign objects before a blocked tag removal), `scrub --orphans`, `create-module`, `add-tag`, `add-implication --from --to --module`, `export <module>` (TOML out). Decide which exist and their names. Shapes [ONTOLOGY8](ontology-implementation-plan.md) and [ONTOLOGY9](ontology-implementation-plan.md).

## ONTOLOGY-D7 — Shape of the cluster ontology delta

`SyncBundle.ontology_delta: Option<OntologyDelta>` ([cluster-design.md](cluster-design.md)) has no defined type. Options: (a) the whole `OntologyImage` of the bundle's `to_snapshot` (simple, rare, a few MiB); (b) steps — install module (full module), remove module (with scrub policy), upgrade module (new version plus the migration steps applied). Option (b) lets a receiver replay object-side effects (rematerialisation, purge) deterministically; (a) needs the receiver to diff images to find them. Blocks [CLUSTER2](cluster-implementation-plan.md).

## ONTOLOGY-D8 — Tag ids after tag removal

`next_tag_id` is derived at boot from the largest `TagId` in the loaded ontology image. The Purge and RemoveUnique scrub policies (ONTOLOGY-DESIGN "Removal and scrub policies") delete tags, so removing the tag with the largest id and reopening would hand that id out again, while `TagId`s appear in postings history, retained journal entries (`AddTag`) and subscription queries. Options: (a) a `tag_id_high_water: u32` field in `OntologyImage`, raised on every allocation and never lowered, from which the counter is derived; (b) keep a removed tag's `TagDefinition` as a tombstone in the image when it holds the largest id, as the term table's high-water marker does (INDEX-DESIGN§1.2). Shapes [ONTOLOGY1](ontology-implementation-plan.md) and [ONTOLOGY9](ontology-implementation-plan.md).

## Future work

- Module extensions listed in SPECIFICATION-DESIGN§4.1 / SPECIFICATION-DESIGN§4.2:
  - `requires`: module dependencies with version requirements (`"systems.metta.core" = ">=1.0, <4.0"`); install fails on a missing dependency or a version outside the requirement, and removal follows reverse-dependency order.
  - `installed_by` refcount: each app that installs a module is recorded; removing an app drops it from every module's list, and only modules whose list becomes empty are removed under the chosen scrub policy, while still-shared modules are reported with the apps that keep them.
  - `core` flag: modules that cannot be removed.
  - `extensible` tags: whether other modules may add implications under a tag.
  - Mutex consistency on install: report existing objects that already carry two tags a new `mutex` relation separates.
- A module repository so `mimir ontology install <module-id>` fetches by id.
- Extend the example ontologies with text, email and photo/film album categories to exercise the category model.
