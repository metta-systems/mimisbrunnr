# Query and SQL — design

Scope: `crates/query` (`QueryExecutor`, `QueryParser`, `FacetedExplorer`), `crates/sql` (parser, planner, executor), `mimir query` / `mimir sql`.
Governing spec: SPECIFICATION-DESIGN§2.3 (query algebra), SPECIFICATION-DESIGN§5.6 (faceted exploration), [extension_SQL.md](../extension_SQL.md) (SQL layer).

Plan: [query-implementation-plan.md](query-implementation-plan.md) · Open decisions: [query-decisions.md](query-decisions.md) · Completed: [query-completed.md](query-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Every query operator evaluates through bitmap algebra over an index built for it, with no linear forward-index scans, and SQL is a thin, read-only front end over the same executor.

## Selected design

- Query evaluation is bitmap algebra over the indices in [index-design.md](index-design.md).
- **SQL is read-only.** `SELECT` maps onto the query algebra; mutations go through the engine API.
- `ORDER BY` over ordered collections reads the tag index `OrderedStore`.
- Queries accept an optional `snapshot_id` once [snapshots-design.md](snapshots-design.md) lands.
