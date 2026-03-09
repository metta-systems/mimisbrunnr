The existing index structures already form something very close to a columnar database engine. The gap between the current query algebra and SQL is mostly syntactic, plus a few missing operators.

## What Already Maps Directly

The current query algebra covers SQL's `WHERE` clause almost completely:

```sql
-- Current query algebra:
HasTag(tag("electronic"))              →  WHERE tags @> '{electronic}'
HasAttr { key: "year", op: Eq, 2024 }  →  WHERE year = 2024
And(vec![...])                         →  WHERE ... AND ...
Or(vec![...])                          →  WHERE ... OR ...
Not(...)                               →  WHERE NOT ...
HasAttr { key: "size", op: Gt, 1MB }   →  WHERE size > 1048576
```

The roaring bitmap engine is already doing what a database query executor does — evaluating predicates and intersecting result sets. What's missing is everything *around* the WHERE clause.

## What SQL Adds

```sql
-- Things the current query algebra can't express:

SELECT name, artist, year FROM objects    -- projection (return specific attrs)
WHERE tags @> '{music}'
ORDER BY year DESC                        -- ordering
LIMIT 50 OFFSET 100                       -- pagination

GROUP BY artist                           -- aggregation
HAVING count(*) > 10

SELECT a.name, b.name                     -- joins (across relations)
FROM objects a
JOIN relations r ON a.id = r.subject
JOIN objects b ON r.target = b.id
WHERE r.predicate = 'derived-from'

SELECT DISTINCT artist FROM objects       -- distinct values
WHERE tags @> '{electronic}'

COUNT(*), SUM(size), AVG(bpm)             -- aggregate functions
```

These fall into five categories, each with different implementation cost:

### 1. Projection (SELECT specific attributes) — Trivial

The forward index already stores all assertions per object. Projection is just filtering which fields to return:

```rust
// Current: query returns Vec<ObjectId>
// With projection: query returns Vec<Row>

struct Row {
    id: ObjectId,
    columns: Vec<(String, Value)>,  // only requested attributes
}

fn project(objects: &RoaringBitmap, columns: &[TagId]) -> Vec<Row> {
    objects.iter().map(|obj_id| {
        let assertions = forward_index.get(obj_id);
        let cols = columns.iter().filter_map(|col| {
            assertions.iter().find_map(|a| match a {
                Assertion::Attr { key, value } if key == col => Some(value.clone()),
                _ => None,
            })
        }).collect();
        Row { id: ObjectId(obj_id as u64), columns: cols }
    }).collect()
}
```

No new index needed. The cost is O(result_set × columns) — reading from the forward index you already have.

### 2. ORDER BY — Moderate

Two cases:

**Order by an indexed attribute** (range B+ tree exists): the index already stores values in order. Walk the B+ tree in order, intersect each entry with the WHERE result bitmap:

```rust
fn order_by_indexed(
    result: &RoaringBitmap,
    attr: TagId,
    direction: SortDir,
) -> Vec<ObjectId> {
    let iter = range_index.scan(attr, ..);  // already sorted
    let iter = match direction {
        SortDir::Asc => iter,
        SortDir::Desc => iter.rev(),
    };
    
    // Filter to only objects in our result set
    iter.filter(|(_, _, obj_id)| result.contains(*obj_id as u32))
        .map(|(_, _, obj_id)| ObjectId(obj_id))
        .collect()
}
```

**Order by a non-indexed attribute**: must fetch values from the forward index, then sort in memory. O(n log n) on the result set. Fine for reasonable result sizes, needs a limit for large ones.

### 3. LIMIT / OFFSET — Trivial

Just truncate the result iterator. With ORDER BY, this becomes "top-K" which can use a heap instead of full sort:

```rust
fn top_k(
    result: &RoaringBitmap,
    attr: TagId,
    k: usize,
    direction: SortDir,
) -> Vec<ObjectId> {
    // BinaryHeap with capacity k — O(n log k) instead of O(n log n)
    let mut heap = BoundedHeap::new(k);
    for obj_id in result.iter() {
        let value = forward_index.get_attr(obj_id, attr);
        heap.push((value, obj_id));
    }
    heap.into_sorted_vec()
}
```

### 4. GROUP BY / Aggregations — Moderate

This requires iterating the result set and grouping by attribute values. No new index needed, but benefits from the KV equality index for knowing which distinct values exist:

