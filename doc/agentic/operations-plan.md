# Operations plan

The order in which plan items across all scopes should land, by dependency. Tasks themselves live only in each scope's implementation plan; this file holds their sequence. Workspace conventions: [operations-design.md](operations-design.md). Process decisions: [operations-decisions.md](operations-decisions.md).

## Scopes

| Scope | Covers | Plan |
|---|---|---|
| btree | Region envelope, sorted runs, packed codec, compaction | [btree](btree-implementation-plan.md) |
| journal | WAL, pins, checkpoint cadence | [journal](journal-implementation-plan.md) |
| allocation | Buckets, generation-checked refs, freespace, discard | [allocation](allocation-implementation-plan.md) |
| engine | Commit, root pointer, live trees, legacy blob retirement | [engine](engine-implementation-plan.md) |
| metadata | Object, location, backpointer tables | [metadata](metadata-implementation-plan.md) |
| index | Forward, tag, KV, range, chunk indices; bitmap representation | [index](index-implementation-plan.md) |
| ontology | Tag authority, modules, policy | [ontology](ontology-implementation-plan.md) |
| subscriptions | Watch engine | [subscriptions](subscriptions-implementation-plan.md) |
| query | Query operators and SQL | [query](query-implementation-plan.md) |
| pool | Multi-disk pools and placement | [pool](pool-implementation-plan.md) |
| blob | Blob zone, chunking, compression | [blob](blob-implementation-plan.md) |
| encryption | Ciphers and keys | [encryption](encryption-implementation-plan.md) |
| snapshots | Key-level snapshots | [snapshots](snapshots-implementation-plan.md) |
| reconcile | Background work items, copy GC, autotier | [reconcile](reconcile-implementation-plan.md) |
| cluster | Multi-node convergence | [cluster](cluster-implementation-plan.md) |
| unix | Path projections and FUSE | [unix](unix-implementation-plan.md) |
| specification | The system design itself, and the format sections as documents | [specification](specification-implementation-plan.md) |
| tooling | analyze, populate, test targets, source markers | [tooling](tooling-implementation-plan.md) |

## Sequence

Steps 1–7 replace the prototype's shortcuts with the specified binary formats and a pool that assembles from its devices, before the device target. Each step's items keep their stated prerequisites.

### 1. Format decisions

Each fixes a layout that later steps write; decide them first.

- [OPERATIONS-D4](operations-decisions.md) — checksum algorithm.
- [INDEX-D1](index-decisions.md) — object ids in bitmaps.
- [BTREE-D1](btree-decisions.md), [BTREE-D2](btree-decisions.md) — whiteouts, separator keys wider than 16 bytes.
- [ALLOCATION-D1](allocation-decisions.md) — sub-bucket regions.
- [ENGINE-D1](engine-decisions.md) — oplog persistence.

### 2. Binary formats that need no new allocation

Independent of each other unless an order is given.

- **Encodings** (FORMAT-DESIGN§1.6): [SPEC25](specification-implementation-plan.md) and [INDEX9](index-implementation-plan.md) first; then [JOURNAL8](journal-implementation-plan.md) (binary WAL payloads), [QUERY11](query-implementation-plan.md) and [ONTOLOGY10](ontology-implementation-plan.md); after QUERY11, [SUBSCRIPTION7](subscriptions-implementation-plan.md) and [POOL5](pool-implementation-plan.md) (native placement-rules tree).
- [SPEC24](specification-implementation-plan.md) — no packed on-disk structs.
- [JOURNAL7](journal-implementation-plan.md) — one checksum algorithm (after OPERATIONS-D4).
- [POOL3](pool-implementation-plan.md), then [POOL7](pool-implementation-plan.md) — pool state through the root pointer; pool assembled from its devices, no `pool.toml`.
- [METADATA3](metadata-implementation-plan.md) — packed-key backpointer table.
- [INDEX1](index-implementation-plan.md) — four-field range key.
- [INDEX2](index-implementation-plan.md), [INDEX3](index-implementation-plan.md) — ordered and ranked tag stores.
- [INDEX7](index-implementation-plan.md) — value-spill table.
- [INDEX8](index-implementation-plan.md) — tag bitmaps beyond one page.
- [ENGINE11](engine-implementation-plan.md) — typed chunking defaults in the superblock.
- [BTREE8](btree-implementation-plan.md) — codec-transparent region read.
- [ONTOLOGY2](ontology-implementation-plan.md) — storage errors.
- [ONTOLOGY1](ontology-implementation-plan.md) — single tag-id allocator (after ONTOLOGY-D1).
- [ALLOCATION2](allocation-implementation-plan.md) — freelist seeding at format time.

### 3. Bitmap representation

[INDEX5](index-implementation-plan.md) after INDEX-D1. It changes every on-disk bitmap, so it lands before the tree machinery that writes them.

### 4. Live tree machinery

1. [BTREE4](btree-implementation-plan.md) — covered key ranges (after BTREE-D2).
2. [JOURNAL2](journal-implementation-plan.md) — WAL trim respects pins.
3. [BTREE5](btree-implementation-plan.md) — append-on-flush with whiteouts (after BTREE-D1).
4. [ENGINE1](engine-implementation-plan.md) — long-lived nodes per tree.
5. [BTREE6](btree-implementation-plan.md), then [JOURNAL3](journal-implementation-plan.md) — `FormatPromote` emit and replay.
6. [JOURNAL4](journal-implementation-plan.md) — checkpoint cadence driver.
7. [SUBSCRIPTION2](subscriptions-implementation-plan.md) — per-subscription COW check.

