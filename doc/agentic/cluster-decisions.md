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

## Future work

- Remote storage backends (SPECIFICATION-DESIGN§10.9), including an IPFS-style frontend developed outside Mettā.
