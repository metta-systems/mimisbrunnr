# Ontology — open decisions

Design: [ontology-design.md](ontology-design.md) · Plan: [ontology-implementation-plan.md](ontology-implementation-plan.md)

## ONTOLOGY-D2 — Module syntax: TOML or KDL

SPECIFICATION-DESIGN§4.2 uses TOML `[[tags]]` tables. A KDL form (`module`, `tags { "artist" text attribute }`, `implications { from "rock" to "genre" }`) reads more compactly. Decide the source format; a change rewrites the host-side compiler and the shipped modules (the compiled record form is unaffected).

## ONTOLOGY-D5 — Storage policy priority

SPECIFICATION-DESIGN§8.3 calls the chunking policy an admin override but ranks it below the ontology setting; SPECIFICATION-DESIGN§9.6 adds further chunking rules. Decide the precedence order of admin, ontology and default policies for each storage axis. Blocks [SPEC4](specification-implementation-plan.md).

## ONTOLOGY-D6 — `mimir ontology` command surface

SPECIFICATION-DESIGN Appendix A lists `list`, `install`, `remove`, `orphans`, `adopt`; `bin/mimir` also has `show <tag>`. Candidate additions: `status` (modules with installers and tag counts, orphan summary), `graph [--tag <t>]` (rule DAG), `conflicts` (objects violating constraints), `upgrade <module>`, `remove --unique | --purge` (scrub policy, ONTOLOGY-DESIGN "Removal and scrub policies"), `migrate` (reassign objects before a blocked tag removal), `scrub --orphans`, `compile <source>` (host-side compiler), `create-module`, `add-tag`, `add-rule`, `export <module>` (source out). Decide which exist and their names. Shapes [ONTOLOGY8](ontology-implementation-plan.md), [ONTOLOGY9](ontology-implementation-plan.md) and [ONTOLOGY14](ontology-implementation-plan.md).

## ONTOLOGY-D10 — Storage policies and single-valued keys

A storage policy (chunking, deduplication, compression, encryption; SPECIFICATION-DESIGN§3.5) attaches to a tag. Mutually exclusive states are values of a single-valued key (ONTOLOGY-DESIGN "Tags and attribute keys"), and a value carries no policy, so a state that should change how a file is stored (`tier = archive | active`, `sensitivity = public | secret`) cannot drive storage directly. The resolver also loses a way to know that two policy tags never meet on one object: two sibling tags with conflicting policies fail install, and declaring them mutually exclusive is no longer one of the fixes. Options:

- (a) **States that drive storage stay labels.** Single-valued keys are for states without storage meaning; two conflicting policy tags need an implication or a common ancestor (SPECIFICATION-DESIGN§3.5 case 2), and nothing marks them as never coexisting.
- (b) **Policies on attribute values.** A key declaration carries a policy per value (`storage` per value of a single-valued key, a `TagRecord` field or a value-policy record); the resolver treats values of one single-valued key as never coexisting, so conflicting policies on them are no conflict. Adds a policy source beside tags to every resolution and to ONTOLOGY-D5's precedence.
- (c) **Through `ValueImplies` into policy labels.** `build-state = stale → cold-storage`, the label carrying the policy — expressible today; the resolver treats labels whose only sources are `ValueImplies` from distinct values of one single-valued key as never coexisting. No new record, but the exclusivity is inferred from the rule graph and breaks as soon as the label is also asserted directly or implied from elsewhere.

Affects SPECIFICATION-DESIGN§3.5 and its sibling-tag case, [ONTOLOGY-D5](#ontology-d5--storage-policy-priority) (precedence of policy sources) and [ONTOLOGY16](ontology-implementation-plan.md).

## Future work

- `extensible` tags: whether other modules may add rules under a tag (SPECIFICATION-DESIGN§4.1).
- General rule chaining: any rule reading any derived term, computed to a fixpoint, in place of the fixed single-pass order (ONTOLOGY-DESIGN "Rules and constraints"); needs a cycle check across all rule kinds and a general recompute on rule removal.
- If maintaining `Bucket` rules on `changed` (INDEX-DESIGN§2.4) proves too costly under real loads: remove the `changed` record field and expose the forward entry's `modified_ns` as its own column (e.g. `retagged`), leaving "either changed" to SQL.
- A module repository so `mimir ontology install <module-id>` fetches by id.
- Extend the example ontologies with text, email and photo/film album categories to exercise the category model.
