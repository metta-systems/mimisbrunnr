# Target System Spec

Mímisbrunnr is the associative filesystem of the Mettā operating system. The
target spec describes what changes in the world once a Mímisbrunnr pool is
created on a node and Mímir is answering queries against it.

## TS.environment.001 Environment change: associative, replicated, ontology-driven storage

```yaml spec-section
id: TS.environment.001
spec: target-system
kind: environment-change
title: Associative retrieval, locally-answerable metadata, ontology-driven policy
owner: human
statement_type: duty
claim_layer: object
status: draft
valid_until: 2026-11-14
depends_on: []
supersedes: [TS.placeholder.001]
terms:
  - Object
  - Assertion
  - Tag
  - Query
  - Ontology
  - Pool
target_refs: []
evidence_required:
  - kind: E2E
    description: brunnr create + mimir tag + mimir query round-trip retrieves the expected ObjectId set without any path argument
  - kind: E2E
    description: with cluster replication enabled, a node that did not write an object can still answer a tag query that selects it, using only local metadata (no blob fetch on the query path)
  - kind: L3
    description: an ontology rule attached to a tag (e.g. archive implies cold-tier) causes a placement change for matching objects without a separate user action
```

Once a Mímisbrunnr pool exists and its metadata plane has converged on a
node, three observables flip relative to a conventional hierarchical
filesystem:

1. **Retrieval is associative, not positional.** A caller obtains an
   `ObjectId` set by submitting a `Query` over assertions
   (`Tag`, `Attr`, `Relation`, `IsA`) — no path is supplied, none exists.
   Whether the object was "put somewhere" is not a question the system
   answers; what assertions it carries is.
2. **Metadata is answerable from any node without query-path network
   I/O.** The metadata plane (tag bitmaps, forward index, ontology,
   object table) is replicated to every participating node. Once
   metadata has converged locally, no network round-trip is required to
   answer a query; query latency is bounded by local index cost and does
   not scale with cluster size. Bytes cross the wire only for
   replication / convergence traffic and when an application actually
   opens a blob the local node does not hold.
3. **Storage policy follows ontology, not location.** Direct assertions
   on an object combine with ontology rules to produce materialized
   assertions; placement, tier, compression, and encryption are
   determined by rules over the resulting assertion set, not by where
   the object was written. A single tagging operation therefore
   re-classifies the object — the system does not require a separate
   relocation or attribute-change call to act on the new
   classification.

## TS.placeholder.001 Target system placeholder (superseded)

```yaml spec-section
id: TS.placeholder.001
spec: target-system
kind: environment-change
title: Target system placeholder
statement_type: explanation
claim_layer: carrier
owner: human
status: superseded
valid_until: 2026-11-14
depends_on: []
supersedes: []
terms: []
target_refs: []
evidence_required: []
```

Retained as a parseable carrier for traceability; superseded by
`TS.environment.001`.
