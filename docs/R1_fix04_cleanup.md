# R1 fix plan — Tier 4: Cleanup

Items here are cosmetic / minor format drift. Land them after Tiers 1–3
remove the underlying causes; rolling them in earlier means re-doing the
work.

Prerequisite: Tiers 1–3 land first.

## Status (2026-05-11)

| Item | Status | Notes |
|------|--------|-------|
| **D4** Retire the legacy `MIXI` CBOR blob | ❌ Not started | Still carries `path_contexts_bytes`, `oplog_bytes`, scalars, transient `blobs` map. |
| **F1** `ChunkEntrySerde.length` field | ✅ Folded into A3.1 | Wire form is now the spec's `ChunkIndexLeafEntry` byte image. |
| **F4** Doubly-serialized POD bytes in CBOR runs | 🟡 Partial | A2-leaf / A1 / A3.1 / A3.2 / A3.3 dropped their CBOR wrappers. Bucket-alloc / backpointer / freespace still CBOR-wrap PODs (pending E1 / E2). |
| **F5** Newtype proxy explosion | 🟡 Partial | `Chunk*`, `Forward*`, `Kv*` proxies now carry the spec wire form (not removed but no longer R1b-flavoured). Still pending after Tier 3 lands. |
| **F6** `BTREE_NODE_FLAG_HEAD_OF_CHAIN` ChunkList | ❌ Not started | Scoped under R4 (real blob zone). |
| **F7** `BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS` recovery | ❌ Not started | Folded into Tier 3 B2. |
| **F8** `EngineError::CborEncode` reused for non-CBOR | ❌ Not started | Watch + ontology still lose error context. |
| **F9** `Engine.next_tag_id` vs ontology allocator | ❌ Not started | Two sources of truth for tag-id allocation. |
| **F10** `BtreeRegion::read` hard-fails on packed-keys flag | ❌ Not started | Today callers must pre-pick `read` vs `read_packed`. |

---

## D4. Retire the legacy `MIXI` CBOR blob

**Spec:** No MIXI blob exists anywhere in IMPL. Recovery model is "WAL
replay from last checkpoint + B+ tree roots in `RootPointer`".

**Current state:** `crates/engine/src/disk_engine.rs:147-167` defines
`IndexBlob { path_contexts_bytes, oplog_bytes, next_oid_local,
next_tag_id, last_applied_lsn, blobs }`. Read at `LEGACY_CBOR_BLOB_OFFSET
= 2304*1024`. The blob writes / reads are inside `save_index_state` /
`load_index_state`.

### Steps

1. **`oplog` migration.** Spec doesn't define an oplog explicitly — it's
   an engine-side construct for WAL replay convenience. Move it into a
   dedicated B+ tree region (key = `oplog_seq u64` → `OpLogEntry`). New
   `BtreeKind::OpLog = N` (request a spec slot). Or: rebuild from WAL on
   boot (preferred — eliminates oplog persistence entirely).

2. **`path_contexts` rebuild on boot.** Per IMPL §10.3 the
   `PathContextManager` is a derived view of the forward index. On
   `DiskEngine::open`, scan the forward index for `Attr(unix-path,
   *)` assertions and rebuild the manager. The crate-level docs in
   `crates/unix/src/manager.rs` already describe this — implement.

3. **Scalars become superblock fields or derived.**
   - `next_oid_local`: derive from `max(object_table.keys()) + 1` at
     boot. No persistence needed.
   - `next_tag_id`: derive from `max(ontology.tags.keys()) + 1`.
   - `last_applied_lsn`: read from the most recent `Checkpoint` WAL
     entry's `RootPointer.lsn`.

4. **`blobs: HashMap<u64, Vec<u8>>`.** Transient, replaced by the real
   blob zone in R4 — drop entirely. (Pre-R4 callers that exercise blobs
   through the engine will need to be gated until R4 lands.)

5. **Drop `IndexBlob`, `INDEX_BLOB_MAGIC`, `LEGACY_CBOR_BLOB_OFFSET`,
   `read_index_blob`, `apply_index_blob`, `build_index_blob`.**
   - One-way format break; document in
     `docs/REWRITE_FOLLOWUP.md` under the migration log.

