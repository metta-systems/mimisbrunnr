# Query and SQL — open decisions

Design: [query-design.md](query-design.md) · Plan: [query-implementation-plan.md](query-implementation-plan.md)

## QUERY-D1 — `HasKey` and `Ne` over the term index

Prefix and range scans are dictionary scans, `Contains` uses n-gram postings over distinct values where the key declares them (INDEX-DESIGN§4.2), and `Related` reads relation-term postings. **`HasKey`** (has any value of the key) and **`Ne`** (has the key, with a different value: `HasKey − Eq v`) need the set of objects carrying a key. Options: (a) a per-key presence term `(key, *)` posted on every attribute write; (b) union the postings of every dictionary entry of the key — for a hierarchical key, the root nodes' subtrees through their roll-ups. (a) adds a posting on every write; (b) costs a scan per query. Blocks [QUERY3](query-implementation-plan.md).

## QUERY-D2 — SQL spelling of Mímisbrunnr-specific operations

The SQL dialect is user-visible surface. Proposed capabilities, each to be spelled in the function style of `direct_tag` / `related` (QUERY-DESIGN "SQL layer"), as a keyword extension, or not at all:

- relation traversal from a selected set (`JOIN RELATION 'derived-from' ON subject` was one proposal), forward and reverse;
- transitive closure over predicates (`CLOSURE OF <start> OVER 'derived-from', 'links', 'bundles'`, or `WITH RECURSIVE` mapped onto closure);
- collection membership in sequence order (`IN COLLECTION playlist = 'workout' ORDER BY POSITION`);
- faceting (`SELECT tag, COUNT(*) FROM TAGS OF (<select>) ORDER BY 2 DESC LIMIT 20`);
- a streaming subscription (`SUBSCRIBE TO <select> ON CHANGE content, created, deleted WITH DEBOUNCE 200ms`), which would register a subscription ([subscriptions-design.md](subscriptions-design.md));
- `EXPLAIN` as SQL, alongside `--explain`.

Keyword forms need a custom `sqlparser` dialect. Blocks [QUERY10](query-implementation-plan.md); shapes [QUERY5](query-implementation-plan.md).

## QUERY-D3 — Projecting multi-valued attributes

An object may carry several values of one key (two `playlist` values, several `unix-path`s). `SELECT name, playlist` is unspecified for them. Options: (a) one row per value combination, as an unnested join would give; (b) one row per object with the column holding an array; (c) one row per object with the first value in key order, the rest reachable through an explicit unnest function. (a) multiplies rows and changes `COUNT(*)`; (b) needs an array type in `SqlOutput` and its renderers. Shapes [QUERY7](query-implementation-plan.md).

## QUERY-D4 — Projecting derived attributes

A derived attribute (`created-month`, `size-class`, a bucket over an attribute) is in postings only; the forward entry has no value to read. Options: (a) recompute it through the object's closure under the current rules (INDEX-DESIGN§2.4) when projected; (b) answer only from postings, by intersecting the result with the key's dictionary entries (as `GROUP BY` does); (c) reject projecting derived keys and allow them only in `WHERE` and `GROUP BY`. Shapes [QUERY7](query-implementation-plan.md) and [QUERY6](query-implementation-plan.md).

## QUERY-D5 — Qualified tag names in SQL

Tags whose bare name is ambiguous are reachable only by qualified name (`systems.metta.music:album`, ONTOLOGY-DESIGN "Tags and attribute keys"), which is not a valid SQL identifier; nor are names with `-` (`unix-path`). Decide the spelling: quoted identifiers (`"systems.metta.music:album"`, `"unix-path"`), a mapping of `-` to `_` for bare names, string-valued forms (`tag = 'music:album'`, `attr('music:album') = 'X'`), or a combination — and what an ambiguous bare name does (error naming the candidates, as the ontology does). Shapes [QUERY7](query-implementation-plan.md).

## Future work

- SQL subqueries in `WHERE` (`id IN (SELECT …)`); relation traversal is QUERY-D2 above.
- SQL `INSERT` / `UPDATE` / `DELETE` as a thin wrapper over engine mutations.
- Precomputed tag co-occurrence (per-pair sketches or cached top-N tags) to speed up faceted exploration beyond O(number of tags) per step.
