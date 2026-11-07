# Cluster sync — implementation plan

Design: [cluster-design.md](cluster-design.md) · Decisions: [cluster-decisions.md](cluster-decisions.md) · Order: [operations-plan.md](operations-plan.md)

Prerequisites for the whole scope: [SNAPSHOT6](snapshots-implementation-plan.md), [RECONCILE2](reconcile-implementation-plan.md).

- [ ] **CLUSTER1 — Peer registry.** `ClusterPeers` tree at `cluster_peers_root` and the peer state machine. Prerequisite: CLUSTER-D1.
- [ ] **CLUSTER2 — Sync op stream.** Serialise mutations into peer-bound `SyncOp`s (SPECIFICATION-DESIGN§10.4); apply remote ops with `HybridClock::observe`. Prerequisite: CLUSTER-D2.
- [ ] **CLUSTER3 — Bitmap delta sync** (SPECIFICATION-DESIGN§10.5).
- [ ] **CLUSTER4 — Sync modes and bandwidth budget** (SPECIFICATION-DESIGN§10.6, SPECIFICATION-DESIGN§10.8).
- [ ] **CLUSTER5 — Hydration.** Engine support for `ContentPresence` (SPECIFICATION-DESIGN§10.3). Prerequisite: CLUSTER-D3.
- [ ] **CLUSTER8 — Ontology ops by uid.** A bundle carries its `OntologyPut` / `OntologyDelete` entries with `TagId`s and rule ids replaced by uids, applied before its other ops; the receiver allocates `TagId`s for tags new to it and reworks postings in the same commit; concurrent edits merge by union of provenance, scalar fields last-writer-wins by HLC (ONTOLOGY-DESIGN "Cluster propagation"). Prerequisites: CLUSTER2, [ONTOLOGY11](ontology-implementation-plan.md), [ONTOLOGY15](ontology-implementation-plan.md). Acceptance: two pools whose `TagId`s differ for the same module tag converge on a module install and an ad-hoc tag, and answer the same tag query identically; a module removed on one node and kept by another installer on the other ends with the union of installers.
- [ ] **CLUSTER7 — No local ids on the wire.** Sync payloads and posting deltas name tags by `TagUid` and terms by value, never by `TagId` or `TermId`; receivers intern them into their own dictionary (cluster-design "Selected design"). Prerequisites: CLUSTER2, [INDEX12](index-implementation-plan.md). Acceptance: two nodes with independently created pools converge on an attribute and both answer the same equality query with the same objects.
- [ ] **CLUSTER6 — Multi-node convergence suite.** Two nodes converge while each stays authoritative for its own writes; a node answers a tag query for an object it did not write without fetching blobs ([acceptance scenarios](operations-design.md#acceptance-scenarios); evidence kind `L4`).