```rust
fn group_by(
    result: &RoaringBitmap,
    group_attr: TagId,
    aggregates: &[Aggregate],
) -> Vec<GroupRow> {
    // Get all distinct values for this attribute that appear in the result
    let distinct_values = kv_index.values_for_key(group_attr);
    
    distinct_values.iter().filter_map(|value| {
        // Intersect result with bitmap for this (key, value) pair
        let group_bitmap = kv_index.get(group_attr, value);
        let group = result & group_bitmap;
        
        if group.is_empty() { return None; }
        
        let agg_values = aggregates.iter().map(|agg| {
            match agg {
                Aggregate::Count => Value::Int(group.len() as i64),
                Aggregate::Sum(attr) => {
                    let sum: i64 = group.iter()
                        .filter_map(|id| forward_index.get_attr_int(id, *attr))
                        .sum();
                    Value::Int(sum)
                }
                Aggregate::Avg(attr) => {
                    let (sum, count) = group.iter()
                        .filter_map(|id| forward_index.get_attr_float(id, *attr))
                        .fold((0.0, 0), |(s, c), v| (s + v, c + 1));
                    Value::Float(if count > 0 { sum / count as f64 } else { 0.0 })
                }
                Aggregate::Min(attr) | Aggregate::Max(attr) => {
                    // If range-indexed, can read directly from B+ tree
                    // Otherwise, scan forward index
                    scan_minmax(&group, *attr, matches!(agg, Aggregate::Max(_)))
                }
            }
        }).collect();
        
        Some(GroupRow { key: value.clone(), aggregates: agg_values })
    }).collect()
}
```

The bitmap intersection trick is what makes this fast: `GROUP BY artist` doesn't scan every object — it intersects the result bitmap with each `(artist, value)` bitmap. If there are 500 distinct artists in the result set, that's 500 bitmap ANDs at ~1 μs each = 500 μs total.

### 5. JOINs (Relations) — The Interesting One

The relation index (derived-from, links, bundles, member-of) already supports what SQL joins express. The question is syntax:

```sql
-- "What source files were used to build the release executable?"
SELECT src.name, src.lang
FROM objects exe
JOIN relations r ON exe.id = r.subject AND r.predicate = 'derived-from'
JOIN objects src ON r.target = src.id
WHERE exe.name = 'vesper'
  AND exe.tags @> '{executable, stripped}'
  AND src.tags @> '{source}'
```

In the current relation index, this is:

```rust
// 1. Find the executable
let exe = query(And(vec![
    HasAttr { key: "name", op: Eq, value: Text("vesper") },
    HasTag(tag("executable")),
    HasTag(tag("stripped")),
]));  // → bitmap with 1 object

// 2. Follow derived-from relation
let sources = relation_forward.get(exe.first(), tag("derived-from"));
// → bitmap of source objects

// 3. Filter to actual source files
let result = sources & query(HasTag(tag("source")));

// 4. Project name and lang
project(&result, &[tag("name"), tag("lang")])
```

A SQL engine would compile the SQL into exactly this sequence of operations. The relation index does the heavy lifting — no need for a traditional hash join or merge join.

**Multi-hop joins** (transitive closure over relations) are the one case where SQL's recursive CTEs (`WITH RECURSIVE`) map to graph traversal:

```sql
-- Full dependency tree of the release image
WITH RECURSIVE deps AS (
    SELECT id FROM objects WHERE name = 'vesper-0.1.0-rpi4.img'
    UNION
    SELECT r.target FROM deps d
    JOIN relations r ON d.id = r.subject
    WHERE r.predicate IN ('derived-from', 'links', 'bundles')
)
SELECT * FROM deps;
```

This needs iterative bitmap expansion:

```rust
fn transitive_closure(
    start: &RoaringBitmap,
    predicates: &[TagId],
) -> RoaringBitmap {
    let mut visited = start.clone();
    let mut frontier = start.clone();
    
    loop {
        let mut next_frontier = RoaringBitmap::new();
        for obj_id in frontier.iter() {
            for pred in predicates {
                let targets = relation_forward.get(obj_id, *pred);
                next_frontier |= &targets;
            }
        }
        next_frontier -= &visited;  // don't revisit
        
        if next_frontier.is_empty() { break; }
        
        visited |= &next_frontier;
        frontier = next_frontier;
    }
    
    visited
}
```

## The SQL Dialect

