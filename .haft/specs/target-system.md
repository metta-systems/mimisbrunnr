# Target System Spec

Mímisbrunnr is the associative filesystem of the Mettā operating system. The
target spec describes what changes in the world once a Mímisbrunnr pool is
created on a node and Mímir is answering queries against it.

## TS.environment.001 Environment change: associative, replicated, ontology-driven storage

```yaml spec-section
id: TS.environment.001
spec: target-system
kind: target.environment
title: Associative retrieval, locally-answerable metadata, ontology-driven policy
owner: human
statement_type: duty
claim_layer: object
status: active
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

## TS.role.001 Target role: default per-node associative filesystem for Metta user-mode state

```yaml spec-section
id: TS.role.001
spec: target-system
kind: target.role
title: Default per-node associative filesystem for Metta user-mode persistent state
owner: human
statement_type: definition
claim_layer: description
status: active
valid_until: 2026-11-15
depends_on: [TS.environment.001]
supersedes: []
terms:
  - Object
  - Query
  - Pool
  - Assertion
target_refs: []
evidence_required:
  - kind: manual
    description: a fresh Metta install boots with Mímisbrunnr mounted as the default storage type for user-mode processes (system services and end-user apps), with no extra operator step required
  - kind: manual
    description: an application that explicitly mounts and uses an alternative filesystem (e.g. ext4) on the same Metta node continues to function; Mímisbrunnr presence is not a precondition for any nanokernel-level operation
  - kind: E2E
    description: two Metta nodes each running their own Mímisbrunnr instance converge on a shared assertion graph through metadata exchange, with each node remaining authoritative for its own writes
```

Mímisbrunnr is assigned the role of Metta's default per-node associative
filesystem for user-mode persistent state. The assignment has four
load-bearing facets:

1. **Filesystem, not raw object store.** Objects are *located* by ID or
   `Query` (no path lookup), but once located are operated on as files
   through Metta's thick typed interfaces. Each object's tags,
   attributes, and relations are surfaced as first-class fields of those
   interfaces — not as a POSIX `xattr` afterthought.
2. **Default but non-mandatory.** Every Metta install ships with
   Mímisbrunnr as the storage type user-mode code reaches for by
   default. The nanokernel has no dependency on it, and an application
   that needs different semantics may mount and use an alternative
   filesystem (e.g. ext4) on the same node without making Metta
   non-functional.
3. **User-mode scope.** Both system services and end-user applications
   use Mímisbrunnr by default; the role is held against the whole of
   Metta userspace, not a subset.
4. **Per-node assignment.** Each Metta node runs its own Mímisbrunnr
   instance and is authoritative for its own writes; nodes exchange
   metadata updates with peers to converge the assertion graph. There is
   no single cluster-level Mímisbrunnr.

Out of role:

- Kernel-level service (the nanokernel does not depend on Mímisbrunnr).
- POSIX-compatible filesystem (path lookup, `open(path, …)`, `mkdir`,
  hard links are out of scope).
- Strongly consistent cluster store (convergence is eventual; cluster
  consensus is not part of the role).

## TS.boundary.001 Target boundary: in-scope, out-of-scope, and perspectives

```yaml spec-section
id: TS.boundary.001
spec: target-system
kind: target.boundary
title: Mímisbrunnr target-system boundary
owner: human
statement_type: admissibility
claim_layer: description
status: active
valid_until: 2026-11-15
depends_on: [TS.environment.001, TS.role.001]
supersedes: []
terms:
  - Object
  - Assertion
  - Query
  - Ontology
  - Pool
target_refs:
  - "law:metta-os-project-and-docs/DESIGN.md"
  - "admissibility:user-mode-callers-via-mimir-api-and-fuse"
  - "deontics:maintainers-operators-app-authors"
  - "evidence:workspace-tests-e2e-roundtrip-convergence-api-review"
