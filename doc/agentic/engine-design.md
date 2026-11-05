# Engine — design

Scope: `crates/engine` (`Engine`, `DiskEngine`, `OpLog`, commit, boot), excluding WAL replay specifics ([journal-design.md](journal-design.md)).
Governing spec: SPECIFICATION-DESIGN§15, ENGINE-DESIGN§1.2 (root pointer and atomic commit), ENGINE-DESIGN§2 (in-memory mirrors), FORMAT-DESIGN§16 (footprint).

Plan: [engine-implementation-plan.md](engine-implementation-plan.md) · Open decisions: [engine-decisions.md](engine-decisions.md) · Completed: [engine-completed.md](engine-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

The engine orchestrates mutations WAL-first, keeps one long-lived `LoadedNode` per tree, flushes dirty trees copy-on-write into freshly allocated regions, and commits by flipping the active `RootPointer`. Everything else on disk is derived or rebuilt at boot.

## Selected design

- **Root pointer.** Every tree the engine owns is reached through its `RootPointer.*_root` `BlockRef`; a `BlockRef::ZERO` slot loads as an empty tree. Slots for snapshots, reconcile and value spill stay zero until those scopes land.
- **Commit.** Flush dirty trees (append a run, or compact into a fresh region), write the new `RootPointer`, flip it with `commit_root`, then free the previous copies. A crash before the flip leaves the previous root's regions intact.
- **No legacy blob.** The `MIXI` CBOR blob is retired: counters are derived at boot (`next_oid_local` from the object table, `next_tag_id` from the ontology, `last_applied_lsn` from the latest checkpoint), path contexts are rebuilt from the forward index, and blob bytes live in the blob zone.
- **Pools are disposable** until the maintainer declares the format stable: a format change means recreating the pool.
- **One engine on host and device.** The engine and every crate it depends on build as `no_std` + `alloc` (operations-design "Deployment targets"); file-backed devices, `pool.toml` and other host parts sit behind the `std` feature. A pool opens from a `BlockDevice` alone, and opening replays the WAL in memory on every target, the device included.

## Invariants

- A reopened pool reproduces exactly the committed state.
- The engine never hides another crate's invariants; it calls the owning crate's operations.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Superblock and Atomic Root

The superblock is the only structure with a fixed location and is written 3× (offsets 0, 4096, and
last-4096-of-device) for redundancy.

### 1.1 Superblock layout (4 KiB, version 1)

```rust
#[repr(C, packed)]
struct Superblock {                          // 4096 bytes total
    header: BlockHeader,                     //   [0..32]   kind = Superblock
    magic_full: [u8; 16],                    //  [32..48]   "MIMISBRUNNR\0\0\0\0\0"
    fs_uuid: [u8; 16],                       //  [48..64]   pool-wide UUID
    node_id: u16,                            //  [64..66]
    disk_id: u16,                            //  [66..68]
    media_type: u8,                          //  [68..69]   MediaType discriminator
    tier: u8,                                //  [69..70]   StorageTier discriminator
    _pad0: [u8; 2],                          //  [70..72]
    device_capacity: u64,                    //  [72..80]
    block_size_log2: u8,                     //  [80..81]   12 == 4 KiB
    _pad1: [u8; 7],                          //  [81..88]
    creation_timestamp_ns: i64,              //  [88..96]
    last_mount_timestamp_ns: i64,            //  [96..104]
    mount_count: u64,                        // [104..112]

    // Two alternating root pointers — atomic commit. RootPointer = 408 bytes (ENGINE-DESIGN§1.2).
    root_a: RootPointer,                     // [112..520]
    root_b: RootPointer,                     // [520..928]
    active_root: u8,                         // [928..929]   0 = a, 1 = b
    _pad2: [u8; 7],                          // [929..936]

    // Static layout pointers (set at format time, not written again).
    wal_offset: u64,                         // [936..944]
    wal_size: u64,                           // [944..952]
    bucket_size_log2: u8,                    // [952..953]   e.g. 20 = 1 MiB bucket
    copygc_reserve_pct: u8,                  // [953..954]   default 8 (range 5..=21)
    btree_node_size_log2: u8,                // [954..955]   default 18 = 256 KiB (BTREE-DESIGN§1)
    _pad3: [u8; 5],                          // [955..960]
    bootstrap_buckets: u32,                  // [960..964]   reserved leading buckets (sb + WAL + …)
    _pad4: [u8; 4],                          // [964..968]
    zone_map_offset: u64,                    // [968..976]   0 until any zone is grown

    // Initial-extent zone descriptors. Always authoritative for the first extent;
    // additional extents (if any) are listed in the ZoneMap block.
    index_zone:    ZoneExtent,               // [976..1000]  24 bytes
    metadata_zone: ZoneExtent,               // [1000..1024]
    blob_zone:     ZoneExtent,               // [1024..1048]

    encryption_keyid: [u8; 16],              // [1048..1064] key identifier (not the key)
    fs_format_version: u32,                  // [1064..1068] FORMAT-DESIGN§15 — current writing version
    fs_min_on_disk: u32,                     // [1068..1072] FORMAT-DESIGN§15 — minimum version of any record on disk
    compat_features: u64,                    // [1072..1080] FORMAT-DESIGN§15.2 — old readers tolerate
    ro_compat_features: u64,                 // [1080..1088] FORMAT-DESIGN§15.2 — old readers mount RO
    incompat_features: u64,                  // [1088..1096] FORMAT-DESIGN§15.2 — old readers refuse
    downgrade_log_ref: BlockRef,             // [1096..1112] FORMAT-DESIGN§15.5 — chain of historical features

    // Default storage policy (SPECIFICATION-DESIGN§3.5 / SPECIFICATION-DESIGN§8.3) — applied when neither ontology nor
    // PlacementRule resolves an axis. Format-time default: chunking algo = None.
    default_chunking_threshold: u64,         // [1112..1120] objects ≤ this size are never
                                             //              chunked regardless of policy
    default_chunking: ChunkParamsRecord,     // [1120..1136] 16 B (INDEX-DESIGN§4.3)
    value_hash_secret: [u8; 16],             // [1136..1152] SipHash-2-4 key of value_hash
                                             //              (INDEX-DESIGN§1.2); identical in
                                             //              every member's superblock

    _reserved: [u8; 2940],                   // [1152..4092] zeroed, available for future fields
    // trailing CRC32C at [4092..4096] lives inside BlockHeader's frame
}

struct ZoneExtent {                          // 24 bytes
    offset: u64,
    length: u64,
    flags: u32,                              // reserved
    _pad: u32,
}
```

`format_version` in the BlockHeader gates field interpretation. Reserved space is zero-initialised
so flipping bytes from 0 → 1 in a future version is a strict additive change.

The three `*_zone` fields in the superblock describe the **first** extent of each zone — the
common case where a zone is one contiguous range. When a zone is grown by a non-contiguous
addition (e.g. extending the metadata zone after the blob zone has already been allocated past
its old tail), the additional extents are recorded in a `ZoneMap` block:

```rust
#[repr(C, packed)]
struct ZoneMap {                             // 4096 bytes
    header: BlockHeader,                     // [0..32]    kind = ZoneMap
    extent_count: u16,                       // [32..34]   total ZoneMapEntry records below
    _pad: [u8; 6],                           // [34..40]
    extents: [ZoneMapEntry; 126],            // [40..4072] 126 × 32 B = 4032 B
    _pad_tail: [u8; 20],                     // [4072..4092]
    // trailing CRC32C at [4092..4096]
}

#[repr(C, packed)]
struct ZoneMapEntry {                        // 32 bytes
    zone_kind: u8,                           //  [0..1]   0 = index, 1 = metadata, 2 = blob
    _pad: [u8; 7],                           //  [1..8]   align embedded extent to u64
    extent: ZoneExtent,                      //  [8..32]  full 24 B form — same shape as in
                                             //          Superblock; carries `flags` for
                                             //          future per-extent hints
}
```

`Superblock.zone_map_offset` is `0` until the first non-contiguous grow; from that point on
it points to the active `ZoneMap` block. Updates use the same A/B alternation as the
superblock root (two adjacent blocks; active selected by `BlockHeader.generation`). When a
zone reaches 126 additional extents, a follow-on `ZoneMap` block is chained via
`BLOCK_FLAG_CONTINUATION` in `BlockHeader.flags`.

Mount-time zone resolution: walk superblock's first-extent fields, then if `zone_map_offset
!= 0` append every entry whose `zone_kind` matches. The resulting per-zone vector is
authoritative for that zone's address space.

### 1.2 Atomic root commit

The two-root scheme is the only way to update the filesystem state durably without TOCTTOU
windows:

```rust
#[repr(C, packed)]
struct RootPointer {                         // 408 bytes
    seq: u64,                                //   [0..8]    monotonic; larger seq wins
    lsn: u64,                                //   [8..16]   WAL LSN this root corresponds to

    // Logical / data btrees (snapshot-aware unless noted; see SNAPSHOTS-DESIGN§1.2 table).
    object_table_root:        BlockRef,      //  [16..32]   METADATA-DESIGN§1     radix table — current view
    object_history_root:      BlockRef,      //  [32..48]   SNAPSHOTS-DESIGN§1.2  sidecar: (oid, snapshot) overrides
    location_table_root:      BlockRef,      //  [48..64]   METADATA-DESIGN§2.1   radix table — current view
    location_history_root:    BlockRef,      //  [64..80]   SNAPSHOTS-DESIGN§1.2  sidecar
    forward_index_root:       BlockRef,      //  [80..96]   INDEX-DESIGN§2
    tag_index_root:           BlockRef,      //  [96..112]  INDEX-DESIGN§3.1   TagDirectory
    kv_index_root:            BlockRef,      // [112..128]  INDEX-DESIGN§4.1   KvDirectory (4 KiB block)
    range_index_root:         BlockRef,      // [128..144]  INDEX-DESIGN§4.2
    chunk_index_root:         BlockRef,      // [144..160]  INDEX-DESIGN§4.3   content-addressed (snapshot-agnostic)
    value_spill_root:         BlockRef,      // [160..176]  INDEX-DESIGN§2.3   content-addressed by value_hash
    backpointer_root:         BlockRef,      // [176..192]  METADATA-DESIGN§2.2   physical (snapshot-agnostic)
    ontology_root:            BlockRef,      // [192..208]  ONTOLOGY-DESIGN§1  → BTREE-DESIGN§1 region directly (no envelope)
    subscriptions_root:       BlockRef,      // [208..224]  SUBSCRIPTIONS-DESIGN§1
    pool_state_root:          BlockRef,      // [224..240]  POOL-DESIGN§1  scalars + inline disks
    snapshot_chain_root:      BlockRef,      // [240..256]  SNAPSHOTS-DESIGN§1.1  snapshots btree

    // Reconcile btrees (RECONCILE-DESIGN§1.2). Zeroed when unused; *_phys variants are
    // populated only when a rotational disk is present in the pool.
    reconcile_work_root:      BlockRef,      // [256..272]
    reconcile_high_prio_root:     BlockRef,      // [272..288]
    reconcile_work_phys_root: BlockRef,      // [288..304]
    reconcile_high_prio_phys_root: BlockRef, // [304..320]
    reconcile_pending_root:   BlockRef,      // [320..336]
    reconcile_scan_root:      BlockRef,      // [336..352]  RECONCILE-DESIGN§1.3  in-progress scan cursors

    // Pool-state btrees (POOL-DESIGN§1). Promoted from PoolStateRoot for uniform
    // root-anchoring of every btree.
    disks_overflow_root:      BlockRef,      // [352..368]  populated iff disk_count > 12
    placement_rules_root:     BlockRef,      // [368..384]  placement rules
    cluster_peers_root:       BlockRef,      // [384..400]  cluster peers

    flags: u32,                              // [400..404]
    crc: u32,                                // [404..408]  CRC32C of bytes [0..404]
}
```

`BlockRef` is `{ disk_id: u16, _pad: u16, block_no: u32, generation: u64 }` — 16 bytes,
self-validating against the destination block's header.

`BlobRef` is `{ disk_id: u16, _pad: u16, block_no: u32, length: u64 }` — also 16 bytes, used to
address a contiguous payload extent within the blob zone (INDEX-DESIGN§1.1, METADATA-DESIGN§1.2's `OverflowAttr` spill,
INDEX-DESIGN§4.3's chunk index leaf). Unlike `BlockRef`, it carries an explicit `length` (in bytes) rather
than a generation; freshness is derived from the owning record's bucket-generation check at
deref time.

**Commit protocol** (invoked per the cadence in JOURNAL-DESIGN§1.5, *not* per mutation):

1. Quiesce: stop new flushes from advancing past `commit_lsn`. Pending mutations keep going to
   the WAL.
2. Write all dirty btree pages produced by the journal-reclaim thread into open buckets (bucket
   allocation tracked in WAL only — see ALLOCATION-DESIGN§1).
3. Append `Checkpoint` WAL entry referencing the new pages and computing the new `RootPointer`.
4. `fsync` the WAL segment.
5. Write the new `RootPointer` into the **inactive** slot of all superblock copies, flip
   `active_root`, recompute superblock CRC, write all 3 superblock copies, `fsync` the device.
6. Advance `WalHeader.read_cursor` to the new `RootPointer.lsn`; old root pages are released to
   the allocator (their buckets become candidates for generation bump once no live snapshot
   pins them).

A reader picks the superblock copy with the highest `(seq, lsn)` whose CRC validates and whose
active root validates. If active root is corrupt, fall back to inactive root (the last good
checkpoint). If both roots are corrupt, replay the WAL forward from the inactive root's `lsn`.

### 1.3 Block addressing

Within a single device, blocks are addressed by `block_no` (4 KiB unit). Across the pool,
`(disk_id, block_no)` uniquely identifies a block. Devices may grow; `block_no` is never
re-mapped.

**Positional access** for the `ObjectRecord` and `ObjectLocation` arrays is preserved at the
*per-leaf* level: the radix descent in METADATA-DESIGN§1 / METADATA-DESIGN§2.1 resolves `oid_local` to a leaf `BlockRef`,
and the record lives at slot `oid_local % LEAF_RECORDS` (2044 for the object table, 5440 for
the location table) within that leaf. The leaves themselves are COW pages whose physical
addresses are indirected through `object_table_root` / `location_table_root`; there is no
zone-wide flat array.

---

## 2. In-Memory Mirror Structures

The on-disk format is the source of truth. The in-memory layer is a **cache**, structured for fast
queries:

| In-memory type                               | Mirrors on-disk                           | Lifetime                  |
| -------------------------------------------- | ----------------------------------------- | ------------------------- |
| `ObjectTable` (positional accessor over `BTreeNodeCache`) | METADATA-DESIGN§1 radix tree of `BtreeKind::ObjectTable` leaves; per-record access = (radix descent → leaf `BlockRef`) → cache lookup → 128 B slot at `oid_local % 2044` | leaves cached per `BTreeNodeCache` policy below; no separate resident array (~1.19 GiB at 10 M objects, ≥ TiBs at the 48-bit cap) |
| `LocationTable` (positional accessor over `BTreeNodeCache`) | METADATA-DESIGN§2.1 radix tree of `BtreeKind::LocationTable` leaves; per-record access = same descent → 48 B slot at `oid_local % 5440` | shares `BTreeNodeCache`; same eviction policy as `ObjectTable` |
| `ObjectHistory`, `LocationHistory` (snapshot-override sidecars) | SNAPSHOTS-DESIGN§1.2 — `BtreeKind::ObjectHistory` / `BtreeKind::LocationHistory` keyed by `(oid, snapshot)`; consulted only for non-current-snapshot reads | shares `BTreeNodeCache`; typically cold (current-snapshot reads bypass) |
| `TagIndex { HashMap<TagId, TagStore> }`      | TagDirectory + TagBitmap pages            | mmap-pinned roaring containers |
| `KvIndex { HashMap<(TagId,u64), RoaringBitmap> }` | KvDirectory + buckets                  | resident, lazy-load buckets |
| `RangeIndex` (`BTreeMap<(TagId, NormKey), Roaring>`) | B+ tree pages                       | resident, paged in     |
| `ForwardIndex { HashMap<u64, SmallVec<...>> }` | B+ tree                                  | LRU-cached pages       |
| `OntologyState`                              | OntologyRoot                              | fully resident         |
| `ImplicationDag` (`petgraph::Graph<TagId, ()>`) | dag pages                              | fully resident         |
| `SubscriptionEngine`                         | SubscriptionsRoot                         | fully resident         |
| `PoolManager`                                | PoolStateRoot                             | fully resident         |
| `BucketCache` (`HashMap<(DiskId, u32), BucketAllocEntry>`) | per-disk buckets B+ tree    | hot buckets pinned, cold paged in |
| `WritePoints` (`HashMap<(DiskId, DataType, StreamTag), OpenBucket>`) | derived             | resident; ~hundreds of entries |
| `BTreeNodeCache` (`HashMap<BlockRef, LoadedNode>` + LRU) | BTREE-DESIGN§1 large nodes (256 KiB each) | journal-pinned nodes never evicted; clean nodes LRU. Working-set ≈ 100–500 hot nodes ⇒ 25–125 MiB. |
| `BackpointerCache` (LRU of bucket-prefix scan results) | METADATA-DESIGN§2.2 backpointer btree leaves | populated on demand by copygc / scrub / resilver |
| `SnapshotTree` (`BTreeMap<SnapshotId, SnapshotNode>` + ancestor cache) | SNAPSHOTS-DESIGN§1.1 snapshots btree | fully resident — typically thousands of entries; ancestor checks must be in-cache |
| `ReconcileEngine` (priority queue heads, throttle counters, move-path semaphore) | RECONCILE-DESIGN§1 reconcile btrees | resident; queue heads cached, deeper queue paged from btree |
| `ScanRegistry` (`HashMap<ScanId, ScanState>`) | RECONCILE-DESIGN§1.3 ReconcileScan | resident — small (< 100 active scans) |
| `JournalReclaim` (`BinaryHeap` of (pin_pressure, BlockRef)) | derived from BTreeNodeCache | resident; rebuilt on demand |
| `OpLog` (`VecDeque<OpLogEntry>`)             | recent WAL tail                           | trimmed at checkpoint  |

`Engine` (SPECIFICATION-DESIGN§15) owns these and is wrapped by `DiskEngine` which adds `FileBlockDevice`,
superblock, allocator, and snapshot manager. All mutations follow the pipeline below; see
JOURNAL-DESIGN§1.4's *Locking model* for the lock domains referenced here:

```
1. Acquire the per-tree write mutex (JOURNAL-DESIGN§1.4) for the target btree.
2. Tag the mutation with the **current snapshot id** (SNAPSHOTS-DESIGN§1.2) — for snapshot-aware btrees, the
   key includes `snapshot = current_snapshot_id`.
3. Append WAL entry (fsync if durability mode = sync).
4. Apply to in-memory mirror (idempotent on lsn).
5. Release the per-tree write mutex.
6. Take per-DirtyNode exclusive locks briefly to bump `pending_lsn_max` /
   `pending_count` on each affected node. Page rewrites are deferred to the
   journal-reclaim thread (JOURNAL-DESIGN§1.4).
```

The per-tree mutex covers steps 1–4 (LSN allocation, WAL append, in-memory-mirror update);
step 6's per-DirtyNode locks are independent and uncontended in steady state because the
flush thread holds the same DirtyNode rwlock in *shared* mode while building new pages.
**Reads** consult the arc-swap'd index handle and are never blocked on either lock domain.

### 2.1 Roaring bitmap representation

Use the `roaring` crate's standard structure (`RoaringBitmap`). Containers can be built directly
from mmapped portable-format bytes via the crate's `RoaringBitmap::deserialize_from_slice` — this
is essentially zero-copy for array and bitmap containers (run containers may copy).

### 2.2 Inode identity for FUSE

The FUSE bridge (`TagVfs`) does **not** use object IDs as inodes — it allocates 64-bit inodes
lazily as paths are traversed (a `BTreeMap<TagSet, u64>` and `BTreeMap<u64, TagVfsEntry>`). Inodes
are session-local; they are never persisted, since the TagVfs view is a derived projection.

---