Keep it close to SQLite's dialect — widely known, no need for PostgreSQL-level complexity. But adapt the type system to Mímisbrunnr's data model:

```sql
-- Objects are the only "table", but you query them as if they were
-- a schemaless table with dynamic columns

-- Simple tag query
SELECT id, name FROM objects
WHERE HAS TAG 'electronic'
  AND year = 2024;

-- HAS TAG is syntactic sugar for tag membership
-- Attribute access is direct column reference (from forward index)

-- Ontology-aware query: IS A follows implications
SELECT id, name FROM objects
WHERE IS A 'audio'        -- matches mp3, flac, opus, wav...
  AND artist = 'Aphex Twin';

-- Aggregation
SELECT artist, COUNT(*) as tracks, AVG(bpm) as avg_bpm
FROM objects
WHERE HAS TAG 'electronic'
GROUP BY artist
HAVING COUNT(*) > 5
ORDER BY tracks DESC;

-- Relations as joins
SELECT src.name, src.lang
FROM objects exe
JOIN RELATION 'derived-from' ON exe.id = subject
JOIN objects src ON target = src.id
WHERE exe.name = 'vesper'
  AND exe HAS TAG 'executable';

-- Transitive closure (recursive relation traversal)
SELECT name, depth FROM objects
WHERE id IN (
    CLOSURE OF 'vesper-0.1.0-rpi4.img'
    OVER 'derived-from', 'links', 'bundles'
);

-- Path projection queries
SELECT unix_path, mode FROM objects
IN CONTEXT 'rpi4-sdcard'
ORDER BY unix_path;

-- Subscription as a query (streaming result set)
SUBSCRIBE TO
    SELECT id, name FROM objects
    WHERE project = 'vesper' AND HAS TAG 'source'
    ON CHANGE content, created, deleted
    WITH DEBOUNCE 200ms;

-- Collection ordering
SELECT name FROM objects
IN COLLECTION 'playlist:workout'
ORDER BY POSITION;    -- ordered collection sequence

-- Cross-object statistics
SELECT
    COUNT(*) as total_files,
    SUM(size) as total_bytes,
    COUNT(DISTINCT project) as projects
FROM objects
WHERE HAS TAG 'source';

-- Faceted exploration
SELECT tag, COUNT(*) as count
FROM TAGS OF (
    SELECT id FROM objects WHERE HAS TAG 'electronic'
)
ORDER BY count DESC
LIMIT 20;
```

### Extensions Beyond Standard SQL

A few Mímisbrunnr-specific extensions that don't exist in standard SQL but map naturally:

```sql
-- Tag algebra (already exists in the query engine)
HAS TAG 'x'              -- tag membership
HAS ALL TAGS ('x', 'y')  -- AND
HAS ANY TAG ('x', 'y')   -- OR
NOT HAS TAG 'x'          -- exclusion
IS A 'x'                 -- ontology-aware (follows implications)

-- Relation traversal
JOIN RELATION 'predicate' ON subject/target
CLOSURE OF <start> OVER <predicates>   -- transitive closure

-- Collection semantics
IN COLLECTION 'tag-name'               -- ordered collection
ORDER BY POSITION                       -- collection sequence order

-- Path projection
IN CONTEXT 'context-name'              -- unix path projection

-- Streaming queries
SUBSCRIBE TO <query> ON CHANGE <events> WITH DEBOUNCE <duration>

-- Mutation (not just queries)
TAG <object> WITH 'tag1', 'tag2';
UNTAG <object> FROM 'tag1';
SET <object> attr = value;
```

## Implementation Architecture

The SQL layer sits above the existing query engine — it compiles SQL into the existing bitmap operations:

```
SQL text
    │
    ▼
┌──────────────┐
│ SQL Parser   │  pest, nom, or sqlparser-rs crate
│ (text→AST)   │  
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ Planner      │  AST → LogicalPlan → PhysicalPlan
│              │  Rewrites, predicate pushdown
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ Compiler     │  PhysicalPlan → sequence of bitmap ops
│              │  + project + sort + aggregate
└──────┬───────┘
       │
       ▼
┌──────────────────────────────────────────────┐
│ Existing Mímisbrunnr query engine            │
│ (bitmap intersection, range scan, forward    │
│  index lookup, relation traversal)           │
└──────────────────────────────────────────────┘
```

The planner is where optimization happens. Key rewrites:

