# R1 fix plan — Tier 3: Latent / Unwired

Items here are present in code but the calling path isn't connected; tests
exist but production never invokes them. The §1.5 *operational* model
(append-on-flush, lazy compaction, journal pinning) is implemented in
`crates/storage/src/btree.rs` but no R1b consumer touches it.

Prerequisite: Tier 1 ships first. These items lift performance from "every
commit rewrites every region" to spec's "tens of KB per flush".

## Status (2026-05-11)

| Item | Status | Notes |
|------|--------|-------|
| **B1** Wire `append_sorted_run` into production flush | ❌ Not started | Every commit still goes through `write_full`. |
| **B2** Wire `should_compact` / `compact` into commit | ❌ Not started | Storage-side machinery exists; engine never invokes it. |
| **B3** Long-lived `LoadedNode` with cached `merged_view` | ❌ Not started | Folded into B1. |
| **B4** Populate `BtreeNodeHeader.min_key` / `max_key` | ❌ Not started | Required before multi-level descent (A2 multi-level). |
| **B5** `LoadedNode.pin: JournalPin` | ❌ Not started | WAL trim could orphan pending journal entries today. |
| **D3** Bucket alignment for B+ tree regions | ❌ Not started | **Unblocks A2 multi-level and lifts the A1/A3.2 region caps.** |
| **D5** Checkpoint cadence driver thread | ❌ Not started | Commits run synchronously on the mutation path. |
| **E1, E2** Per-disk roots for bucket alloc / freespace LRU | ❌ Not started | Currently pool-scoped with `disk_id` in the key. |

---

## B1. Wire `append_sorted_run` into the production flush path

**Spec:** IMPL §1.5.2. New mutations append a fresh sorted run; only the
header sector rewrites. Spec property: "typically a few hundred KB, not
the whole 256 KiB".

**Current state:** `crates/storage/src/btree.rs:690-747` is correct;
referenced only by storage tests. Every R1b consumer goes through
`BtreeRegion::write_full` (full rewrite).

### Steps

1. **Long-lived `LoadedNode` cache in the engine.**
   - File: `crates/engine/src/engine.rs` — replace each direct field
     (`forward_index: ForwardIndex`, `tag_index: TagIndex`, …) with a
     `LoadedNode<K, V>` per tree, keyed by tree kind.
   - Engine API for mutation routes through `LoadedNode::insert(lsn, k, v)`
     / `LoadedNode::remove(lsn, k)`.
   - Lookup goes through `LoadedNode::lookup` (which already merges
     pending journal + sorted runs + cached merged_view).

2. **Flush via `append_sorted_run`.**
   - `DiskEngine::save_index_state` becomes:
     ```rust
     for tree in dirty_trees {
         let new_run = tree.flush_to_run()?;            // §1.5.3 fold pending → run
         BtreeRegion::append_sorted_run(device, tree.block_ref.offset(),
                                        &mut tree.header, &new_run)?;
     }
     ```
   - Only modified header sectors + freshly written run sectors hit disk.

3. **`flush_to_run` gains tombstone support.**
   - File: `crates/storage/src/btree.rs:344-388`. Currently rejects
     tombstones in `pending_journal`. Add packed-key tombstone encoding
     (a per-entry "whiteout" bit flag in the sorted-run header) so
     `append_sorted_run` can ship deletes without forcing a full
     compaction. Spec hint §1.5.3 names them "whiteouts".

### Verification gate
- New test: 1000-entry tree → modify 1 entry → flush → verify the
  on-disk region grows by ~32 B (one new sorted run with one entry +
  header sector overwrite), not by 256 KiB.

---

## B2. Wire `should_compact` / `compact` into commit

**Spec:** IMPL §1.5.4. Trigger when `payload_used > 75%` or
`sorted_run_count > 4`.

**Current state:** Implemented in storage but never invoked.

### Steps

1. **Compaction hook in `DiskEngine::commit`.**
   - For each tree in the engine cache, after `append_sorted_run`:
     ```rust
     let region_size = BtreeRegion::region_size(&tree.header);
     if should_compact(&tree.header, region_size as u32) {
         let compacted = compact(&tree.loaded_node);
         let new_block_ref = self.allocator.alloc(BucketDataType::BtreeNode)?;
         BtreeRegion::write_full(device, block_ref_to_offset(&new_block_ref), &mut compacted)?;
         self.allocator.free(tree.block_ref)?;     // old region abandoned
         tree.block_ref = new_block_ref;
         self.new_root_pointer.X_root = new_block_ref;
     }
     ```
   - Step 3 of spec (§1.5.4 line 326): "Update the parent inner node's
     child pointer". For leaf-only trees this is the RootPointer slot
     itself. For multi-level trees (post-A2), the parent inner node
     mutates → recursive append.