### 5. Copy-on-write allocation

1. [ALLOCATION1](allocation-implementation-plan.md) — sub-bucket regions (after ALLOCATION-D1).
2. [ENGINE3](engine-implementation-plan.md) — COW commit.
3. [ALLOCATION3](allocation-implementation-plan.md) — generation check on read.
4. [BTREE7](btree-implementation-plan.md), then [ENGINE2](engine-implementation-plan.md) — compaction.
5. [METADATA1](metadata-implementation-plan.md), then [METADATA2](metadata-implementation-plan.md) — multi-level radix tables.
6. [INDEX4](index-implementation-plan.md) — KV index full capacity.
7. [ENGINE4](engine-implementation-plan.md) — retire fixed index-zone areas.
8. [SUBSCRIPTION1](subscriptions-implementation-plan.md) — externalized subscription bitmaps.
9. [ALLOCATION4](allocation-implementation-plan.md), [ALLOCATION5](allocation-implementation-plan.md), [ALLOCATION6](allocation-implementation-plan.md) — per-disk allocation trees.
10. [METADATA4](metadata-implementation-plan.md) — metadata wire-form cleanup; [METADATA5](metadata-implementation-plan.md) — overflow records (after SPEC3).
11. [INDEX6](index-implementation-plan.md) — footprint benchmark.

### 6. Blob zone and legacy blob retirement

1. [BLOB1](blob-implementation-plan.md), [BLOB2](blob-implementation-plan.md), [BLOB5](blob-implementation-plan.md).
2. [ENGINE5](engine-implementation-plan.md) — backpointers in the engine; then [METADATA6](metadata-implementation-plan.md) — blob reclaim and tombstone expiry (after METADATA-D3).
3. [BLOB3](blob-implementation-plan.md), [BLOB4](blob-implementation-plan.md) — chunked objects, selective chunking.
4. [BLOB6](blob-implementation-plan.md), [UNIX1](unix-implementation-plan.md), [ENGINE6](engine-implementation-plan.md) (after ENGINE-D1), [ENGINE7](engine-implementation-plan.md), then [ENGINE8](engine-implementation-plan.md) — no legacy blob.

### 7. Device target

[ENGINE10](engine-implementation-plan.md) — the `no_std` engine over a sector-level `BlockDevice` (operations-design "Deployment targets"), once steps 2–6 have fixed the formats it reads.

### 8. Snapshots

[SNAPSHOT1](snapshots-implementation-plan.md) through [SNAPSHOT6](snapshots-implementation-plan.md).

### 9. Reconcile and allocation completeness

[RECONCILE1](reconcile-implementation-plan.md) through [RECONCILE8](reconcile-implementation-plan.md), with [ALLOCATION7](allocation-implementation-plan.md), [ALLOCATION8](allocation-implementation-plan.md) and [POOL6](pool-implementation-plan.md) (disk evacuation); then [POOL2](pool-implementation-plan.md).

### 10. Cluster sync

[CLUSTER1](cluster-implementation-plan.md) through [CLUSTER6](cluster-implementation-plan.md).

### Parallel tracks

These touch disjoint code and can run whenever their prerequisites hold:

- **Encryption:** [ENCRYPTION1](encryption-implementation-plan.md)–[ENCRYPTION4](encryption-implementation-plan.md), [ENCRYPTION6](encryption-implementation-plan.md) any time; [ENCRYPTION5](encryption-implementation-plan.md) after BLOB1.
- **Journal flags:** [JOURNAL5](journal-implementation-plan.md), [JOURNAL6](journal-implementation-plan.md).
- **Query and SQL:** [QUERY1](query-implementation-plan.md)–[QUERY10](query-implementation-plan.md) (QUERY5 after INDEX2; QUERY6 and QUERY9 after QUERY7; QUERY10 after QUERY4 and QUERY-D2).
- **Subscriptions:** [SUBSCRIPTION3](subscriptions-implementation-plan.md)–[SUBSCRIPTION6](subscriptions-implementation-plan.md).
- **Ontology:** [ONTOLOGY3](ontology-implementation-plan.md), [ONTOLOGY4](ontology-implementation-plan.md), [ONTOLOGY5](ontology-implementation-plan.md); [ONTOLOGY6](ontology-implementation-plan.md), then [ONTOLOGY7](ontology-implementation-plan.md), then [ONTOLOGY8](ontology-implementation-plan.md) and [ONTOLOGY9](ontology-implementation-plan.md).
- **Pool:** [POOL1](pool-implementation-plan.md), [POOL4](pool-implementation-plan.md).
- **Unix and FUSE:** [UNIX2](unix-implementation-plan.md)–[UNIX6](unix-implementation-plan.md), [UNIX8](unix-implementation-plan.md); [UNIX9](unix-implementation-plan.md) after UNIX-D3; [UNIX10](unix-implementation-plan.md) after BLOB1.
- **Zone growth:** [ENGINE9](engine-implementation-plan.md).
- **Tooling:** [TOOLING3](tooling-implementation-plan.md)–[TOOLING8](tooling-implementation-plan.md); TOOLING3 early, since later end-to-end acceptance criteria use it.
- **Specification:** [SPEC3](specification-implementation-plan.md)–[SPEC23](specification-implementation-plan.md) as their decisions resolve; SPEC21 (trim struct bodies) scope by scope as code lands.
- **Blob:** [BLOB7](blob-implementation-plan.md).
