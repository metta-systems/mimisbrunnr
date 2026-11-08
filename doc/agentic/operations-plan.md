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
- [POOL-D1](pool-decisions.md) — metadata replica count (sets node-pointer width).
- [BLOB-D4](blob-decisions.md) — small-object bucket layout.

### 2. Binary formats that need no new allocation

Independent of each other unless an order is given.

- **Encodings** (FORMAT-DESIGN§1.6): [SPEC25](specification-implementation-plan.md) and [INDEX9](index-implementation-plan.md) first; then [JOURNAL8](journal-implementation-plan.md) (binary payloads, then [JOURNAL9](journal-implementation-plan.md): journal buckets and journal sets, after JOURNAL7), [QUERY11](query-implementation-plan.md) and [ONTOLOGY10](ontology-implementation-plan.md); after QUERY11, [SUBSCRIPTION7](subscriptions-implementation-plan.md) and [POOL5](pool-implementation-plan.md) (native placement-rules tree).
- [SPEC24](specification-implementation-plan.md) — no packed on-disk structs.
- [JOURNAL7](journal-implementation-plan.md) — one checksum algorithm (after OPERATIONS-D4).
- [POOL1](pool-implementation-plan.md) — one disk-descriptor tree (after ALLOCATION9); [POOL3](pool-implementation-plan.md) — pool state record (after SPEC25 and ENGINE12); then [POOL7](pool-implementation-plan.md) — pool assembled from its devices, no `pool.toml`.
- [METADATA3](metadata-implementation-plan.md) — packed-key backpointer table.
- [ENGINE12](engine-implementation-plan.md) — superblock and root list (after ALLOCATION9 and JOURNAL12); [ENGINE11](engine-implementation-plan.md) — typed chunking defaults in pool state (after POOL3).
- [BTREE9](btree-implementation-plan.md) — write-once regions with block-addressable runs; then [BTREE10](btree-implementation-plan.md) — positional page regions, and [BTREE8](btree-implementation-plan.md) — one run format for every tree.
- [ONTOLOGY2](ontology-implementation-plan.md) — storage errors.
- [ONTOLOGY10](ontology-implementation-plan.md) (per-record ontology tree, in the encodings group above), then [ONTOLOGY1](ontology-implementation-plan.md) — single tag-id allocator with ad-hoc tags in `local` — and [ONTOLOGY15](ontology-implementation-plan.md) — tag uids and name resolution; [ONTOLOGY11](ontology-implementation-plan.md) — ontology journal ops, after JOURNAL8.
- [ALLOCATION9](allocation-implementation-plan.md) — bucket-relative pointers and node pointers (after POOL-D1); [ALLOCATION10](allocation-implementation-plan.md) — 8-byte bucket entries, derived live sectors.
- [ALLOCATION2](allocation-implementation-plan.md) — freelist seeding at format time.

### 3. Term index

The term dictionary, postings and forward index replace every index structure, so they land before the tree machinery that writes them (INDEX-DESIGN).

1. [INDEX12](index-implementation-plan.md) — term dictionary, term table and interning (after INDEX9).
2. [INDEX13](index-implementation-plan.md) — postings containers (after BTREE9); then [INDEX14](index-implementation-plan.md) — forward index by `TermId`, direct terms only.
3. [INDEX15](index-implementation-plan.md) — sequences; [INDEX16](index-implementation-plan.md) — queries, faceting, FUSE and watch over containers.
4. [ONTOLOGY16](ontology-implementation-plan.md) — attribute flavours and key options (after INDEX15); then [INDEX18](index-implementation-plan.md) — hierarchy tries with roll-ups — and [INDEX19](index-implementation-plan.md) — n-gram postings over distinct values.
5. [METADATA7](metadata-implementation-plan.md) — `ObjectRecord` with content locator, one object table (after INDEX14).

### 4. Live tree machinery

