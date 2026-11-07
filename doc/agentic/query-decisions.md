# Query and SQL — open decisions

Design: [query-design.md](query-design.md) · Plan: [query-implementation-plan.md](query-implementation-plan.md)

## QUERY-D1 — `Ne` over the term index

Prefix and range scans are dictionary scans, `Contains` uses n-gram postings over distinct values where the key declares them (INDEX-DESIGN§4.2), and `Related` reads relation-term postings. **`Ne`** (has the attribute, with a different value) remains undefined. Options: (a) a per-key presence term `(key, *)` posted on every attribute write, so `Ne v` = presence minus `Eq v`; (b) union the postings of every dictionary entry of the key, minus `Eq v` — for a hierarchical key, the root nodes' subtrees through their roll-ups. (a) adds a posting on every write; (b) costs a scan per query. Blocks [QUERY3](query-implementation-plan.md).

## QUERY-D2 — SQL spelling of Mímisbrunnr-specific operations

The SQL dialect is user-visible surface. Proposed capabilities, each to be spelled in the function style of `isa` / `related` (QUERY-DESIGN "SQL layer"), as a keyword extension, or not at all:

- relation traversal from a selected set (`JOIN RELATION 'derived-from' ON subject` was one proposal), forward and reverse;
- transitive closure over predicates (`CLOSURE OF <start> OVER 'derived-from', 'links', 'bundles'`, or `WITH RECURSIVE` mapped onto closure);
- collection membership in sequence order (`IN COLLECTION playlist = 'workout' ORDER BY POSITION`);
- path-context projection (`IN CONTEXT 'rpi4-sdcard'` exposing `unix_path`, `mode` as columns);
- faceting (`SELECT tag, COUNT(*) FROM TAGS OF (<select>) ORDER BY 2 DESC LIMIT 20`);
- a streaming subscription (`SUBSCRIBE TO <select> ON CHANGE content, created, deleted WITH DEBOUNCE 200ms`), which would register a subscription ([subscriptions-design.md](subscriptions-design.md));
- `EXPLAIN` as SQL, alongside `--explain`.

Keyword forms need a custom `sqlparser` dialect. Blocks [QUERY10](query-implementation-plan.md); shapes [QUERY5](query-implementation-plan.md).

## Future work

- SQL subqueries in `WHERE` (`id IN (SELECT …)`); relation traversal is QUERY-D2 above.
- SQL `INSERT` / `UPDATE` / `DELETE` as a thin wrapper over engine mutations.
- Precomputed tag co-occurrence (per-pair sketches or cached top-N tags) to speed up faceted exploration beyond O(number of tags) per step.
