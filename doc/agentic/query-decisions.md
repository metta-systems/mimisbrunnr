# Query and SQL — open decisions

Design: [query-design.md](query-design.md) · Plan: [query-implementation-plan.md](query-implementation-plan.md)

## QUERY-D1 — On-disk shape of the query-support indices

`Contains` needs a substring index, `Prefix` a sorted range or trie, `Ne` a per-attribute-key bitmap, `Related` a `(predicate, target) → oids` reverse index. None is defined in IMPLEMENTATION. Decide which of them are persisted structures (needing spec sections, `BtreeKind` slots and footprint entries) and which are derived in memory at boot. Blocks [QUERY1](query-implementation-plan.md)–[QUERY4](query-implementation-plan.md).

## Future work

- SQL `JOIN` and subqueries.
- SQL `INSERT` / `UPDATE` / `DELETE` as a thin wrapper over engine mutations.
- Precomputed tag co-occurrence (per-pair sketches or cached top-N tags) to speed up faceted exploration beyond O(number of tags) per step.
