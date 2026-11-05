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

## Future work

- Module extensions listed in SPECIFICATION-DESIGN§4.1 / SPECIFICATION-DESIGN§4.2: `mutex_groups`, `constraints`, `requires`, `installed_by` refcount, `core` flag, `extensible` tags.
- Extend the example ontologies with text, email and photo/film album categories to exercise the category model.
