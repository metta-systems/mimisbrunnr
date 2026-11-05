# Engine — implementation plan

Design: [engine-design.md](engine-design.md) · Decisions: [engine-decisions.md](engine-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Live trees

- [ ] **ENGINE1 — Long-lived `LoadedNode` per tree.** Replace the per-commit rebuild of every tree with one cached `LoadedNode` per tree; mutations route through `insert` / `remove`, lookups through the cached merged view, and `save_index_state` appends a run for each dirty tree only. Prerequisites: [BTREE5](btree-implementation-plan.md), [JOURNAL2](journal-implementation-plan.md). Acceptance: modifying one entry in a populated tree grows its region by one run; 100 lookups without a flush rebuild no merged view.
- [ ] **ENGINE2 — Compaction hook in commit.** After appending, a tree that meets `should_compact` is compacted into a fresh region and the root slot (or parent inner node) is updated. Prerequisites: ENGINE1, [BTREE7](btree-implementation-plan.md), ENGINE3.

## Copy-on-write commit

- [ ] **ENGINE3 — COW region placement.** Each flushed tree gets a freshly allocated region; the previous root's regions are freed after `commit_root`. Prerequisite: [ALLOCATION1](allocation-implementation-plan.md). Acceptance: two consecutive commits produce different `BlockRef`s; a crash after `save_index_state` but before `commit_root` reopens on the previous root intact; `bin/analyze` shows every named tree at a real bucket.
- [ ] **ENGINE4 — Retire fixed index-zone areas.** The fixed forward-overflow area (8 MiB offset, 32 regions), tag-bitmap area (4 MiB, 1 024 slots) and the fixed-offset region slots give way to allocator placement, lifting their caps and the 32 MiB `FMT_INDEX_ZONE_SIZE` assumption. Prerequisites: ENGINE3, [INDEX4](index-implementation-plan.md).
- [ ] **ENGINE5 — Wire `BackpointerTable` into the engine.** Engine field, flush and load through `RootPointer`, mutations maintained alongside location changes. Prerequisite: [BLOB1](blob-implementation-plan.md) (first producer of backpointers).

## Retire the legacy blob

- [ ] **ENGINE6 — Oplog out of the legacy blob.** Prerequisite: ENGINE-D1.
- [ ] **ENGINE7 — Derive counters at boot.** `next_oid_local` from the object table, `last_applied_lsn` from the latest checkpoint; `next_tag_id` via [ONTOLOGY1](ontology-implementation-plan.md).
- [ ] **ENGINE8 — Delete the legacy blob.** Remove `IndexBlob`, `INDEX_BLOB_MAGIC`, `LEGACY_CBOR_BLOB_OFFSET`, `read_index_blob`, `apply_index_blob`, `build_index_blob`. Prerequisites: ENGINE6, ENGINE7, [UNIX1](unix-implementation-plan.md), [BLOB6](blob-implementation-plan.md). Acceptance: format, insert tags, objects and a path context, commit, reopen: all state survives with no `MIXI` blob on disk.

## Device target

- [ ] **ENGINE10 — `no_std` engine.** The engine and the crates below it build for `aarch64-unknown-none-softfloat` with default features off: `#![no_std]` + `alloc`, `hashbrown` or `BTreeMap` for `HashMap`, dependencies with `default-features = false`, `FileBlockDevice`, `PoolConfig` TOML, the module TOML loader and `petgraph` behind `std`. A single-disk pool opens from a `BlockDevice` and its superblock without `pool.toml`, through an offset view when the pool lives in a partition. A `just` recipe builds the `no_std` set. Prerequisites: OPERATIONS-D4, [BLOB2](blob-implementation-plan.md). Acceptance: a host test builds a partitioned SD-card image, writes `doom1.wad` into the pool partition with the `std` tools, opens it through the `no_std` API over a 512-byte-sector `BlockDevice` and reads the exact bytes by query.

## Zone growth

- [ ] **ENGINE9 — `ZoneMap` continuation chain.** When a zone exceeds the extents one `ZoneMap` block holds, chain a follow-on block through `BLOCK_FLAG_CONTINUATION` and walk the chain at mount (ENGINE-DESIGN§1.1). Acceptance: a zone grown past one block's extent capacity resolves every extent after reopen.
