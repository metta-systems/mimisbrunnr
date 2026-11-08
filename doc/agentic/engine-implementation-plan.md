# Engine — implementation plan

Design: [engine-design.md](engine-design.md) · Decisions: [engine-decisions.md](engine-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Live trees

- [ ] **ENGINE1 — Long-lived `LoadedNode` per tree.** Replace the per-commit rebuild of every tree with one cached `LoadedNode` per tree; mutations route through `insert` / `remove`, lookups through the cached merged view, and `save_index_state` appends a run for each dirty tree only. Prerequisites: [BTREE5](btree-implementation-plan.md), [JOURNAL2](journal-implementation-plan.md). Acceptance: modifying one entry in a populated tree grows its region by one run; 100 lookups without a flush rebuild no merged view.
- [ ] **ENGINE2 — Compaction hook in commit.** After appending, a tree that meets `should_compact` is compacted into a fresh region and the root slot (or parent inner node) is updated. Prerequisites: ENGINE1, [BTREE7](btree-implementation-plan.md), ENGINE3.

## Copy-on-write commit

- [ ] **ENGINE3 — COW region placement.** Each flushed tree gets a freshly allocated region; the previous root's regions are freed after `commit_root`. Prerequisite: [ALLOCATION1](allocation-implementation-plan.md). Acceptance: two consecutive commits produce different `BlockRef`s; a crash after `save_index_state` but before `commit_root` reopens on the previous root intact; `bin/analyze` shows every named tree at a real bucket.
- [ ] **ENGINE4 — Retire zones and fixed areas.** The index, metadata and blob zones, `ZoneExtent`, `ZoneMap`, the fixed forward-overflow area (8 MiB offset, 32 regions), tag-bitmap area (4 MiB, 1 024 slots) and the fixed-offset region slots give way to allocator placement in buckets (ENGINE-DESIGN§1), lifting their caps and the 32 MiB `FMT_INDEX_ZONE_SIZE` assumption. Prerequisites: ENGINE3, [INDEX13](index-implementation-plan.md), [INDEX14](index-implementation-plan.md).
- [ ] **ENGINE5 — Wire `BackpointerTable` into the engine.** Engine field, flush and load through `RootPointer`, mutations maintained alongside location changes. Prerequisite: [BLOB1](blob-implementation-plan.md) (first producer of backpointers).

## Retire the legacy blob

- [ ] **ENGINE6 — Oplog out of the legacy blob.** The in-memory `OpLog` and its CBOR copy in the legacy blob go; mutation history, where a pool keeps any, comes from the retained journal (JOURNAL-DESIGN§1.6; none by default), and subscriptions use their pending events. Prerequisite: [JOURNAL13](journal-implementation-plan.md).
- [ ] **ENGINE7 — Counters from the checkpoint.** `next_oid_local` from the pool's own `ObjectRoots` entry (METADATA-DESIGN§1.4; until [METADATA1](metadata-implementation-plan.md), from the single object-table leaf); `last_applied_lsn`, `next_term_id`, `next_tag_id` and `next_rule_id` from the latest checkpoint's `RootListHead` (ENGINE-DESIGN§1.2), advanced by replay; `next_tag_id` used through [ONTOLOGY1](ontology-implementation-plan.md). Acceptance: a tag created on a branch that a rollback abandons, followed by deleting the branch's snapshots and reopening, does not have its `TagId` reused; a crash after allocating ids and before the next checkpoint replays to the same counters.
- [ ] **ENGINE8 — Delete the legacy blob.** Remove `IndexBlob`, `INDEX_BLOB_MAGIC`, `LEGACY_CBOR_BLOB_OFFSET`, `read_index_blob`, `apply_index_blob`, `build_index_blob`. Prerequisites: ENGINE6, ENGINE7, [UNIX1](unix-implementation-plan.md), [BLOB6](blob-implementation-plan.md). Acceptance: format, insert tags, objects and `unix-path` values, commit, reopen: all state survives with no `MIXI` blob on disk.

## Superblock and root list

- [ ] **ENGINE12 — Superblock and root list.** The ENGINE-DESIGN§1.1 superblock (disk UUID, geometry, `metadata_replicas`, `CheckpointHint`, no zones and no root slots) and the root list (`RootListHead`, `RootEntry` keyed by tree kind) carried by `Checkpoint` entries; mount reads the jset the hint names, then scans for newer checkpoints. Prerequisites: [ALLOCATION9](allocation-implementation-plan.md), [JOURNAL12](journal-implementation-plan.md). Acceptance: superblock layout asserted in code and `validation/`; a pool with every tree populated round-trips its root list; a clean mount reads exactly one jset before replay.
- [ ] **ENGINE11 — Typed chunking defaults in pool state.** The format-time default chunking (`ChunkParamsRecord`, INDEX-DESIGN§4.3) and threshold live in the pool state record (POOL-DESIGN§1) instead of 16 opaque superblock bytes; the struct lives in `crates/storage`, below both pool state and the chunk list; `brunnr create` writes the default and the write path reads it. Prerequisite: [POOL3](pool-implementation-plan.md). Acceptance: a pool created with explicit chunking defaults reports them after reopen.

## Embedding

- [ ] **ENGINE13 — Step-driven engine.** Background work — commits, checkpoints ([JOURNAL4](journal-implementation-plan.md)), node flushes and reconcile ([RECONCILE2](reconcile-implementation-plan.md)) — runs in `step` calls bounded by a caller-supplied budget, with a non-blocking query of what is due; the engine creates no thread, and a `std`-gated helper drives it on the host. Prerequisites: JOURNAL4, ENGINE1. Acceptance: a test drives the engine with `step` alone through writes, a checkpoint and a copy-GC pass; no thread is spawned with the helper disabled.

## Device target

- [ ] **ENGINE10 — `no_std` engine.** The engine and the crates below it build for `aarch64-unknown-none-softfloat` with default features off: `#![no_std]` + `alloc`, `hashbrown` or `BTreeMap` for `HashMap`, dependencies with `default-features = false`, `FileBlockDevice`, the module TOML loader and `petgraph` behind `std`. The pool opens from the device set the caller supplies ([POOL7](pool-implementation-plan.md)). A `just` recipe builds the `no_std` set. Prerequisites: [JOURNAL7](journal-implementation-plan.md), POOL7, ENGINE8. Acceptance: a host test builds a partitioned SD-card image, writes `doom1.wad` into the pool partition with the `std` tools, opens it through the `no_std` API over a 512-byte-sector `BlockDevice` and reads the exact bytes by query.