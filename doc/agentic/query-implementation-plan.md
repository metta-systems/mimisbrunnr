# Query and SQL — implementation plan

Design: [query-design.md](query-design.md) · Decisions: [query-decisions.md](query-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Operators

- [ ] **QUERY1 — `Contains`.** Through n-gram postings on keys that declare them, otherwise a scan of the key's dictionary entries comparing full values (INDEX-DESIGN§4.2); `QueryExecutor` no longer returns `UnsupportedCmpOp`, and SQL `LIKE '%x%'` executes. Prerequisites: [INDEX12](index-implementation-plan.md); [INDEX19](index-implementation-plan.md) for the n-gram path. Acceptance: on a key with and one without `ngram`, `Contains` matches a full scan, including patterns shorter than n and texts longer than 14 bytes.
- [ ] **QUERY12 — Subtree and path-pattern queries on hierarchical keys.** An `Under(key, path)` operator (SQL `unix_path LIKE '/usr/lib/%'` maps onto it) through the nearest roll-ups; `Contains` across separators by the split rule (INDEX-DESIGN§4.2). Prerequisites: [INDEX18](index-implementation-plan.md), QUERY1. Acceptance: subtree results match a full scan for a 100 000-path tree; `Contains("lib/libc")` finds exactly the paths with a component ending in `lib` followed by one starting with `libc`.
- [ ] **QUERY2 — Range and prefix through the dictionary.** `Lt`/`Le`/`Gt`/`Ge`/`Prefix` scan the term dictionary between normalised bounds (INDEX-DESIGN§4.2), comparing full values only at non-exact boundary entries. Prerequisite: [INDEX12](index-implementation-plan.md). Acceptance: range and prefix results match a full scan for Int, Float, Timestamp and Text (including texts longer than 14 bytes sharing a prefix).
- [ ] **QUERY3 — `Ne`.** By the mechanism QUERY-D1 selects. Prerequisites: QUERY-D1, [INDEX12](index-implementation-plan.md).
- [ ] **QUERY4 — `Related` through relation-term postings**, in both directions (forward from forward entries). Prerequisite: [INDEX13](index-implementation-plan.md).

## Encoding

- [ ] **QUERY11 — Positional `Query` encoding.** `Query` and `CmpOp` encode as the positional CBOR pinned by SPEC25 (variant index first, `Value` operands through INDEX9), replacing the derived `Serialize` wherever a query reaches the disk. Prerequisites: [SPEC25](specification-implementation-plan.md), [INDEX9](index-implementation-plan.md). Acceptance: fixed expected bytes for one query of every variant; decode rejects an unknown variant index with a defined error.

## SQL

- [ ] **QUERY5 — `ORDER BY`** through ordered collections. Prerequisite: [INDEX15](index-implementation-plan.md).
- [ ] **QUERY6 — `GROUP BY` beyond `COUNT(*)`, and `HAVING`.** Groups by intersecting the result with each KV-index bitmap of the group key; `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `COUNT(DISTINCT attr)` (QUERY-DESIGN "Post-operations"). Prerequisite: QUERY7. Acceptance: `SELECT artist, COUNT(*), AVG(bpm) FROM objects WHERE tag = 'electronic' GROUP BY artist HAVING COUNT(*) > 5` matches a hand-computed result.
- [ ] **QUERY7 — Multi-column projections and `SELECT DISTINCT`.** `SqlOutput` gains rows of attribute values read from the forward index.
- [ ] **QUERY8 — `LIKE` with internal wildcards** (`'mid%dle'`, `_`), as a post-filter over the pushed-down result when no index answers it.
- [ ] **QUERY9 — `ORDER BY` an attribute.** Range-index walk when the attribute is range-indexed, otherwise forward-index fetch and sort; bounded heap under `LIMIT`. Prerequisite: QUERY7. Acceptance: `ORDER BY year DESC LIMIT 3` over indexed and unindexed attributes returns the same order as a full sort.
- [ ] **QUERY10 — Relation traversal and transitive closure in SQL.** Prerequisites: QUERY4, QUERY-D2. Acceptance: the provenance query from the software-development example (ONTOLOGY-DESIGN "Example module", [ontology-design.md](ontology-design.md)) returns the image's full input set.
