# Query and SQL — implementation plan

Design: [query-design.md](query-design.md) · Decisions: [query-decisions.md](query-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Operators

- [ ] **QUERY1 — `Contains`.** Substring index; `QueryExecutor` no longer returns `UnsupportedCmpOp`, and SQL `LIKE '%x%'` executes. Prerequisite: QUERY-D1.
- [ ] **QUERY2 — `Prefix` without a linear scan.** Prerequisite: QUERY-D1.
- [ ] **QUERY3 — `Ne` through a per-attribute-key bitmap** instead of a tag-index union universe. Prerequisite: QUERY-D1.
- [ ] **QUERY4 — `Related` through a reverse relation index.** Prerequisite: QUERY-D1.

## SQL

- [ ] **QUERY5 — `ORDER BY`** through ordered collections. Prerequisite: [INDEX2](index-implementation-plan.md).
- [ ] **QUERY6 — `GROUP BY` beyond `COUNT(*)`, and `HAVING`.**
- [ ] **QUERY7 — Multi-column projections and `SELECT DISTINCT`.**
- [ ] **QUERY8 — `LIKE` with internal wildcards** (`'mid%dle'`, `_`).