1. [BTREE4](btree-implementation-plan.md) — separator keys for descent.
2. [JOURNAL2](journal-implementation-plan.md) — trim respects pins; [JOURNAL10](journal-implementation-plan.md) — group commit and durability levels.
3. [BTREE5](btree-implementation-plan.md) — append-on-flush with whiteouts.
4. [ENGINE1](engine-implementation-plan.md) — long-lived nodes per tree.
5. [JOURNAL4](journal-implementation-plan.md) — checkpoint cadence driver; then [ENGINE13](engine-implementation-plan.md) — step-driven engine.
6. [SUBSCRIPTION2](subscriptions-implementation-plan.md) — per-subscription COW check.

### 5. Copy-on-write allocation

1. [ALLOCATION1](allocation-implementation-plan.md) — sub-bucket regions.
2. [ENGINE3](engine-implementation-plan.md) — COW commit; then [JOURNAL12](journal-implementation-plan.md) — root in the journal.
3. [ALLOCATION3](allocation-implementation-plan.md) — resident generations and the dereference check.
4. [BTREE7](btree-implementation-plan.md), then [ENGINE2](engine-implementation-plan.md) — compaction.
5. [METADATA1](metadata-implementation-plan.md) — multi-level radix per origin node.
6. [ENGINE4](engine-implementation-plan.md) — retire zones and fixed areas.
7. [SUBSCRIPTION1](subscriptions-implementation-plan.md) — membership containers (after INDEX13).
8. [ALLOCATION4](allocation-implementation-plan.md), [ALLOCATION5](allocation-implementation-plan.md), [ALLOCATION6](allocation-implementation-plan.md) — per-disk allocation trees.
9. [METADATA4](metadata-implementation-plan.md) — metadata wire-form cleanup.
10. [INDEX6](index-implementation-plan.md) — footprint benchmark.

### 6. Blob zone and legacy blob retirement

1. [BLOB1](blob-implementation-plan.md), [BLOB2](blob-implementation-plan.md), [BLOB5](blob-implementation-plan.md); then [JOURNAL11](journal-implementation-plan.md) — one barrier per commit with extent verification.
2. [ENGINE5](engine-implementation-plan.md) — backpointers in the engine; [METADATA8](metadata-implementation-plan.md) — extent lists; then [METADATA6](metadata-implementation-plan.md) — blob reclaim and tombstone expiry (after METADATA-D3).
3. [BLOB3](blob-implementation-plan.md), [BLOB4](blob-implementation-plan.md) — chunked objects, selective chunking; then [BLOB8](blob-implementation-plan.md) — tiny objects (after BLOB-D4) and [BLOB9](blob-implementation-plan.md) — streaming objects.
4. [JOURNAL13](journal-implementation-plan.md) — optional retained history; then [BLOB6](blob-implementation-plan.md), [UNIX1](unix-implementation-plan.md), [ENGINE6](engine-implementation-plan.md), [ENGINE7](engine-implementation-plan.md), then [ENGINE8](engine-implementation-plan.md) — no legacy blob.

### 7. Device target

[ENGINE10](engine-implementation-plan.md) — the `no_std` engine over a sector-level `BlockDevice` (operations-design "Deployment targets"), once steps 2–6 have fixed the formats it reads.

### 8. Snapshots

[SNAPSHOT1](snapshots-implementation-plan.md) through [SNAPSHOT6](snapshots-implementation-plan.md).

### 9. Reconcile and allocation completeness

[RECONCILE1](reconcile-implementation-plan.md), [RECONCILE2](reconcile-implementation-plan.md), [RECONCILE9](reconcile-implementation-plan.md) (scans), then [RECONCILE3](reconcile-implementation-plan.md) through [RECONCILE8](reconcile-implementation-plan.md), [INDEX17](index-implementation-plan.md) (term reclamation), with [ALLOCATION7](allocation-implementation-plan.md), [ALLOCATION8](allocation-implementation-plan.md) and [POOL6](pool-implementation-plan.md) (disk evacuation); then [POOL2](pool-implementation-plan.md).

