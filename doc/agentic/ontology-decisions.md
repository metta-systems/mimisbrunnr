# Ontology — open decisions

Design: [ontology-design.md](ontology-design.md) · Plan: [ontology-implementation-plan.md](ontology-implementation-plan.md)

## ONTOLOGY-D2 — Module syntax: TOML or KDL

SPECIFICATION-DESIGN§4.2 uses TOML `[[tags]]` tables. A KDL form (`module`, `tags { "artist" text attribute }`, `implications { from "rock" to "genre" }`) reads more compactly. Decide the source format; a change rewrites the host-side compiler and the shipped modules (the compiled record form is unaffected).

## ONTOLOGY-D5 — Storage policy priority

SPECIFICATION-DESIGN§8.3 calls the chunking policy an admin override but ranks it below the ontology setting; SPECIFICATION-DESIGN§9.6 adds further chunking rules. Decide the precedence order of admin, ontology and default policies for each storage axis. Blocks [SPEC4](specification-implementation-plan.md).

## ONTOLOGY-D6 — `mimir ontology` command surface

SPECIFICATION-DESIGN Appendix A lists `list`, `install`, `remove`, `orphans`, `adopt`; `bin/mimir` also has `show <tag>`. Candidate additions: `status` (modules with installers and tag counts, orphan summary), `graph [--tag <t>]` (rule DAG), `conflicts` (objects violating constraints), `upgrade <module>`, `remove --unique | --purge` (scrub policy, ONTOLOGY-DESIGN "Removal and scrub policies"), `migrate` (reassign objects before a blocked tag removal), `scrub --orphans`, `compile <source>` (host-side compiler), `create-module`, `add-tag`, `add-rule`, `export <module>` (source out). Decide which exist and their names. Shapes [ONTOLOGY8](ontology-implementation-plan.md), [ONTOLOGY9](ontology-implementation-plan.md) and [ONTOLOGY14](ontology-implementation-plan.md).

## Future work

- `extensible` tags: whether other modules may add rules under a tag (SPECIFICATION-DESIGN§4.1).
- General rule chaining: any rule reading any derived term, computed to a fixpoint, in place of the fixed single-pass order (ONTOLOGY-DESIGN "Rules and constraints"); needs a cycle check across all rule kinds and a general recompute on rule removal.
- If maintaining `Bucket` rules on `changed` (INDEX-DESIGN§2.4) proves too costly under real loads: remove the `changed` record field and expose the forward entry's `modified_ns` as its own column (e.g. `retagged`), leaving "either changed" to SQL.
- A module repository so `mimir ontology install <module-id>` fetches by id.
- Extend the example ontologies with text, email and photo/film album categories to exercise the category model.
