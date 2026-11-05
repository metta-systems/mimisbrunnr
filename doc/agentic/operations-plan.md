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

### 1. Plan and process hygiene

1. [TOOLING2](tooling-implementation-plan.md) — `just clippy` lints test targets.
2. [SPEC1](specification-implementation-plan.md), [SPEC2](specification-implementation-plan.md) — spec free of process language.

### 2. Native layouts that need no new allocation

Independent of each other; any order.

- [INDEX1](index-implementation-plan.md) — four-field range key.
- [INDEX2](index-implementation-plan.md), [INDEX3](index-implementation-plan.md) — ordered and ranked tag stores.
- [INDEX7](index-implementation-plan.md) — value-spill table.
- [BTREE8](btree-implementation-plan.md) — codec-transparent region read.
- [ONTOLOGY2](ontology-implementation-plan.md) — storage errors.
- [ONTOLOGY1](ontology-implementation-plan.md) — single tag-id allocator (after ONTOLOGY-D1).
- [ALLOCATION2](allocation-implementation-plan.md) — freelist seeding at format time.

### 3. Live tree machinery

1. [BTREE4](btree-implementation-plan.md) — covered key ranges (after BTREE-D2).
2. [JOURNAL2](journal-implementation-plan.md) — WAL trim respects pins.
3. [BTREE5](btree-implementation-plan.md) — append-on-flush with whiteouts (after BTREE-D1).
4. [ENGINE1](engine-implementation-plan.md) — long-lived nodes per tree.
5. [BTREE6](btree-implementation-plan.md), then [JOURNAL3](journal-implementation-plan.md) — `FormatPromote` emit and replay.
6. [JOURNAL4](journal-implementation-plan.md) — checkpoint cadence driver.
7. [SUBSCRIPTION2](subscriptions-implementation-plan.md) — per-subscription COW check.

### 4. Copy-on-write allocation

1. [ALLOCATION1](allocation-implementation-plan.md) — sub-bucket regions (after ALLOCATION-D1).
2. [ENGINE3](engine-implementation-plan.md) — COW commit.
3. [ALLOCATION3](allocation-implementation-plan.md) — generation check on read.
4. [BTREE7](btree-implementation-plan.md), then [ENGINE2](engine-implementation-plan.md) — compaction.
5. [METADATA1](metadata-implementation-plan.md), then [METADATA2](metadata-implementation-plan.md) — multi-level radix tables.
6. [INDEX4](index-implementation-plan.md) — KV index full capacity.
7. [ENGINE4](engine-implementation-plan.md) — retire fixed index-zone areas.
8. [SUBSCRIPTION1](subscriptions-implementation-plan.md) — externalized subscription bitmaps.
9. [ALLOCATION4](allocation-implementation-plan.md), [ALLOCATION5](allocation-implementation-plan.md), [ALLOCATION6](allocation-implementation-plan.md) — per-disk allocation trees.
10. [METADATA3](metadata-implementation-plan.md), [METADATA4](metadata-implementation-plan.md) — metadata wire-form cleanup; [METADATA5](metadata-implementation-plan.md) — overflow records (after SPEC3).
11. [INDEX6](index-implementation-plan.md) — footprint benchmark.

### 5. Bitmap representation

[INDEX-D1](index-decisions.md), then [INDEX5](index-implementation-plan.md). Must land before snapshots and cluster sync; may move earlier if large pools are needed sooner.

### 6. Blob zone and legacy blob retirement

1. [BLOB1](blob-implementation-plan.md), [BLOB2](blob-implementation-plan.md), [BLOB5](blob-implementation-plan.md).
2. [ENGINE5](engine-implementation-plan.md) — backpointers in the engine.
3. [BLOB3](blob-implementation-plan.md), [BLOB4](blob-implementation-plan.md) — chunked objects, selective chunking.
4. [BLOB6](blob-implementation-plan.md), [UNIX1](unix-implementation-plan.md), [ENGINE6](engine-implementation-plan.md) (after ENGINE-D1), [ENGINE7](engine-implementation-plan.md), then [ENGINE8](engine-implementation-plan.md) — no legacy blob.

### 7. Snapshots

[SNAPSHOT1](snapshots-implementation-plan.md) through [SNAPSHOT6](snapshots-implementation-plan.md).

### 8. Reconcile and allocation completeness

[RECONCILE1](reconcile-implementation-plan.md) through [RECONCILE8](reconcile-implementation-plan.md), with [ALLOCATION7](allocation-implementation-plan.md), [ALLOCATION8](allocation-implementation-plan.md) and [POOL6](pool-implementation-plan.md) (disk evacuation); then [POOL2](pool-implementation-plan.md).

### 9. Cluster sync

[CLUSTER1](cluster-implementation-plan.md) through [CLUSTER6](cluster-implementation-plan.md).

### Parallel tracks

These touch disjoint code and can run whenever their prerequisites hold:

- **Encryption:** [ENCRYPTION1](encryption-implementation-plan.md)–[ENCRYPTION4](encryption-implementation-plan.md), [ENCRYPTION6](encryption-implementation-plan.md) any time; [ENCRYPTION5](encryption-implementation-plan.md) after BLOB1.
- **Journal flags:** [JOURNAL5](journal-implementation-plan.md), [JOURNAL6](journal-implementation-plan.md).
- **Query and SQL:** [QUERY1](query-implementation-plan.md)–[QUERY8](query-implementation-plan.md) (QUERY5 after INDEX2).
- **Subscriptions:** [SUBSCRIPTION3](subscriptions-implementation-plan.md)–[SUBSCRIPTION6](subscriptions-implementation-plan.md).
- **Ontology:** [ONTOLOGY3](ontology-implementation-plan.md), [ONTOLOGY4](ontology-implementation-plan.md).
- **Pool:** [POOL1](pool-implementation-plan.md), [POOL3](pool-implementation-plan.md), [POOL4](pool-implementation-plan.md), [POOL5](pool-implementation-plan.md).
- **Unix and FUSE:** [UNIX2](unix-implementation-plan.md)–[UNIX7](unix-implementation-plan.md).
- **Zone growth:** [ENGINE9](engine-implementation-plan.md).
- **Tooling:** [TOOLING3](tooling-implementation-plan.md)–[TOOLING8](tooling-implementation-plan.md); TOOLING3 early, since later end-to-end acceptance criteria use it.
- **Specification:** [SPEC3](specification-implementation-plan.md)–[SPEC23](specification-implementation-plan.md) as their decisions resolve; SPEC21 (trim struct bodies) scope by scope as code lands.
- **Blob:** [BLOB7](blob-implementation-plan.md).