### Verification gate
- Format a fresh pool, insert tags + objects + a path context,
  `commit()`, drop, reopen. Every piece of state survives without
  any MIXI blob on disk. Verify with `bin/analyze`.

---

## F1. `ChunkEntrySerde.length` field

Folded into Tier 2 A3.1. After A3.1 ships, `ChunkEntrySerde` and
`BlobRefSerde` proxies are deleted (the on-disk wire form is the spec's
`ChunkIndexLeafEntry` byte image directly). Length is naturally
preserved.

**Verification gate:** `BlobRefSerde` and `ChunkEntrySerde` no longer
exist in `crates/index/src/chunk_index.rs`.

---

## F4. Doubly serialized POD bytes in CBOR sorted runs

**Current state:** Every POD type (ObjectRecord 128 B, ObjectLocation
48 B, BackpointerKey/Value, BucketAllocEntry, DiskDescriptorOnDisk,
…) has a hand-rolled `Serialize` that emits
`serialize_bytes(bytemuck::bytes_of(self))`. CBOR wraps this with its
own length tag.

### Steps

1. Once Tier 1 A2 ships native positional radix encoding for
   ObjectTable / LocationTable, drop the `Serialize` / `Deserialize`
   impls on `ObjectRecord` and `ObjectLocation` — the wire form is
   the POD byte image directly, no CBOR.
2. Once Tier 1 A1 ships native KvIndex extendible hash, the CBOR
   encoder for that path is gone too.
3. Once Tier 2 A3.1 ships native ChunkIndex leaves, drop the
   `Serialize` impl on `BucketAllocEntry` (well — it's still used by
   the bucket alloc tree until the per-disk packed-key migration in
   Tier 3 E1).
4. Once Tier 3 E1/E2 ship per-disk packed-key trees,
   `BucketAllocEntry`'s manual `Serialize` and `BackpointerKey/Value`'s
   manual `Serialize` go away — replaced by the `PackableKey` impl.

5. **`crates/meta/src/serde_pod_bytes.rs`** (the byte-image visitor
   helper) is deleted entirely once every consumer migrates off the
   CBOR path.

### Verification gate
- `Grep "serialize_bytes(bytemuck::bytes_of" crates` returns no matches.
- `crates/meta/src/serde_pod_bytes.rs` removed.

---

## F5. Newtype proxy explosion

**Current state:** ~22 public newtypes (`ChunkIndexKey`,
`ChunkIndexValue`, `ChunkHashKey`, `BlobRefSerde`, `ChunkEntrySerde`,
`RoaringBitmapSerde`, `RangeIndexKey`, `RangeIndexValue`,
`ObjectTableKey`, `ObjectTableValue`, `LocationTableKey`,
`LocationTableValue`, `ForwardIndexKey`, `ForwardIndexValue`,
`KvIndexKey`, `KvIndexValue`, `BucketAllocKey`, `FreespaceLruKey`,
`PersistedState`, `PersistedEngine`, `PersistedSub`, `DagSnapshot`)
introduced by R1b. None appear in IMPL.

### Steps

Each newtype is paired with its R1b consumer. As Tier 1/2/3 land the
spec-mandated wire form, the corresponding proxy disappears:

| Proxy                     | Removed by   |
|---------------------------|--------------|
| `ChunkIndexKey`/`Value`/`ChunkHashKey`/`BlobRefSerde`/`ChunkEntrySerde` | A3.1 |
| `RangeIndexKey`/`Value`/`RoaringBitmapSerde` | F3 + A3.3 |
| `ObjectTableKey`/`Value`   | A2 (positional radix has no per-key K/V split) |
| `LocationTableKey`/`Value` | A2 |
| `ForwardIndexKey`/`Value`  | A3.2 |
| `KvIndexKey`/`Value`       | A1 |
| `BucketAllocKey`           | E1 (per-disk: key reduces to `u32`) |
| `FreespaceLruKey`          | E2 |
| `PersistedState`           | A4 (split into TagDefRecord + DAG pages + module manifest) |
| `PersistedEngine`/`PersistedSub` | A5 (per-subscription becomes the entry value) |
| `DagSnapshot`              | A4 (replaced by `ImplicationDagPages`) |

### Verification gate
- Every proxy on the list above absent from the codebase.

