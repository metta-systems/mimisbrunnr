# Pool management — open decisions

Design: [pool-design.md](pool-design.md) · Plan: [pool-implementation-plan.md](pool-implementation-plan.md)

## POOL-D1 — Metadata replica count

Every tree node is written `metadata_replicas` (R) times on different disks, and node pointers carry R `BlockRef`s (ENGINE-DESIGN§1.2). Decide the default R for a multi-disk pool (for example 2 when at least two disks exist, else 1), whether R may change after format (a change rewrites every inner node and radix slot, since slot width is 16 × R bytes), and how R relates to replicating metadata across the cluster, where every node already holds all metadata (SPECIFICATION-DESIGN§8.2).

## POOL-D2 — Quotas

The original requirements list filesystem quotas; the design has none. Decide whether quotas exist and what they bound (bytes or objects per tag query, per ontology module, per Mettā principal), and whether they are enforced at write time or reported by reconcile. Without paths, a quota is naturally a placement-style rule over a query.

## POOL-D3 — Time-triggered rules

Placement and presence rules follow mutations through the match index (SUBSCRIPTIONS-DESIGN "Evaluation"), which is consulted only when an object changes. Rules that depend on time passing — `AutoTier` (tier by access age), the presence rule `Evict` (drop local chunks after `unused_ns` without access, SPECIFICATION-DESIGN§8.3), and any rule comparing a timestamp with "now" (e.g. modified more than 30 days ago) — are never re-checked for an object nobody touches. The query language has no now-relative predicate yet either.

Direction: **bucket terms with time wheels**, so that several granularities are served.

- Each time-based rule names a timestamp (`modified`, `created`, access time per [METADATA-D7](metadata-decisions.md)) and a threshold. The timestamp has `Bucket` terms at the granularities in use (hour, day, week…), created internally when no ontology rule declares them (ONTOLOGY-DESIGN "Rules and constraints").
- A time wheel per granularity fires at each bucket boundary. When a boundary passes, the objects that just crossed a rule's threshold are the postings of one bucket term — the one that has aged out — so a trigger is one posting read per rule per tick, and the matching objects enqueue reconcile work.
- Thresholds finer than the coarsest fitting bucket use a deadline wheel: an object entering the boundary bucket gets an entry keyed `(deadline, oid, rule)`, popped by the engine's `step` when due.

To decide: the granularities offered and which wheel serves each; whether the wheel's pending deadlines are persisted (a tree) or rebuilt from bucket postings at mount; how a now-relative predicate is written in the query language; how ticks missed while the pool was offline are caught up (one catch-up read per elapsed bucket); and the cost bound per tick. Blocks `AutoTier` and `Evict` ([POOL4](pool-implementation-plan.md), [POOL9](pool-implementation-plan.md), [RECONCILE5](reconcile-implementation-plan.md)).

## POOL-D4 — Bounds on pool composition

Only id widths bound how many nodes and disks a pool has: `NodeId` (65 536) and `disk_id` (65 536 ever added, ENGINE-DESIGN§1.2). Nothing says how many may be present at once, nor whether a removed disk's `DiskDescriptor` is kept (as a tombstone naming the never-reused id) or deleted once its evacuation completes. Every structure holding one entry per node or disk — object roots, disk descriptors, cluster peers, and the checkpoint entry if [ENGINE-D2](engine-decisions.md) (c) carries them — needs a worst case. To decide: the maximum cluster node count, the maximum present disks per pool, and the fate of removed disks' descriptors.

## POOL-D5 — Pool event log and statistics

The superblock no longer counts mounts or records the last mount time, since it is rewritten only on clean unmount and when the journal list changes (ENGINE-DESIGN§1.1). Mount history and similar operational statistics need a home that is written anyway. Sketch, not decided:

- **Counters** in the pool state record (POOL-DESIGN§1.2), advanced in the checkpoint each event already writes: `mounts`, `unclean_mounts` (a mount whose replay found entries after the newest checkpoint), `last_mount_ns`, `last_clean_unmount_ns`, and per disk `mounts_seen` in its descriptor if per-device history matters.
- **An event log** generalising today's `errors: [ErrorEntry]` (newest 64): `PoolEvent = [lsn, time_ns, kind, detail]` with kinds for mount, clean unmount, unclean recovery, disk added, removed or failed, head switch, rule rework started and finished, and the reconcile errors of RECONCILE-DESIGN§1.7; kept as the newest N, with counters surviving the window.
- **Where it lives:** the pool state record while it stays small, or its own bounded tree if N grows; bounded by [RECONCILE-D1](reconcile-decisions.md) (`detail` cap) and part of [ENGINE-D2](engine-decisions.md)'s checkpoint size.

To decide: the counter set, the event kinds, N, and whether `brunnr status` reports from it.

## Future work

- None recorded.