### 10. Cluster sync

[CLUSTER1](cluster-implementation-plan.md), [CLUSTER9](cluster-implementation-plan.md) (sync bundles), then [CLUSTER2](cluster-implementation-plan.md) (after CLUSTER-D2 and CLUSTER-D6), [CLUSTER4](cluster-implementation-plan.md), [CLUSTER5](cluster-implementation-plan.md), [CLUSTER7](cluster-implementation-plan.md), [CLUSTER8](cluster-implementation-plan.md), then [CLUSTER6](cluster-implementation-plan.md).

### Parallel tracks

These touch disjoint code and can run whenever their prerequisites hold:

- **Encryption:** [ENCRYPTION1](encryption-implementation-plan.md)–[ENCRYPTION4](encryption-implementation-plan.md) any time; [ENCRYPTION5](encryption-implementation-plan.md) after BLOB1.
- **Journal flags:** [JOURNAL5](journal-implementation-plan.md), [JOURNAL6](journal-implementation-plan.md).
- **Query and SQL:** [QUERY13](query-implementation-plan.md) (after INDEX14, ONTOLOGY16 and ONTOLOGY17), then [QUERY14](query-implementation-plan.md) (after INDEX16); [QUERY15](query-implementation-plan.md) after METADATA7; [QUERY1](query-implementation-plan.md)–[QUERY10](query-implementation-plan.md), [QUERY12](query-implementation-plan.md) (QUERY1–QUERY3 after INDEX12, QUERY1's n-gram path after INDEX19; QUERY12 after INDEX18 and QUERY1; QUERY4 after INDEX13; QUERY5 after INDEX15; QUERY6 and QUERY9 after QUERY7; QUERY10 after QUERY4 and QUERY-D2).
- **Subscriptions:** [SUBSCRIPTION3](subscriptions-implementation-plan.md) (after QUERY14), then [SUBSCRIPTION8](subscriptions-implementation-plan.md) (after SUBSCRIPTION1), then [SUBSCRIPTION4](subscriptions-implementation-plan.md)–[SUBSCRIPTION6](subscriptions-implementation-plan.md).
- **Ontology:** [ONTOLOGY6](ontology-implementation-plan.md) (after INDEX14), then [ONTOLOGY7](ontology-implementation-plan.md), [ONTOLOGY17](ontology-implementation-plan.md) (after METADATA7) and [ONTOLOGY12](ontology-implementation-plan.md); [ONTOLOGY14](ontology-implementation-plan.md) (after ONTOLOGY11 and ONTOLOGY15), then [ONTOLOGY5](ontology-implementation-plan.md), [ONTOLOGY8](ontology-implementation-plan.md), [ONTOLOGY9](ontology-implementation-plan.md), [ONTOLOGY3](ontology-implementation-plan.md); [ONTOLOGY4](ontology-implementation-plan.md) after ONTOLOGY-D2.
- **Pool:** [POOL4](pool-implementation-plan.md).
- **Unix and FUSE:** [UNIX2](unix-implementation-plan.md)–[UNIX5](unix-implementation-plan.md), [UNIX8](unix-implementation-plan.md); [UNIX9](unix-implementation-plan.md) after UNIX-D3; [UNIX10](unix-implementation-plan.md) after BLOB1 and METADATA-D6.
- **Tooling:** [TOOLING3](tooling-implementation-plan.md)–[TOOLING8](tooling-implementation-plan.md); TOOLING3 early, since later end-to-end acceptance criteria use it.
- **Specification:** [SPEC4](specification-implementation-plan.md)–[SPEC23](specification-implementation-plan.md) as their decisions resolve; SPEC21 (trim struct bodies) scope by scope as code lands.
- **Blob:** [BLOB7](blob-implementation-plan.md).