evidence_required:
  - kind: L1
    description: workspace `cargo test --workspace` passes across all 17 crates
  - kind: E2E
    description: brunnr create + mimir tag + mimir query round-trip exercises only in-scope operations and produces expected ObjectId sets
  - kind: E2E
    description: snapshot create + Query with snapshot_id returns the assertion graph and resolves blob reads from the snapshot's blob contents on the same node
  - kind: E2E
    description: two-node convergence test — each node remains authoritative for its own writes, and metadata exchange brings their assertion graphs into agreement; no global snapshot coordination is performed
  - kind: manual
    description: API surface review confirms no out-of-scope operations (POSIX path lookup, cluster consensus, ACL enforcement, full-text blob search, near-duplicate dedup) are exposed
```

Perspectives on the boundary (resolution of the four `target_refs` above):

- **Law** — the Mettā OS project defines what Mímisbrunnr is and is not;
  `docs/DESIGN.md` and `docs/IMPLEMENTATION.md` are the authoritative
  carriers.
- **Admissibility** — user-mode callers (system services and end-user
  apps) are admitted across the boundary via the Mímir API and the FUSE
  adapter; ontology authors are admitted via ontology modules; pool
  operators are admitted via `brunnr`. The nanokernel and non-Mettā
  hosts are not admitted.
- **Deontics** — Mímisbrunnr maintainers have the duty to preserve
  assertions across node-local writes, deliver eventual convergence,
  and never recycle ObjectIds. Node operators have the duty to keep
  replicas reachable and back up pools. Application authors have the
  duty to use `Query`, not to assume any path structure.
- **Evidence** — the boundary holds iff the workspace test suite, the
  `brunnr` + `mimir` E2E round-trip, the per-node snapshot E2E, the
  two-node convergence E2E, and the API-surface review against this
  carrier all pass.

In-scope:

- **Object lifecycle.** Create, tag, untag, attribute-set,
  relation-set, materialized-implication, and tombstone-deletion of
  Objects within a Pool.
- **Query evaluation.** Bitmap algebra over `HasTag`, `HasAttr`,
  `Related`, And/Or/Not, and `IsA`; faceted exploration of result sets.
  Queries accept an optional `snapshot_id` and resolve the assertion
  graph as of that snapshot.
- **Pool management.** Forge across multiple block devices, attach /
  detach, zone layout (Superblock, WAL, metadata zones, blob zones),
  placement and tiering driven by ontology rules.
- **Metadata replication / convergence.** Per-node Mímisbrunnr
  instances exchange metadata updates; each node is authoritative for
  its own writes; convergence is eventual and is not coordinated by a
  cluster consensus protocol.
- **Subscriptions.** Watch streams over `ChangeInterest` against the
  assertion graph.
- **Access surfaces.** Mímir API (programmatic) and FUSE adapter
  (TMSU-style tag navigation, Unix path projection under `/ctx/`) as
  supported callers.
- **Persistence formats.** Superblock, WAL, ObjectTable, indexes
  (forward / tag / range / KV), ontology — little-endian, CRC-protected,
  CBOR for structured payloads.
- **Versioning and snapshots.** Snapshot creation and retention are
  part of the on-disk format; a snapshot captures both the assertion
  graph (tags, attributes, relations, ontology state) and the blob
  contents it references. Snapshots are per-node — there is no global
  snapshot synchronisation across the cluster.

Out-of-scope:

- Nanokernel integration (Mímisbrunnr is a userspace component; the
  Mettā nanokernel has no dependency on it).
- POSIX path semantics: `open(path, …)`, `mkdir`, hard links, and
  symlink-as-path resolution are not provided.
- Strongly consistent cluster consensus and linearisable cross-node
  writes (convergence is eventual; no quorum).
- The network transport itself — Mímisbrunnr assumes Mettā provides
  peer reach and rides on it for metadata sync.
- Authentication, identity, and access control — these are provided by
  the wider Mettā security model; Mímisbrunnr enforces only what the OS
  hands it.
- Full-text search over blob contents, media transcoding, and
  near-duplicate dedup; only exact-content dedup via the BLAKE3 content
  hash is in scope.

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
