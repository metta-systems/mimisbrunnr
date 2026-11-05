# Cluster sync — implementation plan

Design: [cluster-design.md](cluster-design.md) · Decisions: [cluster-decisions.md](cluster-decisions.md) · Order: [operations-plan.md](operations-plan.md)

Prerequisites for the whole scope: [SNAPSHOT6](snapshots-implementation-plan.md), [RECONCILE2](reconcile-implementation-plan.md).

- [ ] **CLUSTER1 — Peer registry.** `ClusterPeers` tree at `cluster_peers_root` and the peer state machine. Prerequisite: CLUSTER-D1.
- [ ] **CLUSTER2 — Sync op stream.** Serialise mutations into peer-bound `SyncOp`s (SPECIFICATION-DESIGN§10.4); apply remote ops with `HybridClock::observe`. Prerequisite: CLUSTER-D2.
- [ ] **CLUSTER3 — Bitmap delta sync** (SPECIFICATION-DESIGN§10.5).
- [ ] **CLUSTER4 — Sync modes and bandwidth budget** (SPECIFICATION-DESIGN§10.6, SPECIFICATION-DESIGN§10.8).
- [ ] **CLUSTER5 — Hydration.** Engine support for `ContentPresence` (SPECIFICATION-DESIGN§10.3). Prerequisite: CLUSTER-D3.
- [ ] **CLUSTER6 — Multi-node convergence suite.** Two nodes converge while each stays authoritative for its own writes; a node answers a tag query for an object it did not write without fetching blobs ([acceptance scenarios](operations-design.md#acceptance-scenarios); evidence kind `L4`).
