# Allocation — implementation plan

Design: [allocation-design.md](allocation-design.md) · Decisions: [allocation-decisions.md](allocation-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Region allocation

- [ ] **ALLOCATION1 — Sub-bucket region allocation.** `alloc_region(kind)` claims a bucket on first use and returns successive 256 KiB sub-extents, then claims a new bucket; freeing a region lowers the bucket's `dirty_sectors`. Prerequisite: ALLOCATION-D1. Acceptance: four region allocations share one bucket, the fifth opens a new one; freeing a region decreases `dirty_sectors`. Unblocks [ENGINE3](engine-implementation-plan.md), [METADATA1](metadata-implementation-plan.md), [INDEX4](index-implementation-plan.md), [SUBSCRIPTION1](subscriptions-implementation-plan.md).
- [ ] **ALLOCATION2 — Seed the freelist at format time.** `PoolManager::create_pool` seeds every bucket past `bootstrap_buckets` as `Free` with generation 1 through `seed_freelist`. Acceptance: a freshly created pool's alloc table lists every non-bootstrap bucket as free.
- [ ] **ALLOCATION3 — Generation check on region read.** Region reads through a `BlockRef` validate the generation against the alloc table (ALLOCATION-DESIGN§1.3). Prerequisite: [ENGINE3](engine-implementation-plan.md) (generations only change once trees move). Acceptance: tampered generation is rejected with `StaleBlockRef`.

## Per-disk trees

- [ ] **ALLOCATION4 — Per-disk bucket-alloc table.** Rooted at `DiskDescriptorOnDisk.buckets_root`, key `bucket_no: u32`, packed-key codec; engine keeps one table per disk; `BucketAllocator::for_disk(disk_id)`. Prerequisite: ALLOCATION1. Acceptance: in a two-disk pool each disk's tree holds only its own buckets.
- [ ] **ALLOCATION5 — Per-disk freespace LRU.** Rooted at `freespace_root`, key `(fragmentation_band: u8, bucket_no: u32)`; allocator fast path scans band 0; fragmentation bands maintained on free (ALLOCATION-DESIGN§1.4). Prerequisite: ALLOCATION4.
- [ ] **ALLOCATION6 — Drop CBOR-wrapped POD values in allocation trees.** `BucketAllocEntry` and freespace keys lose their manual byte-image `Serialize` impls once ALLOCATION4 / ALLOCATION5 use the packed codec; `BucketAllocKey` and `FreespaceLruKey` proxies go away.

## Completeness

- [ ] **ALLOCATION7 — Per-disk write points.** Writes are segregated by data type and stream tag (ALLOCATION-DESIGN§1.5).
- [ ] **ALLOCATION8 — Discard.** A discard pass issues TRIM for buckets in `NeedDiscard` / `BUCKET_FLAG_NEEDS_DISCARD` (ALLOCATION-DESIGN§1.7).

Copy GC: [RECONCILE3](reconcile-implementation-plan.md).