---

## F6. `BTREE_NODE_FLAG_HEAD_OF_CHAIN` ChunkList chain handling

**Spec:** IMPL §9.3 line 230 — first ChunkList region in an object's
chain carries `ChunkParamsRecord`; chained regions don't.

**Current state:** Bit defined in `crates/storage/src/btree_node.rs:23`
but never set / read. ChunkList chain handling entirely deferred.

### Steps

1. Implement `ChunkList` 256 KiB region writer / reader (positional, no
   sorted runs). First in chain has the head flag set + a
   `ChunkParamsRecord` immediately after the `BtreeNodeHeader`. Tail
   regions chain via a trailing `BlockRef`.
2. Engine consumes via `DiskEngine::read_blob(oid)` for FastCDC objects.
3. Belongs to **R4** (real blob zone & content paths) per
   `REWRITE_FOLLOWUP.md`. Cross-listed here as F6 because the bit
   handling and the BtreeNodeHeader recovery flag check belong to the
   same cleanup.

### Verification gate
- Round-trip a 16 MiB FastCDC object through write → flush → reload →
  read; verify the chain head's ChunkParamsRecord is read but tail
  regions skip the field per the flag.

---

## F7. `BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS` recovery

Folded into Tier 3 B2 step 2 above.

---

## F8. `EngineError::CborEncode` reused for non-CBOR errors

**Current state:** `crates/watch/src/engine.rs:506` and
`crates/ontology/src/state.rs` map `BtreeRegion::write_full`'s
`StorageError` into `WatchError::CborEncode` / `OntologyError::Cbor` even
when the underlying error is `RegionFull` or device I/O.

### Steps

1. Add proper variants:
   - `WatchError::Storage(#[from] StorageError)`.
   - `OntologyError::Storage(#[from] StorageError)`.
2. Replace the lossy `.map_err(|e| WatchError::CborEncode(e.to_string()))`
   with `?` + `From`.

### Verification gate
- `cargo clippy -p mimisbrunnr-watch -p mimisbrunnr-ontology` clean.
- A region-full failure surfaces as the right variant in tests.

---

## F9. `Engine.next_tag_id` divergence from ontology allocator

**Spec:** Ontology owns the tag-id allocator (§3.5 / §10.1).

**Current state:** `crates/engine/src/engine.rs:56` carries a private
`next_tag_id: u32` separately from `OntologyState`. `register_tag`
bypasses `OntologyState::install`.

### Steps

1. **Drop `Engine.next_tag_id`.** Ontology's `IdAllocator` is the
   single source of truth.
2. **`Engine::register_tag` calls `OntologyState::install`.** Wraps
   the ad-hoc tag in a synthetic single-tag `OntologyModule` named
   `"_engine_adhoc"` so it goes through the same install path as a
   user module.
3. **`next_tag_id` derivation on boot** moves into `OntologyState`
   itself (it already has `fresh_allocator()` at `state.rs:400`).

### Verification gate
- The legacy CBOR blob's `next_tag_id` field is unused after F9 — fold
  into D4's deletion.

---

## F10. `BtreeRegion::read` errors out hard on `SORTED_RUN_FLAG_PACKED_KEYS`

**Current state:** `crates/storage/src/btree.rs:559-562` returns
`StorageError::UnsupportedFormatVersion(0)`.

### Steps

1. Detect packed flag in `BtreeRegion::read`; transparently dispatch to
   `read_packed` if every run carries the flag, or error with a clearer
   `StorageError::MixedRunFormats { offset }` if the runs are mixed.
2. Drop the `UnsupportedFormatVersion(0)` misuse.

### Verification gate
- A region written by `write_full_packed` is readable by
  `BtreeRegion::read` without the caller specifying the codec.

---

## Suggested execution order (Tier 4)

The Tier 4 items are best executed *together* once Tiers 1–3 settle, as
they're mostly proxy-removal that follows the spec landings:

1. **F4 + F5 + F1 + F8 + F9** as a single sweep after Tier 1/2 land
   (proxy removal, error variants, allocator unification).
2. **F10** alongside (storage read polish).
3. **D4** last — once nothing else writes to the legacy blob.
4. **F6** is properly its own item, scoped under R4 (blob zone), cross-listed here.