2. **Crash recovery for `BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS`.**
   - On boot, scan every active region's header. If the flag is set,
     re-run compaction (idempotent because we COW into a fresh bucket).
   - Acknowledged as TODO in `btree.rs:33-36`.

### Verification gate
- Test: insert until `payload_used > 75%`, force commit, verify
  `sorted_run_count == 1` post-compact and the old block_ref is freed.

---

## B3. Long-lived `LoadedNode` with merged_view cache

Folded into B1 step 1. Worth listing separately because it has a
distinct performance impact: spec §1.5.3 expects `O(3 × log n)` lookup
amortized via the cached merged_view; current always rebuilds on every
read.

**Verification gate** (post-B1): hot-path benchmark — 100 lookups
against a populated tree, no flush in between. Expect O(log n) per
lookup, not O(n) build of a fresh merged_view per lookup.

---

## B4. Populate `BtreeNodeHeader.min_key` / `max_key`

**Spec:** IMPL §1.5.1 — "covered key range".

**Current state:** Initialized to zeros, never updated. Currently
masked because every R1b tree is leaf-only — the moment A2 ships
multi-level radix or any other multi-level tree, descent becomes
impossible.

### Steps

1. **`LoadedNode` tracks min/max as keys are added.**
   - Encode each key into its 16-byte representation via a new
     `MinMaxKey` trait (`fn to_separator_bytes(&self) -> [u8; 16]`).
   - `flush_to_run`, `compact`, and `append_sorted_run` update
     `header.min_key` / `header.max_key` to the merged extremes.

