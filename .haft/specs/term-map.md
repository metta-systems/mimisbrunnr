# Term Map

Vocabulary for the Mímisbrunnr target system and the Haft v7 governance
carriers that describe it. Object-level terms describe what lives inside a
Mímisbrunnr pool; description-level terms describe the carriers and
artifacts the harness produces about that pool.

```yaml term-map
status: active
entries:
  - term: HarnessableProject
    domain: haft
    layer: carrier
    definition: "A repository that owns parseable Haft v7 authority carriers (specs, decisions, commissions, evidence) sufficient for the harness to operate without re-deriving intent from prose. Mímisbrunnr is the project being onboarded."

  - term: TargetSystemSpec
    domain: haft
    layer: description
    definition: "The specification of the system the project produces, expressed as SpecSection records under .haft/specs/target-system.md. Says what changes in the world; says nothing about how the team builds it."

  - term: EnablingSystemSpec
    domain: haft
    layer: description
    definition: "The specification of the development system that builds and operates the target — repo architecture, work methods, effect boundaries, agent policy, evidence policy. Under .haft/specs/enabling-system.md."

  - term: SpecSection
    domain: haft
    layer: carrier
    definition: "An individually addressable claim inside a spec carrier, identified by an id like TS.environment.001, carrying statement_type, claim_layer, valid_until, status, and (when load-bearing) evidence_required."

  - term: SpecSectionBaseline
    domain: haft
    layer: carrier
    definition: "A snapshot hash of a SpecSection captured at human approval. Subsequent edits report as spec_section_drifted until rebaselined or rolled back."

  - term: SpecCoverage
    domain: haft
    layer: description
    definition: "The derived view of which active SpecSections have authorising DecisionRecords, evidence, and harness work in progress; produced by `haft spec coverage`."

  - term: DecisionRecord
    domain: haft
    layer: carrier
    definition: "A recorded engineering decision under .haft/decisions/, authorising a chosen approach for a framed problem. Decisions cite the SpecSections they advance."

  - term: WorkCommission
    domain: haft
    layer: carrier
    definition: "An authorisation envelope (scope, lockset, evidence requirement) under which the harness may execute changes. Derived from active DecisionRecords."

  - term: RuntimeRun
    domain: haft
    layer: work
    definition: "A single harness execution against a WorkCommission, producing diffs, logs, and Evidence; lifecycle-owned by a surface (Claude Code, Codex, …)."

  - term: Evidence
    domain: haft
    layer: evidence
    definition: "A typed artifact that backs a SpecSection or DecisionRecord — type-check pass, L1..L4 test, DB-level invariant, E2E run, or manual procedure record. Subject to refresh triggers."

  - term: ExternalProjection
    domain: haft
    layer: carrier
    definition: "A read-only view of Haft state published for an external audience (e.g. a generated README section, a status page); never the source of truth."

  - term: Object
    domain: mimisbrunnr
    layer: object
    definition: "An entity stored in a Mímisbrunnr pool, identified by an ObjectId (16-bit node id, 48-bit local sequence). Has no intrinsic name or path; carries a bag of Assertions."

  - term: Assertion
    domain: mimisbrunnr
    layer: object
    definition: "A statement about an Object. Three shapes — Tag (this object is X), Attr (key K has value V), Relation (predicate P targets ObjectId T) — each either Direct (set by caller) or Materialized (added by ontology implication)."

  - term: Tag
    domain: mimisbrunnr
    layer: object
    definition: "A TagId-keyed Assertion shape recording membership in a category. The TagId space (u32) is shared across tags, attribute keys, and relation predicates."

  - term: Query
    domain: mimisbrunnr
    layer: description
    definition: "A predicate over the Assertion store evaluated by bitmap algebra; supports HasTag, HasAttr (with comparison ops), Related, And/Or/Not, and IsA (ontology-aware subsumption). Resolves to an ObjectId set."

  - term: Ontology
    domain: mimisbrunnr
    layer: object
    definition: "The schema of TagDefinitions and implication rules attached to a pool. Determines Materialized Assertions and policy effects (placement, tier, compression, encryption) on Objects whose direct Assertions match."

  - term: Pool
    domain: mimisbrunnr
    layer: object
    definition: "The unit of Mímisbrunnr deployment — one or more block devices forged together by `brunnr create`, hosting a Superblock, WAL, metadata zones, and blob zones. A Mímir instance operates against exactly one pool."
```

Drift discipline: extend or correct entries as the system evolves; do not
delete an entry without first checking that no active SpecSection lists it
under `terms:`.
