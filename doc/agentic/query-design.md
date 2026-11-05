# Query and SQL — design

Scope: `crates/query` (`QueryExecutor`, `QueryParser`, `FacetedExplorer`), `crates/sql` (parser, planner, executor), `mimir query` / `mimir sql`.
Governing spec: SPECIFICATION-DESIGN§2.3 (query algebra), SPECIFICATION-DESIGN§5.6 (faceted exploration); the SQL layer is specified here.

Plan: [query-implementation-plan.md](query-implementation-plan.md) · Open decisions: [query-decisions.md](query-decisions.md) · Completed: [query-completed.md](query-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Every query operator evaluates through bitmap algebra over an index built for it, with no linear forward-index scans, and SQL is a thin, read-only front end over the same executor.

## Selected design

- Query evaluation is bitmap algebra over the indices in [index-design.md](index-design.md).
- **SQL is read-only.** `SELECT` maps onto the query algebra; mutations go through the engine API.
- `ORDER BY` over ordered collections reads the tag index `OrderedStore`.
- Queries accept an optional `snapshot_id` once [snapshots-design.md](snapshots-design.md) lands.

## SQL layer

The indices already behave like a columnar engine: the query algebra is SQL's `WHERE` clause, and bitmap intersection is what a database executor does with predicates. SQL adds what surrounds `WHERE` — projection, ordering, pagination, aggregation, relation traversal — as post-processing on the result bitmap. It needs no new storage structures and no on-disk format.

### Pipeline

```text
SQL text ─▶ SqlParser ─▶ SqlPlanner ─────────────────────────────▶ SqlEngine
            sqlparser    Query (pushed-down bitmap algebra)       QueryExecutor → bitmap
            Generic-     + post-operations                        → post-operations → rows
            Dialect
```

`SqlParser` accepts exactly one `SELECT` under `sqlparser`'s `GenericDialect`. `SqlPlanner` compiles the whole `WHERE` clause into one `Query`, which `QueryExecutor` evaluates through the indices, and records the post-operations the SQL layer applies itself. `explain` renders the plan (`mimir query --sql --explain`).

**Predicate pushdown.** Every predicate an index can answer goes into the `Query`; only the rest filters the narrowed result:

```sql
SELECT name FROM objects
WHERE tag = 'source' AND lang = 'rust' AND year > 2023 AND component LIKE 'k%nel';
```

```text
HasTag(source) ∧ HasAttr(lang = rust) ∧ HasAttr(year > 2023)   tag index, KV index, range index: ~3 µs
  → filter component LIKE 'k%nel'                              over the ~200 survivors, not the pool
  → project name                                               forward index
```

### Implemented surface

```sql
SELECT * FROM objects WHERE tag = 'electronic';
SELECT * FROM objects WHERE tag IN ('electronic', 'portable');
SELECT * FROM objects WHERE artist = 'Aphex Twin' AND year >= 2024 AND year < 2026;
SELECT * FROM objects WHERE name LIKE 'cargo%';            -- Prefix
SELECT * FROM objects WHERE name LIKE '%lock';             -- Contains
SELECT * FROM objects WHERE tag = 'electronics' AND NOT tag = 'discontinued';
SELECT * FROM objects WHERE isa('vehicle');                -- follows implications
SELECT * FROM objects WHERE related('member_of', 900);
SELECT * FROM objects WHERE tag = 'song' LIMIT 100 OFFSET 50;
SELECT COUNT(*) FROM objects WHERE tag = 'electronics';
```

`objects` is the only table: a schemaless table whose columns are attribute keys, read from the forward index. Mímisbrunnr-specific predicates are functions (`isa`, `related`) so the stock dialect parses them. Results are object ids or a count; everything else returns `SqlError::Unsupported`.

### Post-operations

Each operates on the result bitmap from the executor.

- **Projection** (`SELECT name, artist`): for each result object, read the requested attributes from its forward entry. O(result × columns); no new index ([QUERY7](query-implementation-plan.md)).
- **`ORDER BY` an attribute:** if the attribute is range-indexed, walk the range index in key order (reversed for `DESC`) keeping entries in the result bitmap; otherwise fetch values from the forward index and sort, O(n log n) over the result. With `LIMIT k` the sort is a bounded heap, O(n log k) ([QUERY9](query-implementation-plan.md)). Collection order is [QUERY5](query-implementation-plan.md).
- **`LIMIT` / `OFFSET`:** truncate the ordered result.
- **`GROUP BY` / aggregates:** for each distinct value of the group key in the KV index, intersect its bitmap with the result; empty intersections drop out. `COUNT` is the intersection's cardinality; `SUM` / `AVG` read the attribute from each member's forward entry; `MIN` / `MAX` read the range index when the attribute has one. 500 groups cost about 500 bitmap ANDs, ~0.5 ms ([QUERY6](query-implementation-plan.md)).
- **Relation traversal:** following a predicate from the current set yields the targets' bitmap (forward direction from forward entries; reverse direction from the reverse relation index, [QUERY4](query-implementation-plan.md)), which further predicates intersect. The relation indices do the work of a join; no hash or merge join exists. "Sources of the release executable": find the executable, follow `derived-from`, intersect with `source`, project `name, lang`.
- **Transitive closure:** iterative frontier expansion — `next = ⋃ targets(frontier, predicates) − visited`, until empty — for full provenance or dependency trees ([QUERY10](query-implementation-plan.md)).

How relation traversal, closure, collection order, path contexts and faceting are spelled in SQL is [QUERY-D2](query-decisions.md).

### Non-goals

| Feature | Why not |
|---|---|
| Subqueries in the `SELECT` list | Complexity out of proportion to use |
| Window functions (`OVER`, `PARTITION BY`) | Rarely needed over tagged objects |
| Stored procedures | A filesystem, not a database server |
| Foreign keys and constraints | The ontology owns constraints |
| Views | Subscriptions serve this purpose |
| Tables other than `objects` | Relations are traversed, not joined as tables |

### Rejected alternatives

- **A custom keyword grammar** (`HAS TAG 'x'`, `HAS ALL TAGS (…)`, `IS A 'x'`, `exe HAS TAG …`) in a forked dialect. Tag predicates are `tag = 'x'` / `tag IN (…)` and extensions are functions, so `GenericDialect` parses everything.
- **Mutation statements in SQL** (`TAG <obj> WITH …`, `UNTAG`, `SET <obj> attr = value`). SQL is read-only; mutations go through `mimir tag` / `mimir set` and the engine API.
- **A separate physical-plan tree** (`PhysicalOp` with `TagBitmap`, `Intersect`, `Difference`, …) between planner and executor. The `Query` algebra is the pushed-down plan; only post-operations sit outside it.
