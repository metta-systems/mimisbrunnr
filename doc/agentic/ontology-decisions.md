# Ontology — open decisions

Design: [ontology-design.md](ontology-design.md) · Plan: [ontology-implementation-plan.md](ontology-implementation-plan.md)

## ONTOLOGY-D2 — Module syntax: TOML or KDL

SPECIFICATION-DESIGN§4.2 uses TOML `[[tags]]` tables. A KDL form (`module`, `tags { "artist" text attribute }`, `implications { from "rock" to "genre" }`) reads more compactly. Decide the source format; a change rewrites the host-side compiler and the shipped modules (the compiled record form is unaffected).

## ONTOLOGY-D4 — Hierarchy versus intersecting categories in examples

SPECIFICATION-DESIGN§13.2 builds an implication hierarchy on top of a non-hierarchical system. Decide whether the example (and the recommended modelling style) should use intersecting low-height categories instead.

## ONTOLOGY-D5 — Storage policy priority

SPECIFICATION-DESIGN§8.3 calls the chunking policy an admin override but ranks it below the ontology setting; SPECIFICATION-DESIGN§9.6 adds further chunking rules. Decide the precedence order of admin, ontology and default policies for each storage axis. Blocks [SPEC4](specification-implementation-plan.md).

## ONTOLOGY-D6 — `mimir ontology` command surface

SPECIFICATION-DESIGN Appendix A lists `list`, `install`, `remove`, `orphans`, `adopt`; `bin/mimir` also has `show <tag>`. Candidate additions: `status` (modules with installers and tag counts, orphan summary), `graph [--tag <t>]` (rule DAG), `conflicts` (objects violating constraints), `upgrade <module>`, `remove --unique | --purge` (scrub policy, ONTOLOGY-DESIGN "Removal and scrub policies"), `migrate` (reassign objects before a blocked tag removal), `scrub --orphans`, `compile <source>` (host-side compiler), `create-module`, `add-tag`, `add-rule`, `export <module>` (source out). Decide which exist and their names. Shapes [ONTOLOGY8](ontology-implementation-plan.md), [ONTOLOGY9](ontology-implementation-plan.md) and [ONTOLOGY14](ontology-implementation-plan.md).

## ONTOLOGY-D9 — What a write violating `Exclusive` does

An `Exclusive { tags }` constraint allows at most one of its tags on an object (`current` / `stale`, `draft` / `published`). Decide what asserting a second tag of the set does:

- (a) **Replace**: the other tags of the set are removed from the object in the same mutation and the result reports what was removed. One call moves an object between states; a mistaken assertion silently drops a tag, and a removal implied through rules (asserting `x` where `x → stale`) is non-obvious.
- (b) **Reject**: the write fails with a defined error naming the conflicting tag, as `Requires` does; moving between states is an explicit remove-then-add in one mutation.
- (c) **Per constraint**: the constraint record carries a mode (replace or reject), chosen by the module author.

Whichever is chosen also decides what an implied tag does: whether a derived term may displace a direct one, and whether a rule change that makes an object violate the set is reported (as on install) or applied. Cross-node races are [CLUSTER-D2](cluster-decisions.md). Blocks [ONTOLOGY12](ontology-implementation-plan.md); a per-constraint mode (c) adds a field to `RuleRecord` kind 3 (ONTOLOGY-DESIGN§1).

## Future work

- Module extensions listed in SPECIFICATION-DESIGN§4.1 / SPECIFICATION-DESIGN§4.2:
  - `requires`: module dependencies with version requirements (`"systems.metta.core" = ">=1.0, <4.0"`); install fails on a missing dependency or a version outside the requirement, and removal follows reverse-dependency order.
  - `extensible` tags: whether other modules may add rules under a tag.
- A module repository so `mimir ontology install <module-id>` fetches by id.
- Extend the example ontologies with text, email and photo/film album categories to exercise the category model.
