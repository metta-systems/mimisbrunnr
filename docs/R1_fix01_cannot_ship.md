# R1 fix plan — Tier 1: Cannot Ship

Items here block shipping a real persistent pool: wrong on-disk shape such that
spec recovery / sync / migration paths cannot be built on top.

Audit refs: see chat audit dated 2026-05-06 (project memory
`project_r1b_progress.md`); IMPLEMENTATION.md sections noted inline.

## Dependency order

Execute strictly top-down. Each step lands a self-contained piece that the
next builds on.

```
D2 (bucket allocator wiring)
    └── D1 (RootPointer.*_root population)
            └── A2 (ObjectTable / LocationTable positional radix)
            └── A1 (KvIndex extendible hash)
            └── A4 (OntologyRoot 4-subtree split)
```

---

## D2. Bucket allocator wiring for B+ tree regions  (foundation)

**Spec:** IMPL §12.1 (buckets), §12.2 (`BucketAllocEntry`), §12.3 (generation-checked
pointers), §1.5.4 step 1 ("allocate a fresh region in a new bucket via the standard
write-point mechanism §12.5"), §5 "Tree growth".

**Current state:** `crates/engine/src/disk_engine.rs:48-95` writes regions at
compile-time fixed offsets in the index zone (`CHUNK_INDEX_REGION_OFFSET = 0`,
…). `BucketAllocTable` is never consulted; `BucketAllocEntry.data_type` is
never set; `dirty_sectors` is never incremented; copygc cannot find these
buckets.

### Steps

1. **Live `BucketAllocator` API in `mimisbrunnr-storage`.**
   - File: new `crates/storage/src/allocator.rs`.
   - Public type:
     ```rust
     pub struct BucketAllocator<'a> {
         table: &'a mut BucketAllocTable,
         bucket_size_log2: u8,
         first_usable_bucket: u32,
         total_buckets: u32,
         disk_id: u16,
     }
     impl BucketAllocator<'_> {
         pub fn alloc(&mut self, kind: BucketDataType) -> Result<BlockRef, StorageError>;
         pub fn free(&mut self, block_ref: BlockRef) -> Result<(), StorageError>;
         pub fn block_no_to_bucket(&self, block_no: u32) -> (u32 /* bucket_no */, u32 /* in_bucket */);
         pub fn bucket_to_block_no(&self, bucket_no: u32) -> u32;
     }
     ```
   - `alloc` walks `BucketAllocTable` for a free bucket
     (`data_type == Free`), bumps its `generation`, stamps `data_type =
     BtreeNode` (or as supplied), returns `BlockRef { disk_id, block_no =
     bucket_no * sectors_per_bucket, generation }`.
   - For R1c-fix, scan-and-claim is acceptable; the freespace LRU
     (`crates/storage/src/freespace.rs`) becomes the fast path under D3.

2. **Per-disk `BucketAllocTable` storage.**
   - Spec wants the table per-disk (E1). For Cannot-Ship we accept the
     pool-scoped tree in `crates/storage/src/alloc.rs` for now (already keyed
     by `(disk_id, bucket_no)`); the per-disk split is Tier 3 (E1).
   - Engine needs to load the table at boot. Reserve a fixed slot in the
     index zone for it for now (offset just past `LEGACY_CBOR_BLOB_OFFSET`),
     to be retired when D1 lands and the table moves under
     `DiskDescriptorOnDisk.buckets_root`.

3. **Engine plumbing in `DiskEngine`.**
   - File: `crates/engine/src/disk_engine.rs`.
   - Drop the per-region fixed offsets (`CHUNK_INDEX_REGION_OFFSET`, …).
   - Replace with `RootPointer.*_root: BlockRef` lookups (D1 step). For
     this step (D2) just replace the *write side*: every
     `engine.X.flush_to_region(device, fixed_offset)` becomes:
     ```rust
     let block_ref = self.allocator.alloc(BucketDataType::BtreeNode)?;
     let offset = block_ref_to_offset(&block_ref, self.bucket_size_log2);
     engine.X.flush_to_region(device, offset)?;
     // free old block_ref via self.allocator.free(prev_root_for_X)
     self.new_root_pointer.X_root = block_ref;
     ```
   - Existing `prev_root_for_X` comes from the active root pointer's
     existing slot (will be `BlockRef::ZERO` on the first commit; allocator
     skips `ZERO`).

4. **Freelist seeding at format time.**
   - File: `crates/pool/src/tier.rs` `PoolManager::create_pool`.
   - After superblock format: walk every bucket past `bootstrap_buckets`
     and insert a `BucketAllocEntry { data_type: Free, generation: 1 }` for
     each. This populates the in-memory `BucketAllocTable`. (Spec: §12.1
     "`bootstrap_buckets` (typically 32–64) pins the leading buckets used
     for the superblock, WAL header, and the root pages of the alloc table
     itself; these are excluded from the freespace LRU".)

5. **`BlockRef.generation` validation at read time.**
   - File: `crates/storage/src/btree.rs` `BtreeRegion::read`.
   - New parameter `expected_generation: Option<u64>` (None for
     pre-D2 callers). If `Some(g)`, look up the `BucketAllocEntry` for
     the bucket and reject if `entry.generation != g`. The dereference
     protocol from §12.3.

### Tests
- `crates/storage/tests/allocator.rs`: format → alloc N buckets → verify
  `data_type` flips to `BtreeNode`, `generation` is non-zero, returned
  `BlockRef`s are unique. Free → generation bumps. Re-alloc may return
  the same bucket but with new generation.
- Engine test: `commit()` → reload → tree contents preserved through
  generation-checked reads. Stale-`BlockRef` test: tamper with the
  in-memory generation, expect rejection.

### Verification gate
- `cargo build -p mimisbrunnr-storage -p mimisbrunnr-engine` clean.
- All existing engine integration tests pass.
- One new test that proves a tree's `BlockRef` survives a commit/reopen
  cycle and the bucket alloc entry reflects it.

### Risks
- **Format-time backwards-compat break.** Old pools (with no
  `BucketAllocTable` populated) won't load under D2. Same one-way
  migration policy as R1b: recreate the pool.
- **Allocator scan cost** for the linear walk; acceptable for ≤ 1 M
  buckets (typical dev pool); production large pools need the freespace
  LRU fast path which is Tier 3 (E2).

---

## D1. Populate `RootPointer.*_root` BlockRef fields  (depends on D2)

**Spec:** IMPL §2.2. RootPointer carries 22 BlockRef slots (lines 580–609).
Atomic commit step 5 flips the active root.

**Current state:** `crates/engine/src/disk_engine.rs:535-577`. `commit()`
copies the previous RootPointer with only `seq` and `lsn` bumped. Every
`*_root` slot stays `BlockRef::ZERO`.

### Steps

1. **`RootPointer` builder helper.**
   - File: `crates/engine/src/disk_engine.rs`.
   - Method `DiskEngine::new_root_pointer(&mut self) -> Result<RootPointer,
     EngineError>` — builds the *target* root for the next checkpoint.
     Calls `allocator.alloc(...)` for each tree the engine knows about
     (chunk, kv, forward, tag, range, object, location, ontology,
     subscriptions, backpointer, bucket_alloc, freespace_lru,
     placement_rules, disks_overflow). Stamps each into the matching
     `RootPointer` field. Recomputes CRC.

2. **Per-tree flush keyed by RootPointer slot.**
   - For each tree that previously used a fixed offset:
     - alloc fresh `BlockRef`,
     - `flush_to_region(device, block_ref_to_offset(...))`,
     - free the previous `RootPointer.X_root` (if non-zero),
     - store fresh `BlockRef` into the new RootPointer.
   - The 22-slot list is hard-coded today; later phases may iterate. For
     this step, hardcode the engine-visible subset (about 10 of 22; the
     reconcile / snapshot / value_spill slots stay `BlockRef::ZERO` until
     R6/R7).

3. **Per-tree load from RootPointer slot.**
   - In `load_index_state`, replace fixed offsets with
     `block_ref_to_offset(self.superblock.active_root_pointer().X_root, ...)`.
   - Skip load if `X_root == BlockRef::ZERO` (fresh pool).

4. **Free old root pointers after commit succeeds.**
   - After `commit_root` returns, walk the previous RootPointer and call
     `allocator.free` for each non-zero slot. Bump `dirty_sectors = 0` on
     each freed bucket. (No copy GC yet — buckets become candidates the
     next time someone wants a fresh allocation.)

5. **Drop `LEGACY_CBOR_BLOB_OFFSET` indexing.**
   - The legacy MIXI blob stays for now (path_contexts + oplog +
     scalars), but its offset becomes a `RootPointer` slot too — add a
     transitional `legacy_blob_root: BlockRef` field… **no — easier**:
     keep the blob at a `BlockRef` allocated from the allocator just
     like the trees, store it in a temporary engine-private slot in
     `RootPointer.flags` reserved bits or a side-band table. Tier 4
     retires the blob entirely (D4); the bridge keeps it on a
     dynamically-allocated bucket so the index zone stops being a
     fixed-layout zone.

### Tests
- `mimir create / commit / mimir status / mimir get` cycle still works.
- Test that `RootPointer.chunk_index_root != BlockRef::ZERO` after first
  `commit()` with at least one chunk inserted.
- Test that two consecutive commits produce **different** `BlockRef`s
  (each commit allocates a fresh bucket; the previous one is freed).
- Test that crash mid-commit (simulated by dropping the engine after
  `save_index_state` but before `commit_root`) recovers the **previous**
  RootPointer's blocks intact (the new ones haven't been flipped to
  active).

### Verification gate
- `cargo test -p mimisbrunnr-engine` all green.
- Run `bin/analyze` against a freshly committed pool; every named tree
  must point to a real bucket, not ZERO.

---

## A2. ObjectTable / LocationTable positional radix tree  (depends on D1, D2)

**Status (2026-05-06):**
- **Leaf-only path: DONE.** `crates/meta/src/{object_leaf,location_leaf}.rs`
  ship the spec's byte-exact 256 KiB leaf layouts. ObjectTable /
  LocationTable's `flush_to_region` / `load_from_region` use them.
  Limit: `oid_local ≥ 2044` (or 5440 for locations) returns
  `MetaError::OidOutOfRange`.
- **Multi-level (inner nodes, descent, tree growth, COW) — DEFERRED:**
  blocked on **Tier 3 D3** (COW reallocation). Without D3 the tree
  can't move leaves to fresh buckets on flush; growing the tree's
  root level needs allocator-driven inner-node placement that D3
  unlocks. Tracked as todo "A2 multi-level" in
  `memory/project_r1c_progress.md`. Resume order: D3 → A2
  multi-level.


**Spec:** IMPL §5 (ObjectTable), §6.1 (LocationTable). COW radix tree of large
nodes. Leaf: 2044 × `ObjectRecord` (128 B) + 256 B occupancy bitmap + 32 B
trailer. Inner: 16 380 × `BlockRef`. Address translation:
`slot = oid_local % 2044; child[level] = (oid_local / 2044) % 16 380`.

**Current state:** `crates/meta/src/{object_table,location_table}.rs` use
sorted-run B+ trees keyed by `(oid, snapshot)`. The address-translation
helpers in `crates/meta/src/radix.rs` exist (`oid_to_radix_path`,
`MAX_LEVELS`, `LEAF_RECORDS`) but are unused.

### Steps

1. **Native leaf node format.**
   - New file: `crates/meta/src/object_leaf.rs`.
   - Struct `ObjectTableLeaf` representing the 256 KiB layout:
     `BtreeNodeHeader (64 B) | occupancy_bitmap (256 B) | trailer (32 B) |
     [ObjectRecord; 2044]`. Round to 261 984 B used + 160 B tail pad.
   - Method `slot_at(slot: u16) -> Option<&ObjectRecord>` returns `None`
     if occupancy bit is clear.
   - Method `set_slot(slot: u16, record: ObjectRecord)`. Sets occupancy
     bit, writes record at byte offset
     `352 + slot * OBJECT_RECORD_SIZE`.

2. **Native inner node format.**
   - Struct `ObjectTableInner` for the 256 KiB layout:
     `BtreeNodeHeader (64 B) | [BlockRef; 16 380]` = exactly 256 KiB.
   - Empty child = `BlockRef { generation: 0, .. }` (sentinel that never
     matches a live bucket).

3. **Address translation entry points.**
   - Use existing `radix.rs` helpers verbatim:
     `oid_to_radix_path(oid_local, root_level)` returns
     `RadixPath { leaf_slot, child_path: [u16; 3] }`.

4. **Tree descent + flush.**
   - `ObjectTable` becomes a *cache* of recently-loaded leaves, keyed by
     `BlockRef`. Lookup: descend from `RootPointer.object_table_root`,
     deref each `BlockRef`, return the slot or `None`. Insert: descend
     to leaf, mark dirty, queue for flush.
   - Flush at commit time: walk dirty leaves, allocate fresh
     buckets via `BucketAllocator::alloc(BucketDataType::Metadata)`,
     write the leaves, propagate up the inner nodes (each level rewrites
     because the child `BlockRef` changed — COW).
   - Tree growth (§5 "Tree growth"): when `oid_to_radix_path` overflows
     the current `root_level`, allocate a new inner one level higher,
     point its first child at the previous root, increase
     `RootPointer.object_table_root` to the new top.

5. **Engine integration.**
   - File: `crates/engine/src/disk_engine.rs`.
   - Replace
     `engine.object_table.flush_to_region(...)` and
     `ObjectTable::load_from_region(...)`
     with the new descent-driven path. The current sorted-run helpers
     stay available under a `cfg(test)` flag for the per-table
     round-trip tests, but production goes through the radix path.
   - Same for LocationTable (`crates/meta/src/location_leaf.rs`,
     leaf fanout 5 440 for 48 B records).

### Tests
- `crates/meta/tests/radix_persistence.rs`:
  - Insert 5 000 records spread across the oid space; verify every one
    round-trips through write → flush → reload.
  - Verify the on-disk leaf is a valid `BtreeNodeHeader || bitmap ||
    trailer || [ObjectRecord; 2044]` exactly per spec offsets.
  - Verify tree growth: fill leaf 0 to capacity, then write to a far oid
    that triggers root_level+1.
  - Sparse-leaf occupancy: write to oid 5 and oid 2043 in the same leaf,
    reload, verify the bitmap reflects exactly bits 5 and 2043.

### Verification gate
- `cargo test -p mimisbrunnr-meta` all green.
- New test that proves a 5 000-object pool's `object_table_root` resolves
  via radix descent (not sorted-run binary search).
- Old sorted-run round-trip tests deleted; new tests cover the radix path.

### Migration note
- One-way on-disk format break. Recreate the pool to load under A2.

---

## A1. KvIndex extendible hash  (depends on D1, D2)

**Spec:** IMPL §9.1. `KvDirectory` (4 KiB block, kind=`KvHashDirectory`,
inline 252 `BlockRef` slots, `global_depth ≤ 7`). Spillover at
`global_depth ≥ 8` to `BtreeKind::KvDirectory` positional region. Buckets
are `KvBucket` 4 KiB blocks (kind=`KvHashBucket`, 144 entries each).

**Current state:** `crates/index/src/kv_index.rs` writes a §1.5 B+ tree
region with sorted-run entries. The `KvHashDirectoryHeader` and
`KvHashBucketHeader` structs exist but are unused on the wire.

### Steps

1. **`KvDirectory` 4 KiB block writer / reader.**
   - File: `crates/index/src/kv_directory.rs`.
   - Layout: 32 B `BlockHeader` + 1 B `global_depth` + 3 B `_pad0` + 4 B
     `bucket_count` + `[BlockRef; 252]` + 16 B `spillover_root` + 4 B
     `_pad_tail` + 4 B trailing CRC32C.
   - Serializer / parser per spec offsets. Use `BlockKind::KvHashDirectory`.

2. **`KvBucket` 4 KiB block writer / reader.**
   - File: `crates/index/src/kv_bucket.rs`.
   - Layout: 32 B `BlockHeader` + 1 B `local_depth` + 2 B `entry_count` +
     1 B `_pad` + `[{ tag_id: u32, value_hash: u64, bitmap_ref: BlockRef }; 144]`
     + 24 B `_pad_tail` + 4 B trailing CRC32C.
   - Use `BlockKind::KvHashBucket`. Each entry's `bitmap_ref` points to a
     §8.2 `TagBitmap` region (also unimplemented; for A1 phase, use a
     `BlockRef::ZERO` and store the roaring bitmap inline as a transient
     side-table — Tier 2 A3.6 will land the real `TagBitmap`).

3. **In-memory mirror with extendible-hash semantics.**
   - `KvIndex` keeps `directory: Vec<BlockRef>` of `1 << global_depth`
     entries + per-bucket `LoadedKvBucket` cache.
   - Lookup: `hash(tag_id, value_hash) → directory_idx → bucket → linear scan`.
   - Split: when a bucket overflows, increment `local_depth`; if it
     exceeds `global_depth`, double the directory (allocate new
     `KvDirectory` block; if `global_depth ≥ 8`, allocate a spillover
     region instead).

4. **Engine integration.**
   - `DiskEngine::save_index_state` / `load_index_state` swap to the new
     KvIndex API. `RootPointer.kv_index_root` points at the
     `KvDirectory` block (4 KiB), not a 256 KiB region.
   - Drop the previous sorted-run `KvIndex::flush_to_region` /
     `load_from_region` (the test suite in `kv_index.rs` will be deleted
     and rewritten against the new API).

### Tests
- `crates/index/tests/kv_extendible.rs`:
  - 1 000 entries → verify directory grows monotonically.
  - Single-bucket → split → re-insert path.
  - `global_depth ≥ 8` → spillover region allocated.
  - Reload after commit → every key's bitmap survives.

### Verification gate
- New tests pass; old kv_index sorted-run tests deleted.
- `mimir attr / mimir get` round-trip via the new path.

---

## A4. OntologyRoot 4-subtree split  (depends on D1, D2)

**Spec:** IMPL §10.1.

```
OntologyRoot (4 KiB):
  header (BlockHeader, kind = OntologyRoot)
  module_count: u32
  tag_count: u32
  implication_count: u32
  modules_root: BlockRef     → §1.5 B+ tree, key = (module_id_hash, snapshot)
  tags_root:    BlockRef     → §1.5 B+ tree, key = (TagId, snapshot)
                                              → TagDefRecord (64 B fixed)
  tag_names:    BlockRef     → §1.5 B+ tree, key = (name_hash, snapshot)
                                              → (TagId, BlockRef → CBOR(TagDef))
  dag_root:     BlockRef     → ImplicationDagPages (sparse adjacency lists)
```

**Current state:** `crates/ontology/src/state.rs` writes a single sorted-run
entry with the entire `PersistedState` CBOR'd into one value.

### Steps

1. **Add `BlockKind::OntologyRoot`.**
   - File: `crates/storage/src/block.rs`. Reserve a discriminant — pick
     the next free one *after* the existing `OverflowRecord = 10`. Spec
     doesn't pin a number for this kind explicitly, but per IMPL §1.3
     line 116 OntologyRoot doesn't have a slot — the spec uses
     `BtreeKind::Ontology` for the four sub-trees and wraps them in a
     4 KiB block of an existing kind. *Ambiguity*: spec is unclear on
     whether `OntologyRoot` is a `BlockKind` or a `BtreeKind`. Resolve
     by reading the 4 KiB envelope text; the closest existing analogue
     is `PoolStateRoot = BlockKind`, suggesting OntologyRoot is also a
     `BlockKind`. Add as `BlockKind::OntologyRoot = 11`.

2. **`OntologyRoot` 4 KiB block writer / reader.**
   - File: `crates/ontology/src/root_block.rs`.
   - Layout above, total 4096 B with trailing CRC32C.

3. **`TagDefRecord` 64 B fixed-size struct.**
   - Follows spec line 2066: `name_offset` points into the `tag_names`
     tree. The 64 B layout isn't specified bit-for-bit in IMPL §10.1;
     derive from §3.1 `TagDefinition` and the spec's "fixed records" hint:
     ```
     tag_id: u32                                  // [0..4]
     semantics: u8                                // [4..5]
     value_type: u8                               // [5..6]   (for Attr semantics)
     storage_axes_present: u8                     // [6..7]   bitfield
     _pad0: u8                                    // [7..8]
     storage: { chunking_id u32 |                 // [8..16]
                compression_id u32 }
     name_offset: u64                             // [16..24] BlockRef into tag_names
     created_ns: i64                              // [24..32]
     last_modify_lsn: u64                         // [32..40]
     _reserved: [u8; 24]                          // [40..64]
     ```
     (This is **derived**, not in spec — propose to the user; if rejected,
     defer to a spec update.)

4. **Four sub-trees as separate B+ trees.**
   - `modules_root`: keyed by `(module_id_hash u64, snapshot u32)` →
     CBOR(ModuleManifest). Sorted-run B+ tree (variable-shape value, so
     CBOR for now until Tier 2 R1d-pack-2).
   - `tags_root`: keyed by `(TagId u32, snapshot u32)` → 64 B
     `TagDefRecord` byte image. Eligible for packed-key codec once R1c
     fix lands.
   - `tag_names`: keyed by `(name_hash u64, snapshot u32)` →
     `(TagId u32, BlockRef)` (16 B value).
   - `dag_root`: `ImplicationDagPages` — sparse adjacency lists. A
     positional region (no sorted runs); each page lists outgoing edges
     for a `TagId` range. For Cannot-Ship, accept a single 256 KiB
     region holding the entire DAG snapshot (today's CBOR shape, but in
     a `BtreeKind::Ontology`-flagged region).

5. **Engine integration.**
   - `OntologyState::flush_to_region(device, root_block_offset)` writes
     the 4 KiB `OntologyRoot` block + four child regions; populates
     `RootPointer.ontology_root: BlockRef` to point at the 4 KiB block.
   - `OntologyState::load_from_region` parses the root block, then
     descends each subtree.

### Tests
- `crates/ontology/tests/multi_subtree.rs`:
  - Install module → flush → reload → verify each subtree round-trips.
  - Modify only the dag → flush → verify the other three subtrees'
    `BlockRef`s are unchanged (COW property: only mutated subtrees
    rewrite).

### Verification gate
- `cargo test -p mimisbrunnr-ontology` all green.
- `RootPointer.ontology_root` points at a real 4 KiB block (not ZERO).

### Open questions for the user
- Is `BlockKind::OntologyRoot` an acceptable spec extension, or should
  the 4 KiB envelope re-use `BlockKind::PoolStateRoot` semantics?
- Is the 64 B `TagDefRecord` layout above acceptable, or should we wait
  for a spec amendment naming the exact bit positions?
