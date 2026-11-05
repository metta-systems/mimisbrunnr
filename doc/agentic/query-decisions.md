# Query and SQL — open decisions

Design: [query-design.md](query-design.md) · Plan: [query-implementation-plan.md](query-implementation-plan.md)

## QUERY-D1 — On-disk shape of the query-support indices

`Contains` needs a substring index, `Prefix` a sorted range or trie, `Ne` a per-attribute-key bitmap, `Related` a `(predicate, target) → oids` reverse index. None is defined in IMPLEMENTATION. Decide which of them are persisted structures (needing spec sections, `BtreeKind` slots and footprint entries) and which are derived in memory at boot. Blocks [QUERY1](query-implementation-plan.md)–[QUERY4](query-implementation-plan.md).

## QUERY-D2 — SQL spelling of Mímisbrunnr-specific operations

The SQL dialect is user-visible surface. Proposed capabilities, each to be spelled in the function style of `isa` / `related` (QUERY-DESIGN "SQL layer"), as a keyword extension, or not at all:

- relation traversal from a selected set (`JOIN RELATION 'derived-from' ON subject` was one proposal), forward and reverse;
- transitive closure over predicates (`CLOSURE OF <start> OVER 'derived-from', 'links', 'bundles'`, or `WITH RECURSIVE` mapped onto closure);
- collection membership in sequence order (`IN COLLECTION 'playlist:workout' ORDER BY POSITION`);
- path-context projection (`IN CONTEXT 'rpi4-sdcard'` exposing `unix_path`, `mode` as columns);
- faceting (`SELECT tag, COUNT(*) FROM TAGS OF (<select>) ORDER BY 2 DESC LIMIT 20`);
- a streaming subscription (`SUBSCRIBE TO <select> ON CHANGE content, created, deleted WITH DEBOUNCE 200ms`), which would register a subscription ([subscriptions-design.md](subscriptions-design.md));
- `EXPLAIN` as SQL, alongside `--explain`.

Keyword forms need a custom `sqlparser` dialect. Blocks [QUERY10](query-implementation-plan.md); shapes [QUERY5](query-implementation-plan.md).

## Future work

- SQL subqueries in `WHERE` (`id IN (SELECT …)`); relation traversal is QUERY-D2 above.
- SQL `INSERT` / `UPDATE` / `DELETE` as a thin wrapper over engine mutations.
- Precomputed tag co-occurrence (per-pair sketches or cached top-N tags) to speed up faceted exploration beyond O(number of tags) per step.
