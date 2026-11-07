# Pool management — open decisions

Design: [pool-design.md](pool-design.md) · Plan: [pool-implementation-plan.md](pool-implementation-plan.md)

## POOL-D1 — Metadata replica count

Every tree node is written `metadata_replicas` (R) times on different disks, and node pointers carry R `BlockRef`s (ENGINE-DESIGN§1.2). Decide the default R for a multi-disk pool (for example 2 when at least two disks exist, else 1), whether R may change after format (a change rewrites every inner node and radix slot, since slot width is 16 × R bytes), and how R relates to replicating metadata across the cluster, where every node already holds all metadata (SPECIFICATION-DESIGN§8.2).

## POOL-D2 — Quotas

The original requirements list filesystem quotas; the design has none. Decide whether quotas exist and what they bound (bytes or objects per tag query, per ontology module, per Mettā principal), and whether they are enforced at write time or reported by reconcile. Without paths, a quota is naturally a placement-style rule over a query.

## Future work

- None recorded.
