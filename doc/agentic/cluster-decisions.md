# Cluster sync — open decisions

Design: [cluster-design.md](cluster-design.md) · Plan: [cluster-implementation-plan.md](cluster-implementation-plan.md)

## CLUSTER-D1 — Unique NodeId assignment

ObjectIds embed a 16-bit `NodeId` (SPECIFICATION-DESIGN§2.1). Decide how a node obtains a cluster-unique id (operator-assigned, derived from Mettā identity, coordinated on join) and what happens on collision. Blocks [CLUSTER1](cluster-implementation-plan.md).

## CLUSTER-D2 — Conflict resolution rules

SPECIFICATION-DESIGN§10.7 names conflict resolution without specifying it. Define per-assertion-kind rules (tags, attributes, relations, deletions, ontology changes) under concurrent writes from different nodes. Blocks [CLUSTER2](cluster-implementation-plan.md).

## CLUSTER-D3 — Hydration states

SPECIFICATION-DESIGN§10.3 has both `Partial` and `Hydrating` (with progress). Decide whether both states are needed and what each means for reads. Blocks [CLUSTER5](cluster-implementation-plan.md).

## CLUSTER-D4 — Sync transport

DESIGN notes suggest iroh as the sync transport; the target-system boundary ([operations-design.md](operations-design.md#boundary)) places the transport outside Mímisbrunnr (Mettā provides peer reach). Decide whether Mímisbrunnr defines a transport binding at all, and if so which.

## CLUSTER-D5 — `value_hash` secrets across nodes

Each pool's KV index is keyed by its own `value_hash` secret ([INDEX-D2](index-decisions.md)). If peers exchange anything keyed by `value_hash` (KV entries, bitmap deltas for KV keys), every node must hash with the same secret. Options:

- **(a) Cluster-wide secret.** Fixed when the cluster forms; a joining node receives it wrapped for that node through the Mettā security model before it builds its KV index; a node's pool created before joining rebuilds its KV index under the cluster secret.
- **(b) Hashes never cross nodes.** Sync carries attribute values, never `value_hash`; each node rehashes with its own secret. Bitmap-delta sync (SPECIFICATION-DESIGN§10.5) then covers tag bitmaps only, and KV state converges through the assertion ops.

(b) keeps each node's secret local and needs no key distribution; (a) lets KV bitmaps sync as deltas. Blocks [CLUSTER7](cluster-implementation-plan.md); shapes [CLUSTER3](cluster-implementation-plan.md).

## Future work

- Remote storage backends (SPECIFICATION-DESIGN§10.9), including an IPFS-style frontend developed outside Mettā.
