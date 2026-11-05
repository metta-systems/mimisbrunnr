# Query and SQL — implementation plan

Design: [query-design.md](query-design.md) · Decisions: [query-decisions.md](query-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Operators

- [ ] **QUERY1 — `Contains`.** Substring index; `QueryExecutor` no longer returns `UnsupportedCmpOp`, and SQL `LIKE '%x%'` executes. Prerequisite: QUERY-D1.
- [ ] **QUERY2 — `Prefix` without a linear scan.** Prerequisite: QUERY-D1.
- [ ] **QUERY3 — `Ne` through a per-attribute-key bitmap** instead of a tag-index union universe. Prerequisite: QUERY-D1.
- [ ] **QUERY4 — `Related` through a reverse relation index.** Prerequisite: QUERY-D1.

## SQL

- [ ] **QUERY5 — `ORDER BY`** through ordered collections. Prerequisite: [INDEX2](index-implementation-plan.md).
- [ ] **QUERY6 — `GROUP BY` beyond `COUNT(*)`, and `HAVING`.** Groups by intersecting the result with each KV-index bitmap of the group key; `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `COUNT(DISTINCT attr)` (QUERY-DESIGN "Post-operations"). Prerequisite: QUERY7. Acceptance: `SELECT artist, COUNT(*), AVG(bpm) FROM objects WHERE tag = 'electronic' GROUP BY artist HAVING COUNT(*) > 5` matches a hand-computed result.
- [ ] **QUERY7 — Multi-column projections and `SELECT DISTINCT`.** `SqlOutput` gains rows of attribute values read from the forward index.
- [ ] **QUERY8 — `LIKE` with internal wildcards** (`'mid%dle'`, `_`), as a post-filter over the pushed-down result when no index answers it.
- [ ] **QUERY9 — `ORDER BY` an attribute.** Range-index walk when the attribute is range-indexed, otherwise forward-index fetch and sort; bounded heap under `LIMIT`. Prerequisite: QUERY7. Acceptance: `ORDER BY year DESC LIMIT 3` over indexed and unindexed attributes returns the same order as a full sort.
- [ ] **QUERY10 — Relation traversal and transitive closure in SQL.** Prerequisites: QUERY4, QUERY-D2. Acceptance: the provenance query from the software-development example (ONTOLOGY-DESIGN "Example module", [ontology-design.md](ontology-design.md)) returns the image's full input set.
