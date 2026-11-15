# Allocation — implementation plan

Design: [allocation-design.md](allocation-design.md) · Decisions: [allocation-decisions.md](allocation-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Pointers and entries

- [ ] **ALLOCATION9 — Bucket-relative pointers and node pointers.** `BlockRef` and `BlobRef` take the ENGINE-DESIGN§1.2 layout (`disk_id`, `sector`, `bucket_no`, `u32` generation); `NodePtr` holds `Superblock.metadata_replicas` `BlockRef`s; every reader and writer, `validation/` and the journal payloads move with them. Acceptance: layouts asserted in code and `validation/`; a `BlockRef` into bucket 2³² − 1 of a 1 MiB-bucket disk round-trips; a two-disk pool with R = 2 writes each node to both disks and reads it from the second after the first copy is corrupted.
- [ ] **ALLOCATION10 — Derived live sectors.** `live_sectors` maintained through `BucketAllocTable::add_live_sectors` / `remove_live_sectors` from the data entries and checkpoint node accounting, and replayed from them (ALLOCATION-DESIGN§1.2). Prerequisites: [ENGINE3](engine-implementation-plan.md) (trees in allocated buckets), [JOURNAL8](journal-implementation-plan.md) (the data ops), [METADATA3](metadata-implementation-plan.md) (backpointers to recount from). Acceptance: write, delete and move a set of extents, crash before checkpoint, reopen: every bucket's `live_sectors` equals a recount from backpointers.

## Region allocation

- [ ] **ALLOCATION2 — Seed the freelist at format time.** Format seeds every bucket from `first_usable_bucket` to the last bucket before the trailing superblock as `Free` with generation 1 through `seed_freelist`. Acceptance: a freshly created pool's alloc table lists exactly those buckets as free.
- [ ] **ALLOCATION3 — Resident generations and the dereference check.** Per-disk generation arrays loaded at mount and maintained by replay; dereferencing a committed pointer with a mismatched generation fails with `StaleBlockRef`, a backpointer with one is skipped (ALLOCATION-DESIGN§1.3). Prerequisite: [ENGINE3](engine-implementation-plan.md). Acceptance: a tampered child-pointer generation fails the read; a stale backpointer is skipped by a bucket scan.

## Per-disk trees

- [ ] **ALLOCATION4 — Per-disk bucket-alloc table.** Rooted in the disk descriptor, key `bucket_no: u32`, packed-key codec; engine keeps one table per disk; `BucketAllocator::for_disk(disk_id)`. Prerequisites: ALLOCATION1, ALLOCATION10. Acceptance: in a two-disk pool each disk's tree holds only its own buckets.
- [ ] **ALLOCATION5 — Per-disk freespace LRU.** Rooted in the disk descriptor, key `(fragmentation_band: u8, bucket_no: u32)`, closed buckets only; allocator fast path takes band 0 (ALLOCATION-DESIGN§1.4). Prerequisite: ALLOCATION4.
- [ ] **ALLOCATION6 — Drop CBOR-wrapped POD values in allocation trees.** `BucketAllocEntry` and freespace keys lose their manual byte-image `Serialize` impls once ALLOCATION4 / ALLOCATION5 use the packed codec; `BucketAllocKey` and `FreespaceLruKey` proxies go away.

## Completeness

- [ ] **ALLOCATION7 — Per-disk write points.** Writes are segregated by bucket kind and stream tag (ALLOCATION-DESIGN§1.5).
- [ ] **ALLOCATION8 — Discard.** A discard pass issues TRIM for free buckets with `BUCKET_FLAG_NEEDS_DISCARD` (ALLOCATION-DESIGN§1.7).

Copy GC: [RECONCILE3](reconcile-implementation-plan.md).