```rust
enum PhysicalOp {
    // Leaf: fetch a bitmap from an index
    TagBitmap(TagId),
    KvBitmap(TagId, Value),
    RangeScan(TagId, Range<Value>),
    
    // Combine bitmaps
    Intersect(Vec<PhysicalOp>),    // AND
    Union(Vec<PhysicalOp>),        // OR
    Difference(Box<PhysicalOp>, Box<PhysicalOp>),  // AND NOT
    
    // Post-filter (for predicates not in any index)
    Filter(Box<PhysicalOp>, Predicate),
    
    // Projection
    Project(Box<PhysicalOp>, Vec<TagId>),
    
    // Sort
    Sort(Box<PhysicalOp>, TagId, SortDir),
    TopK(Box<PhysicalOp>, TagId, SortDir, usize),
    
    // Aggregate
    GroupBy(Box<PhysicalOp>, TagId, Vec<Aggregate>),
    
    // Relation traversal
    RelationJoin(Box<PhysicalOp>, TagId, RelationDir),
    TransitiveClosure(Box<PhysicalOp>, Vec<TagId>),
    
    // Limit/offset
    Limit(Box<PhysicalOp>, usize, usize),
}
```

### Predicate Pushdown

The planner pushes predicates as deep as possible — into bitmap operations rather than post-filtering:

```sql
SELECT name FROM objects
WHERE HAS TAG 'source'
  AND lang = 'rust'
  AND year > 2023
  AND component LIKE 'kernel%';
```

Becomes:

```
1. TagBitmap("source")              ← from tag inverted index
2. KvBitmap("lang", "rust")         ← from KV equality index
3. RangeScan("year", 2024..)        ← from range B+ tree
4. Intersect(1, 2, 3)               ← bitmap AND, ~3 μs
5. Filter(4, component LIKE 'k%')   ← post-filter (LIKE not indexed)
6. Project(5, ["name"])             ← forward index lookup
```

Steps 1-4 narrow the result to maybe 200 objects using index-only operations. Step 5 only scans those 200, not millions.

## Implementation Approach

Use the `sqlparser-rs` crate for parsing — it already handles standard SQL syntax. Only the planner and compiler need to be custom:

```rust
use sqlparser::parser::Parser;
use sqlparser::dialect::GenericDialect;

fn execute_sql(sql: &str) -> Result<QueryResult> {
    // 1. Parse
    let dialect = MimisbrunnrDialect::new();  // extends GenericDialect
    let ast = Parser::parse_sql(&dialect, sql)?;
    
    // 2. Plan
    let logical = logical_plan(&ast)?;
    let physical = optimize(logical)?;
    
    // 3. Execute against existing indexes
    let bitmap_result = execute_physical(&physical)?;
    
    // 4. Project + format results
    let rows = project_and_format(&bitmap_result, &physical)?;
    
    Ok(rows)
}
```

The total new code is roughly: parser adapter (~500 lines), planner/optimizer (~2000 lines), compiler to bitmap ops (~1000 lines). The execution engine already exists — it's the bitmap intersection machinery.

## What NOT to Implement

Keep it simple. Specifically avoid:

| Feature | Why skip it |
|---|---|
| Subqueries in SELECT | Complexity explosion, marginal utility |
| Window functions | OVER/PARTITION BY — complex, rarely needed here |
| Stored procedures | This is a filesystem, not a database server |
| Foreign keys / constraints | The ontology already handles constraints |
| UPDATE ... SET (SQL DML) | Use `mimir tag/set` — SQL is for queries |
| Views | Subscriptions already serve this purpose |
| Multi-table FROM | Objects are the only table; relations use JOIN RELATION |
| EXPLAIN | Nice to have, defer to v2 |

Mutations stay in the imperative API (`mimir tag`, `mimir set`). SQL is the query language, not the data manipulation language. This avoids the impedance mismatch of trying to express tag operations in SQL's row-oriented DML.

## The Result

A practical SQL layer for Mímisbrunnr is roughly 3000–4000 lines of Rust on top of the existing query engine. It doesn't need new storage structures, new indexes, or new on-disk formats. It's a syntax layer that compiles down to the bitmap operations you already have, plus projection/sort/aggregate as post-processing on result sets.

The `sqlparser-rs` crate does the heavy parsing. The custom extensions (`HAS TAG`, `IS A`, `JOIN RELATION`, `CLOSURE OF`, `IN COLLECTION`, `SUBSCRIBE TO`) need grammar additions but follow the existing crate's pattern.
