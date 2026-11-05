# Pool management — open decisions

Design: [pool-design.md](pool-design.md) · Plan: [pool-implementation-plan.md](pool-implementation-plan.md)

## POOL-D1 — Index and metadata zone replicas on fast disks

SPECIFICATION-DESIGN§8.2 places the index and metadata zones on one disk. Decide whether the SSD tier also carries replicas of these zones within a node, and how that relates to their replication across the cluster.

## POOL-D2 — Quotas

The original requirements list filesystem quotas; the design has none. Decide whether quotas exist and what they bound (bytes or objects per tag query, per ontology module, per Mettā principal), and whether they are enforced at write time or reported by reconcile. Without paths, a quota is naturally a placement-style rule over a query.

## Future work

- None recorded.