2. **Inner-node descent honours min_key / max_key.**
   - When following a child pointer, validate the child's `min_key ≤
     search_key ≤ max_key` post-load. Mismatch → tree corruption,
     emit a recovery signal (eventually a reconcile work item).

### Verification gate
- New unit test: build a tree, dump the on-disk header bytes, verify
  the 16 B `min_key` / `max_key` fields equal the smallest / largest
  key in the tree's serialized form.

---

## B5. `LoadedNode.pin: JournalPin`

**Spec:** IMPL §1.5.3 + §3.4 `DirtyNode` accounting.

**Current state:** Field absent. The "WAL ring entries past
`last_persisted_lsn` must not be reclaimed" invariant is unenforced —
WAL truncation could orphan in-memory pending journal entries.

### Steps

1. **`JournalPin` type in WAL crate.**
   - File: `crates/wal/src/pin.rs`.
   - Holds `(lsn_min: u64, lsn_max: u64, count: u32)`. The WAL trimmer
     queries the lowest live `lsn_min` across all pins; can't trim past
     it.

2. **`LoadedNode` owns one pin.**
   - File: `crates/storage/src/btree.rs` — add
     `pub pin: Option<JournalPin>` field. Updated on every
     `push_journal` (lsn_max bumps), drained on `flush_to_run`.

3. **WAL trim respects pins.**
   - `mimisbrunnr-wal` exposes
     `Wal::add_pin(&mut self, pin: JournalPin)` /
     `Wal::release_pin(&mut self, pin: JournalPin)`.
   - `Wal::checkpoint` only advances `read_cursor` to
     `min(lsn, lowest_live_pin_lsn_min)`.

### Verification gate
- Test: push journal entry without flushing, force `wal.checkpoint`,
  verify the WAL doesn't trim past the pin.

---

## D3. Bucket alignment for B+ tree regions  (unblocks A2 multi-level)

> **Cross-ref:** Tier 1 A2's leaf-only landing accepts that trees stay
> at fixed offsets across commits. A2 *multi-level* (inner-node
> descent + tree growth + COW write path) needs the allocator to
> hand out fresh buckets per flush — that's exactly D3's job. Land
> D3 before resuming A2 multi-level.


**Spec:** IMPL §1.5.5 — "A 256 KiB node in a 1 MiB bucket means **4
nodes per bucket**, which is the typical packing".

**Current state:** Engine packs regions back-to-back from offset 0 in
the index zone, ignoring bucket boundaries.

### Steps

1. **Allocator returns bucket-aligned `BlockRef`s.**
   - Mostly free once Tier 1 D2 lands — `BucketAllocator::alloc` returns
     `BlockRef { block_no = bucket_no * sectors_per_bucket, .. }`.
   - Region size 256 KiB; 4 × 256 KiB regions per 1 MiB bucket.
   - Either: (a) one region per bucket (waste of 768 KiB) or (b) pack
     4 regions per bucket. Spec implies (b). Allocator needs a
     "sub-bucket region claim" API:
     ```rust
     pub fn alloc_region(&mut self, kind: BucketDataType) -> Result<BlockRef, StorageError>;
     ```
     Internally claims a bucket (only on first call for that bucket),
     returns successive 256 KiB sub-extents until full, then claims a
     new bucket.

2. **Bucket sealing.**
   - When the allocator runs out of sub-extents in the current bucket,
     mark it `BUCKET_FLAG_SEALED` (new flag — propose) and pick a fresh
     bucket. Once sealed, the bucket's `dirty_sectors` only decreases
     (regions inside can be freed individually but the bucket isn't
     reused until generation bump).

### Verification gate
- After 4 region writes, all 4 sit in the same bucket; the 5th opens
  a new bucket.
- Old region freed → `BucketAllocEntry.dirty_sectors` decreases.

---

## D5. Checkpoint cadence (rest of Theme C)

**Spec:** IMPL §3.5. Cadence: WAL fill thresholds (§3.4 table),
accumulated dirty bytes, 30 s elapsed.

**Current state:** Engine flushes synchronously inside every `commit()`.
No driver thread.

### Steps

1. **Journal-reclaim driver thread.**
   - File: `crates/engine/src/journal_driver.rs`.
   - Spawned by `DiskEngine::open`. Wakes on:
     - WAL `used_bytes / segment_size > 0.75` → flush all dirty trees.
     - `accumulated_dirty_bytes > 8 MiB` → flush.
     - 30 s elapsed since last checkpoint.
   - Calls into `DiskEngine::checkpoint` (extracted from current
     `commit`).

2. **Decouple `commit` from the live mutation path.**
   - Mutations go to WAL + in-memory `LoadedNode` (B1 cache). Flush
     happens on the driver thread's cadence.
   - `commit()` becomes a force-flush primitive for tests and
     foreground sync points.

### Verification gate
- 10 k mutations with no `commit()` call — verify driver thread
  flushes within 30 s.
- WAL fill test: pump WAL past 75% → expect immediate driver wake.

---

## E1, E2. Per-disk roots for `BucketAllocTable` / `FreespaceLru`

**Spec:** IMPL §12.2 / §12.4. Each disk has its own tree rooted at
`DiskDescriptorOnDisk.buckets_root` / `freespace_root`.

**Current state:** Both pool-scoped, key includes `disk_id` redundantly.

### Steps

1. **Per-disk tree storage.**
   - Each `DiskDescriptorOnDisk` already has `buckets_root: BlockRef`
     and `freespace_root: BlockRef` slots — currently `BlockRef::ZERO`.
   - At `mimisbrunnr-pool::PoolManager::open_or_create_disk`, allocate
     a fresh region for each (via the per-disk allocator after Tier 1
     D2 lands).

2. **Drop `disk_id` from the keys.**
   - `BucketAllocKey` becomes `bucket_no: u32` only (4 B; matches
     spec). `FreespaceLruKey` becomes `(band: u8, bucket_no: u32)` (5 B).
   - In-memory mirror keyed by `bucket_no` per disk; engine carries a
     `Vec<BucketAllocTable>` indexed by `disk_id` (or `HashMap<DiskId,
     BucketAllocTable>`).

3. **Allocator parameterized by disk.**
   - `BucketAllocator::for_disk(disk_id)` returns the per-disk view;
     `alloc(BucketDataType::Blob)` for the blob zone, etc.

### Verification gate
- `analyze` reports per-disk `buckets_root` / `freespace_root` non-zero.
- Two-disk pool: each disk's tree has only that disk's bucket
  entries.

---

## Suggested execution order (Tier 3)

1. **B4** (min_key / max_key — small, unblocks proper inner-node descent).
2. **B5** (JournalPin in WAL — small, enables B1).
3. **B1 + B3** (long-lived LoadedNode with append-on-flush; combined
   because they touch the same engine refactor).
4. **B2** (compaction hook).
5. **D5** (driver thread; depends on B1 to have something to drive).
6. **D3** (bucket alignment; depends on D2's allocator API).
7. **E1 + E2** (per-disk roots; depends on D3).
