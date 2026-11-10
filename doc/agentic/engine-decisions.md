# Engine — open decisions

Design: [engine-design.md](engine-design.md) · Plan: [engine-implementation-plan.md](engine-implementation-plan.md)

## ENGINE-D2 — Tiny bounded state: separate trees or the checkpoint entry

Five trees hold a handful of small, bounded entries each — `PoolState` (1), `ObjectRoots` (one per origin node), `DiskDescriptors` (one per disk), `ClusterPeers` (one per peer), `ReconcileScan` (one per running scan) — yet each costs a root entry, a `BtreeKind`, and at least one 256 KiB region written R times. Options:

- (a) one `Catalog` B+ tree keyed `(catalog_kind, key)` holding all five;
- (b) keep five trees;
- (c) carry them as typed sections of the `Checkpoint` entry after the root list (ENGINE-DESIGN§1.2), rewritten whole at every checkpoint; the five trees, their regions, kinds and root entries go, administrative changes (POOL-DESIGN§1.2) land exactly where they live, and opening a pool knows its disks before loading any tree. `PlacementRules`, `Snapshots`, `Subscriptions` and `Ontology` stay trees.

Estimated checkpoint entry size under (c), with 64 errors of 64-byte detail and 300-byte scan records:

| Part | Each | Small pool: 1 node, 3 disks, R = 2 | Large pool: 64 nodes, 64 disks, R = 3 |
|---|---|---|---|
| `RootListHead` + ~18 `RootEntry` (`8 + 16R`) | 88 B + 40–72 B | 0.8 KiB | 1.1 KiB |
| Pool state (~60 B fixed + errors ~25 B + detail) | | 5.6 KiB | 5.6 KiB |
| Object roots (`16 + 16R`) | 48–80 B | 0.05 KiB | 4 KiB |
| Disk descriptors (`32 + 32R`) | 96–160 B | 0.3 KiB | 8 KiB |
| Cluster peers (layout undefined, ~200 B assumed) | | 0 | 12.6 KiB |
| Scan records | ~300 B | ≤ 1.2 KiB | 4.8 KiB |
| **Total** | | **~8 KiB** | **~36 KiB** |

(c) depends on bounds the format does not yet have: [JOURNAL-D2](journal-decisions.md) (an entry's payload is at most 64 KiB), [POOL-D4](pool-decisions.md) (how many nodes and disks), [RECONCILE-D1](reconcile-decisions.md) (scan parameters and error detail) and the peer record's layout ([CLUSTER-D7](cluster-decisions.md)). Decide once those are settled.

## Future work

- None recorded.
