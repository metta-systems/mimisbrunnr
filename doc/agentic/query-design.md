# Query and SQL — design

Scope: `crates/query` (`QueryExecutor`, `QueryParser`, `FacetedExplorer`), `crates/sql` (parser, planner, executor), `mimir query` / `mimir sql`.
Governing spec: SPECIFICATION-DESIGN§2.3 (query algebra), SPECIFICATION-DESIGN§5.6 (faceted exploration); the SQL layer is specified here.

Plan: [query-implementation-plan.md](query-implementation-plan.md) · Open decisions: [query-decisions.md](query-decisions.md) · Completed: [query-completed.md](query-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Every query operator evaluates through bitmap algebra over an index built for it, with no linear forward-index scans, and SQL is a thin, read-only front end over the same executor.

## Selected design

- Query evaluation is bitmap algebra over the indices in [index-design.md](index-design.md); the algebra is SPECIFICATION-DESIGN§2.3.
- **SQL is read-only.** `SELECT` maps onto the query algebra; mutations go through the engine API.
- `ORDER BY` over ordered collections reads the collection's sequence (INDEX-DESIGN§3.3).
- Query operators map onto the term index: `HasTag` reads postings, which hold derived members too (with `direct`, the result is filtered against forward entries); `HasAttr` with `Eq` is a dictionary lookup (how `HasKey` is answered is [QUERY-D1](query-decisions.md)), with `Lt`/`Le`/`Gt`/`Ge`/`Prefix` one dictionary scan; `Contains` intersects n-gram postings on keys that declare them and scans the key's dictionary entries otherwise; `Under` unions roll-up and node postings of a hierarchical key; `Related` reads the postings of the relation term; `Not` subtracts from the `_object` system label's postings (INDEX-DESIGN§4.2, SPECIFICATION-DESIGN§2.3).
- **Record fields are columns, not terms.** `size`, `created`, `modified` and `changed` filter the result after the indexed part of the query; to index them, the ontology declares a `Bucket` rule over the field (ONTOLOGY-DESIGN "Rules and constraints").
- Queries accept an optional `snapshot_id` once [snapshots-design.md](snapshots-design.md) lands.

## Evaluation

**Compile, then run.** A query names tags (by `TagUid` in its stored form, by name in text)
and values. Compilation resolves them once: tags to `TermId`s, attribute equalities to
dictionary lookups, ranges and prefixes to dictionary key ranges, `Contains` to n-gram term
sets. The compiled query is cached with the `rules_epoch` and the dictionary entries it read,
and recompiled when either changes; a name that did not resolve (a value with no dictionary
entry, a tag not installed) is recorded with it, and interning that value or installing that tag
also recompiles it (SUBSCRIPTIONS-DESIGN "Evaluation"). Terms derived by a rule still being reworked return the
partial result the rework has reached (INDEX-DESIGN§2.4). Subscriptions and placement rules keep compiled forms
resident. A query at an older snapshot compiles against that snapshot's name maps, built lazily
from its ontology records; `TagUid`s and ids are never reused, so only names can differ, and its
execution needs no closure tables (INDEX-DESIGN§2.4).

**Stream window by window.** The executor produces the result one 65 536-id window at a time
over posting containers (INDEX-DESIGN§3.2): for each window, intersections run smallest first
by dictionary cardinality, a window empty after any operand is skipped without reading the
rest, and `Not` reads the `_object` container of the window. `LIMIT` stops the stream; SQL
post-operations consume windows, not a materialised result. Memory is bounded by a few
containers per operand, whatever the result size — the property the device needs.

**Per-object evaluation.** The second question a query answers is whether *one* object
matches. An `ObjectView` holds what is known about one object without reading postings: its
forward entry, the closure of its direct terms (INDEX-DESIGN§2.4), its system labels and its
record fields. `matches(&CompiledQuery, &ObjectView) -> bool` evaluates the same compiled form
the set executor runs:

- `HasTag` tests the closure, `HasTag { direct }` the forward entry;
- `HasAttr`, ranges, prefixes and `Contains` compare the object's values of the key through the
  term table, with full values at non-exact boundaries as in a dictionary scan;
- `Under` walks the object's hierarchy node's parents (INDEX-DESIGN§4.2);
- `Related` tests the object's relation terms;
- record-field predicates (`size`, `created`, `modified`, `changed`) read the record;
- `And`, `Or` combine; `Not(q)` is `_object ∧ ¬q`.

It is the only per-object evaluator. Subscriptions, placement and hydration rules, the reconcile
worker's desired-state computation, the executor's `direct` filter and SQL's record-field filter
all call it, so an object matches a query in the same way wherever the question is asked. The two
evaluators agree by construction and by test: for any query `q` and object `o`,
`matches(q, o) ⇔ o ∈ execute(q)`.

Which persisted queries a mutation can affect is found through the match index
(SUBSCRIPTIONS-DESIGN "Evaluation"), shared by subscriptions and by placement and hydration
rules.

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
WHERE tag = 'released' AND language = 'rust' AND year > 2023 AND component LIKE 'k%nel';
```

```text
HasTag(released) ∧ HasAttr(language = rust) ∧ HasAttr(year > 2023)   postings; dictionary lookup; dictionary scan
  → filter component LIKE 'k%nel'                              over the ~200 survivors, not the pool
  → project name                                               forward index
```

### Surface

```sql
SELECT * FROM objects WHERE tag = 'electronic';
SELECT * FROM objects WHERE tag IN ('electronic', 'portable');
SELECT * FROM objects WHERE artist = 'Aphex Twin' AND year >= 2024 AND year < 2026;
SELECT * FROM objects WHERE name LIKE 'cargo%';            -- Prefix
SELECT * FROM objects WHERE name LIKE '%lock';             -- Contains
SELECT * FROM objects WHERE tag = 'electronics' AND NOT tag = 'discontinued';
SELECT * FROM objects WHERE tag = 'vehicle';               -- cars and trucks too (derived)
SELECT * FROM objects WHERE direct_tag('vehicle');         -- only objects tagged vehicle themselves
SELECT * FROM objects WHERE unix_path LIKE '/usr/lib/%';   -- Under on a hierarchical key
SELECT * FROM objects WHERE size > 1048576 AND tag = 'video';  -- size: record-field column, post-filter
SELECT * FROM objects WHERE related('member_of', 900);
SELECT * FROM objects WHERE tag = 'song' LIMIT 100 OFFSET 50;
SELECT COUNT(*) FROM objects WHERE tag = 'electronics';
```

`objects` is the only table: a schemaless table whose columns are the attribute keys, read from the forward index, plus the record-field columns `id`, `size`, `created`, `modified` and `changed`, read from the object record. Mímisbrunnr-specific predicates are functions (`direct_tag`, `related`) so the stock dialect parses them. Statements outside the surface and its planned post-operations return `SqlError::Unsupported`.

### Post-operations

Each operates on the result bitmap from the executor.

- **Projection** (`SELECT name, artist`): for each result object, read the requested attributes from its forward entry. O(result × columns); no new index ([QUERY7](query-implementation-plan.md)).
- **`ORDER BY` an attribute:** walk the attribute's dictionary entries in key order (reversed for `DESC`), emitting each entry's postings intersected with the result; when the result is much smaller than the attribute's value count, fetch each result object's value instead (forward entry, then term table) and sort, O(n log n) over the result. With `LIMIT k` the sort is a bounded heap, O(n log k) ([QUERY9](query-implementation-plan.md)). Collection order is [QUERY5](query-implementation-plan.md).
- **`LIMIT` / `OFFSET`:** truncate the ordered result.
- **`GROUP BY` / aggregates:** for each dictionary entry of the group key (one per distinct value), intersect its postings with the result; empty intersections drop out. `COUNT` is the intersection's cardinality; `SUM` / `AVG` read the attribute through each member's forward entry; `MIN` / `MAX` are the first and last dictionary entries whose postings meet the result. 500 groups cost about 500 bitmap ANDs, ~0.5 ms ([QUERY6](query-implementation-plan.md)).
- **Relation traversal:** following a predicate from the current set yields the targets' bitmap (forward direction from forward entries; reverse direction from the postings of the relation term `(predicate, target)`, [QUERY4](query-implementation-plan.md)), which further predicates intersect. The relation indices do the work of a join; no hash or merge join exists. "Sources of the release executable": find the executable, follow `derived-from`, intersect with `source`, project `name, lang`.
- **Transitive closure:** iterative frontier expansion — `next = ⋃ targets(frontier, predicates) − visited`, until empty — for full provenance or dependency trees ([QUERY10](query-implementation-plan.md)).

How relation traversal, closure, collection order and faceting are spelled in SQL is [QUERY-D2](query-decisions.md).

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
- **An `IsA` operator** (and SQL `isa()`) distinct from `HasTag`. Derived terms are posted, so `HasTag` already matches implied tags; `direct` covers the narrower question.
- **Record fields as attribute terms** (`size = 4096` interned per object). Millions of distinct sizes and times would become terms; fields are columns, and `Bucket` rules index coarse classes of them.
- **A separate physical-plan tree** (`PhysicalOp` with `TagBitmap`, `Intersect`, `Difference`, …) between planner and executor. The `Query` algebra is the pushed-down plan; only post-operations sit outside it.
