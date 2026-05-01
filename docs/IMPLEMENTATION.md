# Mímisbrunnr — Implementation

This document specifies the **on-disk** and **in-memory** data structures that realise the design in
[DESIGN.md](DESIGN.md). It is the contract between the storage layer, the index layer, and the
cluster-sync layer.

Goals:

1. **Zero-copy access.** A `mmap()` of any zone returns directly castable, naturally aligned
   structures. No deserialisation step on the hot path.
2. **Format evolution.** Every persistent structure carries a version tag and a length so newer
   readers can skip unknown trailing fields and older readers can refuse cleanly.
3. **Atomic root updates.** Mutations land in the WAL, then in shadowed (alternating) root blocks.
   A torn write never produces a corrupt root.
4. **Key-level snapshots.** Snapshots are 32-bit IDs embedded in the position of every key in
   snapshot-aware btrees. Creation is O(1); diffs are set differences over the snapshot tree's
   ancestor relation, driving cluster sync.
5. **No JSON on disk.** Fixed binary layouts everywhere they fit. CBOR (RFC 8949) only where the
   shape is genuinely heterogeneous (ontology module manifests, attribute `Value`s).

---

## 1. Format Conventions

### 1.1 Byte order, alignment, packing

- All multi-byte integers are **little-endian**.
- All fixed-size on-disk structs are `#[repr(C)]`, with `Pod + Zeroable` from `bytemuck`. This
  permits `bytemuck::cast_slice` over an mmapped region for zero-copy traversal.
- No bit-packed fields cross a byte boundary (use a `u32` flags word, not C bitfields).
- All structs are sized as a multiple of 8 bytes; explicit `_pad` arrays document tail padding.
- Strings on disk are UTF-8, length-prefixed (`u16 len`), never NUL-terminated.

### 1.2 Block size

- Logical block: **4 KiB**. Every persistent structure is laid out so that:
  - Records that must be updated atomically fit in one logical block, or
  - Records spanning multiple blocks use a shadow-paging / log-structured update.
- All offsets in superblocks and zone maps are byte offsets, but always 4 KiB-aligned.

### 1.3 Per-block header (`BlockHeader`)

Every persistent header — both 4 KiB block headers (this section) and 256 KiB region headers
(§1.5.1) — begins with the same 8-byte `BlockPreamble`. A generic reader can read those 8 bytes
to identify the structure (atomic 4 KiB block vs. append-only 256 KiB region) and dispatch to
the right parser:

```rust
#[repr(C, packed)]
struct BlockPreamble {            // 8 bytes — first field of every persistent header
    magic: [u8; 4],               // "MIMR" = 4 KiB block; "MIMB" = 256 KiB region (§1.5.1)
    kind: u16,                    // BlockKind (when magic = "MIMR") or BtreeKind ("MIMB")
    format_version: u16,          // per-kind structural version
}
```

Every block-sized persistent structure that is written or replaced as a unit begins with a
32-byte `BlockHeader`:

```rust
#[repr(C, packed)]
struct BlockHeader {              // 32 bytes
    pre: BlockPreamble,           // [0..8]   magic = "MIMR", kind ∈ BlockKind
    payload_length: u32,          // [8..12]  bytes following the header (excl. trailing CRC).
                                  //          u32 (not u16) leaves headroom for >64 KiB blocks
                                  //          in a future format revision; values for the current
                                  //          4 KiB block are < 4096.
    generation: u64,              // [12..20] monotonic per-block generation, for COW
    lsn: u64,                     // [20..28] WAL LSN that produced this block
    flags: u32,                   // [28..32] bit 0 = encrypted, bit 1 = continuation
}
```

A block is `BlockHeader | payload | u32 CRC32C(BlockHeader || payload)` — total = 4096 bytes by
construction. CRC is computed with the CRC slot itself zeroed.

`BlockKind` enumerates:

```rust
enum BlockKind {
    Superblock,
    ZoneMap,
    WalSegment,                              // WAL header / segment marker
    TagBitmapPage,                           // roaring bitmap framing (4 KiB; §8.2)
    KvHashBucket,                            // extendible-hash bucket (4 KiB; §9.1)
    OverflowRecord,                          // per-object tag/attr overflow (4 KiB; §5.2)
    Checkpoint,                              // checkpoint block within the WAL
}
```

Large-node regions (256 KiB B+ tree nodes and radix leaves; §1.5) carry **`BtreeNodeHeader.kind`**
of type `BtreeKind` instead of `BlockHeader.kind`:

```rust
enum BtreeKind {
    ObjectTable,        // §5 radix leaves & inners (current view)
    ObjectHistory,      // §11.2 sidecar: (oid, snapshot) → ObjectRecord overrides
    LocationTable,      // §6.1 radix leaves & inners (current view)
    LocationHistory,    // §11.2 sidecar: (oid, snapshot) → ObjectLocation overrides
    Backpointer,        // §6.2 reverse-mapping B+ tree (snapshot-agnostic)
    Forward,            // §7 forward index B+ tree (snapshot-aware key)
    ForwardOverflow,    // §7.2 per-object assertion spill (positional, no bsets)
    TagDirectory,       // §8.1 tag directory B+ tree (snapshot-aware)
    Range,              // §9.2 range index B+ tree (snapshot-aware)
    ChunkIndex,         // §9.3 chunk index B+ tree (content-addressed; snapshot-agnostic)
    KvDirectory,        // §9.1 extendible-hash directory spillover (positional)
    ValueSpill,         // value-hash → CBOR(Value); content-addressed
    Ontology,           // §10.1 ontology / dag B+ tree (snapshot-aware)
    PathContext,        // §10.3 path context B+ tree (snapshot-aware)
    Subscriptions,      // §10.2 subscription B+ tree (snapshot-aware)
    Snapshots,          // §11.1 snapshot tree (SnapshotId → SnapshotNode)
    BucketAlloc,        // §12.2 per-disk bucket alloc B+ tree (physical)
    FreespaceLru,       // §12.4 per-disk freespace LRU B+ tree (physical)
    ReconcileWork,      // §17.2 normal-priority reconcile queue (logical order)
    ReconcileHipri,     // §17.2 high-priority reconcile queue
    ReconcileWorkPhys,  // §17.2 physical-LBA-ordered work index (HDD pools)
    ReconcileHipriPhys, // §17.2 physical-LBA-ordered hipri index (HDD pools)
    ReconcilePending,   // §17.2 failed items awaiting device-config retry
    ReconcileScan,      // §17.3 in-progress scan cursors
}
```

The **format_version** is *per-kind*, not global. Old kinds are evolved independently. A reader
that encounters an unknown `(kind, format_version)` aborts with a clear "format too new" error.

### 1.4 CRC

CRC32C (Castagnoli, hardware-accelerated on x86 SSE 4.2 and ARMv8 CRC). Sufficient for 4 KiB blocks
and faster than CRC32. BLAKE3 is used for *content* integrity (per-object), not for block
checksums.

### 1.5 Large node format

All B+ tree nodes and radix-tree leaves are **256 KiB** (configurable 128–512 KiB at format time;
`Superblock.btree_node_size_log2`, default 18 = 256 KiB). This is the bcachefs model: shallow
trees, large sequential reads/writes, and node-internal log structure that lets a flush append
new keys without rewriting the whole node.

A 256 KiB node in a 1 MiB bucket means **4 nodes per bucket**, which is the typical packing.
Nodes do not span buckets; if a node would fill its region, it triggers a split or full
compaction (§1.5.4) that writes the new node(s) into a fresh bucket region.

Two large-node variants share the same outer envelope but differ in their internal layout:

- **B+ tree node** (forward index, range index, alloc table, freespace LRU, ontology, path
  contexts, subscriptions): a sequence of **bsets** — sorted runs of keyed records. New updates
  append a new bset; periodic full compaction merges all bsets back into one.
- **Radix leaf** (object table, location table): a positional array of fixed-size records. No
  internal bsets — the WAL journal (§3.4) serves as the per-leaf update log; on flush the leaf
  is rewritten from the merged in-memory state.

#### 1.5.1 Region envelope

Every large node begins with a 64-byte `BtreeNodeHeader`:

```rust
#[repr(C, packed)]
struct BtreeNodeHeader {                     // 64 bytes
    pre: BlockPreamble,                      // [0..8]   magic = "MIMB", kind ∈ BtreeKind (§1.3)
    seq: u64,                                // [8..16]  monotonic per-region; bumped on each rewrite
    last_persisted_lsn: u64,                 // [16..24] BlockHeader.lsn analogue
    region_size_log2: u8,                    // [24..25] 18 = 256 KiB
    level: u8,                               // [25..26] 0 = leaf, ≥1 = inner
    bset_count: u8,                          // [26..27] number of bsets present
    flags: u8,                               // [27..28] bit 0 = compaction-in-progress (recovery hint)
    payload_used: u32,                       // [28..32] bytes consumed by all bsets so far (≤ region_size − 64)
    min_key: [u8; 16],                       // [32..48] covered key range (interpreted per-kind)
    max_key: [u8; 16],                       // [48..64] ditto
}
```

`BlockPreamble` is the 8-byte common prefix shared with `BlockHeader` (§1.3), so a generic
reader can identify any persistent header by its first 8 bytes. The two header shapes
**diverge after the preamble** because their storage models are genuinely different:

- `BlockHeader` describes a **write-once 4 KiB unit** with a single trailing CRC over the
  whole block. `payload_length` is bounded by the block size; `flags` carries per-block
  encryption / continuation bits.
- `BtreeNodeHeader` describes an **append-only 256 KiB region** that is rewritten only at
  its header sector (§1.5.2). There is no whole-region CRC — each bset carries its own CRC,
  so a torn append invalidates only the trailing bset rather than the whole region.
  `payload_used` is a high-water mark that grows monotonically across header rewrites within
  one region's lifetime; `flags` carries region-rewrite hints.

Forcing a single header shape onto both would require (a) inventing a "CRC that is not a
CRC" slot for regions, and (b) overloading `payload_length`/`flags` across two unrelated
semantic spaces. Keeping them distinct, with the shared preamble making the dispatch
explicit, is cleaner.

Subsequent bytes after the `BtreeNodeHeader` are a **stream of bsets**, each preceded by:

```rust
#[repr(C, packed)]
struct BsetHeader {                          // 32 bytes
    magic: u32,                              // "BSET"
    seq: u32,                                // monotonic within the region
    journal_seq: u64,                        // newest WAL LSN merged into this bset (recovery)
    entry_count: u32,                        // entries in this bset
    payload_length: u32,                     // bytes of bset payload
    flags: u32,                              // bit 0 = packed-keys, bit 1 = encrypted
    crc: u32,                                // CRC32C over (BsetHeader || payload), CRC slot zeroed
}
```

The CRC scope is **per-bset**, not per-4 KiB-block. Each bset is therefore an independently
verifiable, append-only commit unit. A torn write of a partial bset fails its CRC and is
discarded — earlier bsets remain valid. There is no trailing CRC over the whole region.

#### 1.5.2 Append-only growth

When the journal-reclaim thread (§3.4) decides to flush a node:

1. Materialise the pending journal entries that target this node into a new sorted bset.
2. Append `BsetHeader` + bset payload at offset `payload_used` within the region.
3. Update `BtreeNodeHeader.bset_count`, `payload_used`, `last_persisted_lsn`.
4. Rewrite **only** the modified bytes (the new bset, plus a re-checksummed header) — typically
   a few hundred KB, not the whole 256 KiB.

Because the bucket is write-once-then-recycle (§12), the new bset lands at the next free sectors
of the bucket. The header rewrite at offset 0 is a single 4 KiB-sector overwrite — a permitted
operation because the region is reserved within the bucket and only the header sector is
re-touched.

> **Note.** Strictly write-once buckets cannot accept overwriting the header sector. Buckets
> hosting btree nodes are flagged `BucketDataType::BtreeNode` and tolerate **header-sector
> overwrites only** (the rest of the region remains append-only). On SMR / zoned drives this
> constraint moves header rewrites to a separate co-located header bucket; see §12.5 for the
> zoned variant.

#### 1.5.3 In-memory representation

Loaded nodes are decoded into:

```rust
struct LoadedNode {
    header: BtreeNodeHeader,
    bsets: SmallVec<[Bset; 4]>,              // typically 1–3 active bsets
    merged_view: BTreeMap<Key, Value>,       // lazy: built on first lookup
    pending_journal: Vec<JournalEntry>,      // §3.4 entries past last_persisted_lsn
    dirty: bool,
    pin: JournalPin,
}
```

Lookups merge-search across bsets; bsets are kept sorted at write time. With ≤ 3 active bsets
each binary-searched, lookup cost is `O(3 × log(n))` per node — equivalent to a single sorted
search at the constant-factor bcachefs measures at < 5% overhead.

#### 1.5.4 Full compaction

When a node's `payload_used` exceeds 75 % of region size, or `bset_count > 4`, full compaction
runs:

1. Allocate a fresh region in a new bucket (via the standard write-point mechanism, §12.5).
2. Merge-sort all bsets into a single bset; write it as bset 0 in the new region.
3. Update the parent inner node's child pointer (which itself may need a flush — propagates
   up the tree).
4. Old region is abandoned; its bucket's `dirty_sectors` decreases. The bucket becomes a copygc
   candidate (§12.6) when fragmentation is high enough.

Full compactions are O(node_size) per node but rare — bcachefs measures ~1 per 100–1000 flushes
on typical workloads.

#### 1.5.5 Why this matters

For a 10 M-object pool, the radix object table is **2 levels** deep; tag, range, and forward
indexes are **2–3 levels**. Each node access is a single sequential I/O of 256 KiB — critical
for HDD performance and friendly to SSD command queues. The cache holds whole nodes, so
intra-node lookups are memory-resident after the first hit.

#### 1.5.6 Bset format descriptors and packed keys

Each B+ tree bset (§1.5.1) carries a per-bset **format descriptor** that exploits commonalities
across the bset's key range to compress keys end-to-end. Constant fields (e.g. `attr_id` within
a range-index leaf where every key shares the same attribute) consume **zero bits** and are
recorded once in the descriptor rather than per key.

```rust
#[repr(C, packed)]
struct BsetKeyFormat {                       // 8 + nr_fields × 12 bytes
    nr_fields: u8,                           // 1..=8
    key_header_bytes: u8,                    // 1..=4 (entry-type discriminator + flags)
    common_value_prefix: u8,                 // bytes shared at the start of every value (0..=24)
    _pad: u8,
    sum_bit_width: u32,                      // total packed-key bits (incl. header), informational
    fields: [FieldFormat; nr_fields],
}

#[repr(C, packed)]
struct FieldFormat {                         // 12 bytes
    bit_width: u8,                           // 0..=64;  0 ⇒ constant (use `base` directly)
    flags: u8,                               // bit 0 = signed, bit 1 = MSB-first
    _pad: u16,
    base: u64,                               // value subtracted from each field at write time
}
```

The descriptor is part of the `BsetHeader` payload (§1.5.1), prepended before the packed-key
stream. With the typical 3-field shape it adds 8 + 36 = 44 bytes per bset — amortised over
hundreds to thousands of keys.

**Encoding.** A packed key is:

```
[ key_header_bytes of type / flags ]
[ ∑ field[i].bit_width  bits of (field[i] − base[i]) for each field ]
[ pad to byte boundary ]
[ value bytes; first `common_value_prefix` bytes elided ]
```

Keys are laid out back-to-back with no inter-key padding. Binary search within a bset compares
packed keys **directly** without decoding — base subtraction is strictly monotonic, so packed
ordering matches unpacked ordering. Full decoding happens only at the lookup boundary.

**Format selection.** Full compaction (§1.5.4) computes an optimal format for the merged bset
by scanning the key distribution: `max − min` for each field gives the minimum bit width.
Append-on-flush keeps the existing format. If a new key's field overflows the format's bit
width, the flush either:

- Promotes the bset's format (rewriting the bset under a wider format — rare), or
- Triggers a full compaction of the node, producing a fresh format.

Format upgrades are journalled as a `FormatPromote` WAL op so recovery can reconstruct the
in-memory bset state.

**Typical savings across our key shapes** (snapshot-aware btrees include a trailing
`snapshot: u32` field, typically 0–4 bits per leaf since one snapshot dominates):

| Index             | Unpacked key | Typical packed | Saving | Driver                            |
| ----------------- | ------------ | -------------- | ------ | --------------------------------- |
| Forward leaf      | 12 B (oid + snapshot) | 2–4 B  | 65–80% | Sequential oid; snapshot mostly constant |
| Forward inner     | 12 B         | 2–4 B          | 65–80% | Same                              |
| Range leaf        | 32 B         | 8–14 B         | 55–75% | `attr_id` constant; snapshot mostly constant |
| Tag directory     | 8 B          | 2–4 B          | 50–75% | Sparse tag-id; snapshot constant  |
| Alloc table       | 4 B (bkt_no) | 2 B            | 50%    | Sequential bucket numbers         |
| Subscriptions     | 12 B (sub_id + snap) | 2–4 B  | 65–85% | Sequential ids                    |
| Snapshots btree   | 4 B (snap_id) | 2 B           | 50%    | Sequential snapshot ids           |
| Path / chunk hash | 8 / 32 B     | unchanged      | 0%     | Random-looking hashes — packing skipped |

Random-looking content hashes (chunk index, path-string hashes) bypass packing via
`bit_width = 64` and `base = 0` — they keep the explicit form.

**Footprint impact.** Combined across the metadata zone, packing reduces B+ tree footprint by
~30% and improves cache utilisation proportionally — more keys per cache line means more keys
inspected per memory fetch during binary search.

---

## 2. Superblock and Atomic Root

The superblock is the only structure with a fixed location and is written 3× (offsets 0, 4096, and
last-4096-of-device) for redundancy.

### 2.1 Superblock layout (4 KiB, version 2)

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

    // Two alternating root pointers — atomic commit. RootPointer = 376 bytes (§2.2).
    root_a: RootPointer,                     // [112..488]
    root_b: RootPointer,                     // [488..864]
    active_root: u8,                         // [864..865]   0 = a, 1 = b
    _pad2: [u8; 7],                          // [865..872]

    // Static layout pointers (set at format time, not written again).
    wal_offset: u64,                         // [872..880]
    wal_size: u64,                           // [880..888]
    bucket_size_log2: u8,                    // [888..889]   e.g. 20 = 1 MiB bucket
    copygc_reserve_pct: u8,                  // [889..890]   default 8 (range 5..=21)
    btree_node_size_log2: u8,                // [890..891]   default 18 = 256 KiB (§1.5)
    _pad3: [u8; 5],                          // [891..896]
    bootstrap_buckets: u32,                  // [896..900]   reserved leading buckets (sb + WAL + …)
    _pad4: [u8; 4],                          // [900..904]
    zone_map_offset: u64,                    // [904..912]   0 until any zone is grown

    // Initial-extent zone descriptors. Always authoritative for the first extent;
    // additional extents (if any) are listed in the ZoneMap block.
    index_zone:    ZoneExtent,               // [912..936]   24 bytes
    metadata_zone: ZoneExtent,               // [936..960]
    blob_zone:     ZoneExtent,               // [960..984]

    encryption_keyid: [u8; 16],              // [984..1000]  key identifier (not the key)
    fs_format_version: u32,                  // [1000..1004] §15 — current writing version
    fs_min_on_disk: u32,                     // [1004..1008] §15 — minimum version of any record on disk
    compat_features: u64,                    // [1008..1016] §15.2 — old readers tolerate
    ro_compat_features: u64,                 // [1016..1024] §15.2 — old readers mount RO
    incompat_features: u64,                  // [1024..1032] §15.2 — old readers refuse
    downgrade_log_ref: BlockRef,             // [1032..1048] §15.5 — chain of historical features

    _reserved: [u8; 3044],                   // [1048..4092] zeroed, available for future fields
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
    extent_count: u16,                       // [32..34]   total ZoneExtent records below
    _pad: [u8; 6],                           // [34..40]
    extents: [ZoneMapEntry; 168],            // [40..4072] 168 × 24 B = 4032 B
    _pad_tail: [u8; 20],                     // [4072..4092]
    // trailing CRC32C at [4092..4096]
}

#[repr(C, packed)]
struct ZoneMapEntry {                        // 24 bytes
    zone_kind: u8,                           // 0 = index, 1 = metadata, 2 = blob
    _pad: [u8; 7],
    extent: ZoneExtent,                      // 16 B (offset + length only — flags/pad reused)
}
```

`Superblock.zone_map_offset` is `0` until the first non-contiguous grow; from that point on
it points to the active `ZoneMap` block. Updates use the same A/B alternation as the
superblock root (two adjacent blocks; active selected by `BlockHeader.generation`). When a
zone reaches 168 additional extents, a follow-on `ZoneMap` block is chained via
`BlockHeader.flags` bit 1 (`continuation`).

Mount-time zone resolution: walk superblock's first-extent fields, then if `zone_map_offset
!= 0` append every entry whose `zone_kind` matches. The resulting per-zone vector is
authoritative for that zone's address space.

### 2.2 Atomic root commit

The two-root scheme is the only way to update the filesystem state durably without TOCTTOU
windows:

```rust
#[repr(C, packed)]
struct RootPointer {                         // 376 bytes
    seq: u64,                                //   [0..8]    monotonic; larger seq wins
    lsn: u64,                                //   [8..16]   WAL LSN this root corresponds to

    // Logical / data btrees (snapshot-aware unless noted; see §11.2 table).
    object_table_root:        BlockRef,      //  [16..32]   §5     radix table — current view
    object_history_root:      BlockRef,      //  [32..48]   §11.2  sidecar: (oid, snapshot) overrides
    location_table_root:      BlockRef,      //  [48..64]   §6.1   radix table — current view
    location_history_root:    BlockRef,      //  [64..80]   §11.2  sidecar
    forward_index_root:       BlockRef,      //  [80..96]   §7
    tag_index_root:           BlockRef,      //  [96..112]  §8.1   TagIndexDirectory
    kv_index_root:            BlockRef,      // [112..128]  §9.1   KvDirectory (4 KiB block)
    range_index_root:         BlockRef,      // [128..144]  §9.2
    chunk_index_root:         BlockRef,      // [144..160]  §9.3   content-addressed (snapshot-agnostic)
    value_spill_root:         BlockRef,      // [160..176]  §7.3   content-addressed by value_hash
    backpointer_root:         BlockRef,      // [176..192]  §6.2   physical (snapshot-agnostic)
    ontology_root:            BlockRef,      // [192..208]  §10.1
    path_context_root:        BlockRef,      // [208..224]  §10.3
    subscriptions_root:       BlockRef,      // [224..240]  §10.2
    pool_state_root:          BlockRef,      // [240..256]  §10.4
    snapshot_chain_root:      BlockRef,      // [256..272]  §11.1  snapshots btree

    // Reconcile btrees (§17.2). Zeroed when unused; *_phys variants are
    // populated only when a rotational disk is present in the pool.
    reconcile_work_root:      BlockRef,      // [272..288]
    reconcile_hipri_root:     BlockRef,      // [288..304]
    reconcile_work_phys_root: BlockRef,      // [304..320]
    reconcile_hipri_phys_root:BlockRef,      // [320..336]
    reconcile_pending_root:   BlockRef,      // [336..352]
    reconcile_scan_root:      BlockRef,      // [352..368]  §17.3  in-progress scan cursors

    flags: u32,                              // [368..372]
    crc: u32,                                // [372..376]  CRC32C of bytes [0..372]
}
```

`BlockRef` is `{ disk_id: u16, _pad: u16, block_no: u32, generation: u64 }` — 16 bytes,
self-validating against the destination block's header.

**Commit protocol** (invoked per the cadence in §3.5, *not* per mutation):

1. Quiesce: stop new flushes from advancing past `commit_lsn`. Pending mutations keep going to
   the WAL.
2. Write all dirty btree pages produced by the journal-reclaim thread into open buckets (bucket
   allocation tracked in WAL only — see §12).
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

### 2.3 Block addressing

Within a single device, blocks are addressed by `block_no` (4 KiB unit). Across the pool,
`(disk_id, block_no)` uniquely identifies a block. Devices may grow; `block_no` is never
re-mapped.

Zone-relative indexing (e.g. "object id N is at object_table_offset + N*128") is preserved for the
**ObjectRecord array**, but the array itself lives in COW pages whose physical addresses are
indirected through `object_table_root` (§5).

---

## 3. Write-Ahead Log

The WAL is the **btree-update journal**: a 64 MiB circular log (configurable; must be a multiple
of 4 KiB) on the fastest disk, mirrored to a second disk. Each entry records one key-level
mutation (add tag, write extent, bucket transition, …). Btree nodes on disk are **not** rewritten
per mutation — they are rewritten lazily when journal reclaim or memory pressure demands it
(§3.4). The journal is therefore the **source of truth** for any btree state newer than each
node's `BlockHeader.lsn`.

Per-mutation cost is one ≈ 80-byte journal append. Btree page rewrites are amortised across all
mutations that touched each node since its last flush. A tag mutation that touches four index
trees costs four journal entries; the corresponding page rewrites land later, batched.

Internal layout is segmented to allow parallel truncation and replay.

### 3.1 WAL headers (two 4 KiB blocks at `wal_offset`)

```rust
#[repr(C, packed)]
struct WalHeader {                           // 4096 bytes (one block)
    header: BlockHeader,                     // [0..32]   kind = WalSegment, format_version = 1
    next_lsn: u64,                           // [32..40]
    write_cursor: u64,                       // [40..48]  byte offset within WAL ring
    read_cursor: u64,                        // [48..56]  oldest entry not yet checkpointed
    used_bytes: u64,                         // [56..64]
    last_checkpoint_lsn: u64,                // [64..72]
    last_checkpoint_offset: u64,             // [72..80]  block_no of newest Checkpoint block
    segment_size: u32,                       // [80..84]  typically 1 MiB
    _pad: u32,                               // [84..88]  align to u64
    encryption_keyid: [u8; 16],              // [88..104]
    _reserved: [u8; 3988],                   // [104..4092]
    // trailing CRC32C at [4092..4096] inside BlockHeader's frame
}
```

The header is updated using the same A/B alternation as the superblock root — the WAL header
lives in two adjacent blocks; the active one is the copy with the larger
`BlockHeader.generation` whose CRC validates. (The `BlockHeader.generation` field is the same
monotonic counter that drives every COW block update; no separate `seq` is needed.)

### 3.2 WAL entry

Entries are byte-packed, never crossing a 4 KiB boundary unless `payload_length` > 4060, in which
case the entry is split into `Continuation` frames (flag bit 1 in `BlockHeader.flags`).

```rust
#[repr(C, packed)]
struct WalEntryHeader {                      // 40 bytes
    magic: u32,                              // "WALR"
    op_kind: u8,                             // WalOpKind
    format_version: u8,
    flags: u16,                              // bit 0 = encrypted, bit 1 = compressed payload
    lsn: u64,
    timestamp: HybridTimestamp,              // 16 bytes (see §10)
    payload_length: u32,
    payload_crc: u32,                        // CRC32C of payload (post-compression/encryption)
}
```

Followed by `payload_length` bytes of CBOR-encoded payload (per `WalOpKind`) and a 4-byte trailing
CRC over `WalEntryHeader || payload` for end-to-end framing detection.

CBOR is acceptable here because:
- WAL entries are written once and read once during replay (not random access).
- Op payloads are heterogeneous (`AddTag` vs `WriteBlob` differ wildly in size and shape).
- CBOR's deterministic encoding mode gives stable byte-for-byte serialisation, important for HMAC
  authentication (entries are AES-GCM authenticated; LSN is the nonce).

### 3.3 WAL op payloads (CBOR schemas)

```rust
CreateObject     : { oid: u64, generation: u32, created_ns: i64 }
DeleteObject     : { oid: u64, lsn: u64 }
AddTag           : { oid: u64, tag: u32, origin: u8 }
RemoveTag        : { oid: u64, tag: u32 }
SetAttr          : { oid: u64, key: u32, value: Value }       // Value tagged-union (§4.2)
RemoveAttr       : { oid: u64, key: u32, value_hash: u64 }
AddRelation      : { oid: u64, predicate: u32, target: u64 }
RemoveRelation   : { oid: u64, predicate: u32, target: u64 }
WriteBlob        : { oid: u64, content_hash: [u8;32], extent: ExtentRef, size: u64 }

// Bucket lifecycle (§12)
BucketAlloc      : { disk_id: u16, bucket_no: u32, data_type: u8, generation: u32 }
BucketWrite      : { disk_id: u16, bucket_no: u32, sectors_added: u16 }   // dirty_sectors delta
BucketGenBump    : { disk_id: u16, bucket_no: u32, new_generation: u32 }  // bucket reused
BucketDiscard    : { disk_id: u16, bucket_no: u32 }                       // TRIM issued

// Backpointers (§6.2)
BackpointerInsert: { key: BackpointerKey, value: BackpointerValue }
BackpointerRemove: { key: BackpointerKey }                                // explicit removal
// Implicit removal: a BucketGenBump invalidates all of that bucket's backpointers lazily
// (stale entries detected by gen mismatch on the next scan, no per-bp WAL op needed).

// Snapshot lifecycle (§11)
SnapshotCreate   : { new_id: u32, parent_id: u32, current_replacement: u32, label: Option<String> }
SnapshotDelete   : { id: u32 }                                            // marks for async cleanup
// Note: snapshot deletion does NOT emit per-key WAL ops. The actual
// re-tag / drop happens via a reconcile-driven scan whose progress is
// checkpointed via ReconcileScanStep (§11.5, §17.3). The scan is
// idempotent: a re-tagged leaf no longer matches the deleted snapshot
// id, so resuming from a stale cursor after crash is safe.

// Reconcile (§17)
ReconcileEnqueue : { work: WorkItem, hipri: bool, phys_index: bool }
ReconcileDequeue : { target_kind: u8, owner_key: bytes, work_kind: u8 }   // completion or cancel
ReconcileMove    : { from_loc: BlockRef, to_loc: BlockRef, owner_key: bytes }
                   // atomic location-update for move-path completion
ReconcileScanStep: { scan_id: u64, btree: BtreeKind, cursor_key: bytes }  // resumable progress

// Bset format mutations (§1.5.6)
FormatPromote    : { node_ref: BlockRef, bset_seq: u32, new_format: BsetKeyFormat }

Checkpoint       : { new_root: RootPointer, gc_reserve_buckets: u32 }
```

Replay applies entries strictly in LSN order. Each in-memory mutation is idempotent under
`(lsn ≤ structure.lsn)` shortcutting, so replay is safe across crashes mid-replay.

### 3.4 Journal pins and deferred btree flushes

Each in-memory dirty btree node carries a **journal pin** — the LSN of the oldest WAL entry
whose update has not yet been merged into the on-disk node:

```rust
struct DirtyNode {                           // in-memory only
    block_ref: BlockRef,                     // current on-disk location
    last_persisted_lsn: u64,                 // BlockHeader.lsn of the on-disk version
    pending_lsn_min: u64,                    // oldest WAL entry pinning this node
    pending_lsn_max: u64,                    // newest WAL entry pinning this node
    pending_count: u32,                      // entries waiting to be merged
    in_memory: NodeContent,                  // merged live state (sorted)
}
```

**Journal reclaim invariant.** `WalHeader.read_cursor` (the LSN below which entries may be
overwritten) cannot advance past `min(all_dirty_nodes.pending_lsn_min)`. The reclaim thread
monitors WAL fill level:

| Fill level | Reclaim behaviour                                                           |
| ---------- | --------------------------------------------------------------------------- |
| < 25 %     | Idle. No flushes scheduled.                                                 |
| 25 – 60 %  | Background flushes at low priority — flush nodes with highest pin pressure. |
| 60 – 85 %  | Aggressive: rank dirty nodes by `(pending_lsn_max − pending_lsn_min)` × `pending_count` and flush the top fraction. |
| > 85 %     | New mutations stall until reclaim catches up.                               |

A flush rewrites the affected leaf via the COW path (§5), then each parent up to the root,
updating `BlockHeader.lsn = pending_lsn_max` and clearing the journal pin. The next checkpoint
(§2.2) commits the new root pointers atomically.

**Memory reclaim.** A node may be evicted from the in-memory btree cache only after its pending
journal entries have been merged and the resulting page persisted. Clean nodes (no pending
updates) can be discarded immediately.

**Read path.** Lookups consult the in-memory dirty-node mirror first. Cold reads load the node
from disk, then the engine **replays** journal entries in `[node.last_persisted_lsn,
journal_head]` whose key range intersects the node, producing the merged live view. This replay
is bounded by journal size (~800 K entries worst-case), but the reclaim thresholds keep typical
lag under ~10 K entries per node.

**Write-once-under-read-lock.** Because rewrites are full-node COW (a fresh page at a fresh
location), the flush thread holds only a *shared* lock on the source DirtyNode while it builds
the new page contents. The exclusive lock is taken only at the moment the parent BlockRef is
swapped — milliseconds, regardless of node size. Readers are never blocked on disk I/O.

**Durability semantics.**

- A user fsync triggers a journal flush (an fsync of the WAL ring) — the mutation is durable
  even though its btree page may not yet be persisted.
- Crash recovery reads the active root pointer, then replays all WAL entries with
  `lsn > root.lsn` against the loaded btree.
- `RootPointer.lsn` therefore lags `WalHeader.next_lsn`; the gap is the unmaterialised journal
  tail, bounded by reclaim policy.

### 3.5 Atomic root commit cadence

The §2.2 commit protocol is invoked when:

1. Btree topology actually changes (root split / merge / new tree depth).
2. Sufficient flushes have accumulated that committing a fresh root meaningfully advances
   `read_cursor` (default: every 30 s of mutation activity, or 256 MiB of accumulated flushed
   pages, whichever first).
3. A snapshot is created — snapshot creation forces a checkpoint so the snapshot's
   `RootPointer` materialises a coherent btree state.

Routine mutations do **not** flip the superblock root.

---

## 4. Heterogeneous Value Encoding

`Value` (the contents of attributes) is the only data type whose shape varies enough that fixed
binary layout is wasteful. It is encoded as **CBOR** with a fixed tag scheme:

```
Value ::= 0  Text(string)            // CBOR major-type 3
        | 1  Int(i64)                // CBOR major-type 0/1
        | 2  Float(f64)              // CBOR major-type 7
        | 3  Timestamp(i64)
        | 4  Blob(bytes)             // CBOR major-type 2; large blobs spilled — see below
```

### 4.1 Inlined vs spilled values

- Values whose CBOR encoding is ≤ 96 bytes are stored **inline** in the index leaf (KV or B+
  tree).
- Values larger than 96 bytes are **spilled**: the index stores `BlobRef { disk_id, block_no,
  length }` and the value lives as a `BlobZone` extent. The 96-byte threshold matches the size at
  which a B+ tree leaf can still pack ≥ 16 entries per page.

### 4.2 Value hash for KV index

`value_hash(v) = SipHash-2-4(domain="mimir-kv", type_tag || cbor_encode(v))` truncated to 64 bits.
The type_tag prefix prevents `Int(42)` from colliding with `Text("42")`. SipHash is keyed by a
per-pool secret (stored in superblock-adjacent metadata, not the superblock itself) to defeat hash
flooding.

---

## 5. Object Record Table

Logically a flat array indexed by `ObjectId.local`. Physically, a **COW radix tree** of large
nodes (§1.5) whose depth grows with the populated id space.

### Node capacities

Each node is a 256 KiB region (§1.5) with a 64-byte `BtreeNodeHeader`. For the radix variants
there are no internal bsets — positional updates are journalled via §3.4 and merged on flush.

- **Leaf node** (`BtreeKind::ObjectTable`, level 0): **2044** × `ObjectRecord` (128 B). Layout:
  64 B header + 256 B occupancy bitmap (one bit per slot, ≥ 2044 bits) + 32 B trailer
  (generation, version, reserved) + 2044 × 128 B = **261 984 B used**, leaving 160 B trailing
  pad inside the 256 KiB region. The bitmap distinguishes "never allocated" from "cleared"
  slots (DESIGN §7.3).
- **Inner node** (level ≥ 1): **16 380** × `BlockRef` (16 B) = 262 080 B = exactly 256 KiB −
  64 B header. No bitmap and no trailer: empty child slots are denoted by the
  `BlockRef.generation == 0` sentinel, which never matches a live bucket generation.

### Tree depth and capacity

| Depth (inner levels + leaf) | Max objects                              |
| --------------------------- | ---------------------------------------- |
| 0 inner (leaf only)         | 2 044                                    |
| 1 inner                     | 2 044 × 16 380 ≈ 33 M                    |
| 2 inner                     | 2 044 × 16 380² ≈ 548 G                  |

A 10 M-object pool sits in a **single-inner-level tree** (root inner + leaves; depth 2). The
full 48-bit local-id space is reachable at depth 3 (≈ 9 P objects). The 4-bit `level` field in
`BtreeNodeHeader` supports depths 0–15.

`RootPointer.object_table_root` is a `BlockRef` to the topmost node; the node's
`BtreeNodeHeader.level` identifies whether it is a leaf (very small pool) or an inner node.

### Address translation (oid → leaf slot)

```rust
// MAX_LEVELS bounds the inner-node depth above the leaf.
// BtreeNodeHeader.level is u8 (§1.5.1), so the absolute upper bound is 15;
// in practice depth 3 (MAX_LEVELS = 3) covers the entire 48-bit local-id
// space at 2044 × 16380³ ≈ 9 P objects.
const MAX_LEVELS: usize = 3;

let mut idx = oid_local;
let leaf_slot  = (idx % 2044) as u16; idx /= 2044;
let mut child_path = [0u16; MAX_LEVELS];
for level in 0..root_level {
    child_path[level] = (idx % 16380) as u16;
    idx /= 16380;
}
// idx must be zero now; non-zero means oid_local exceeds the tree's
// addressable range (caller should have grown the tree first — §"Tree growth").
debug_assert_eq!(idx, 0);
```

Object IDs are allocated sequentially per node (DESIGN §2.1), so populated leaves cluster densely
and the tree stays compact. A missing child pointer in any inner node marks an unallocated id
range; cleared slots are sparse and distinguished from "never allocated" by the leaf occupancy
bitmap (§5.1 / DESIGN §7.3).

### Tree growth

When the root is full and a new id falls outside its range, a new inner node one level higher is
allocated, populated with the previous root as its first child, and committed as the new
`object_table_root` at the next checkpoint. Tree growth is log-amortised — never a wholesale
rewrite.

### COW write path

Nodes are rewritten copy-on-write **only when the journal-reclaim thread flushes them** (§3.4),
never per mutation. A flush at depth *D* costs roughly *D* × 256 KiB of writes (each level's
node is rewritten into a fresh region). For the 10 M-object pool that's **2 node rewrites per
flush** (leaf + root inner), amortised across however many mutations have accumulated against
that leaf since its last flush.

Per-mutation cost is still the WAL append (~80 B). Reads consult the in-memory `LoadedNode`
(§1.5.3), which holds the on-disk state plus pending journal entries past
`BtreeNodeHeader.last_persisted_lsn`; cold reads materialise the merged view at load time.

Because positional radix leaves don't use internal bsets, every flush rewrites the whole leaf
into a fresh region. This is acceptable here: a leaf holding 2044 records absorbs hundreds to
thousands of pending mutations before journal-reclaim chooses to flush it, so the per-mutation
amortised write cost is well under 1 KiB. (The B+ tree variants in §7+ avoid even this by
appending bsets — for keyed structures that's cheaper than rebuilding a sorted run.)

### 5.1 ObjectRecord (128 bytes, version 1)

Identical to DESIGN.md §6.2 but with explicit POD layout and a small generation-tracking header:

```
#[repr(C)]
struct ObjectRecord {                        // 128 bytes
    id: u64,                                 //  [0..8]
    generation: u32,                         //  [8..12]
    state: u8,                               //  [12..13]    ObjectState
    flags: u8,                               //  [13..14]    bit 0 = has_overflow, bit 1 = chunked
    record_version: u16,                     //  [14..16]    structural version of THIS record
    content_hash: [u8; 32],                  //  [16..48]    BLAKE3 of plaintext
    blob_offset: u64,                        //  [48..56]
    blob_length: u64,                        //  [56..64]
    created_ns: i64,                         //  [64..72]
    modified_ns: i64,                        //  [72..80]
    tag_count: u16,                          //  [80..82]    total tags on this object (object-wide)
    attr_count: u16,                         //  [82..84]    total attrs on this object
    compression: u8,                         //  [84..85]
    encryption: u8,                          //  [85..86]
    _pad0: u16,                              //  [86..88]
    inline_tags: [u32; 4],                   //  [88..104]   inline tag IDs; valid iff !has_overflow
    overflow_offset: u64,                    // [104..112]   block_no in metadata zone
    stored_size: u64,                        // [112..120]
    last_modify_lsn: u64,                    // [120..128]   for snapshot diffing
}
```

`record_version` in addition to `BlockHeader.format_version` lets a single page mix old and new
records when format changes; the page is rewritten to homogenise on the next checkpoint touching
it.

### 5.2 Overflow records (when tags > 4 or attrs > 0)

For objects with more than 4 tags or any attributes, a separate **OverflowRecord** lives in the
metadata zone, addressed by `overflow_offset`. The `has_overflow` flag (`ObjectRecord.flags`
bit 0) is set; while the flag is set, `inline_tags` is **ignored** and **all** tags + attrs +
relations for the object live in the overflow chain (not split between inline and overflow).
The object-wide totals stay in `ObjectRecord.{tag,attr}_count` (capped at u16 max ≈ 65 K).

```
struct OverflowRecord {
    header: BlockHeader,                     // kind = OverflowRecord (§1.3)
    object_id: u64,
    tag_count: u16,                          // count IN THIS BLOCK only (≤ tag_count of owner)
    attr_count: u16,                         // count IN THIS BLOCK only
    relation_count: u16,                     // count IN THIS BLOCK only
    _pad: u16,
    next_overflow: u64,                      // block_no of the next overflow block, 0 if last
    // Followed by:
    //   tag_count × u32                                      (extra tag IDs)
    //   attr_count × { key: u32, value_hash: u64,
    //                  inline_value: [u8; 96] | spill: BlobRef }
    //   relation_count × { predicate: u32, target: u64 }
    // ... up to ~4020 bytes payload, then trailing CRC.
}
```

If an object outgrows a single 4 KiB overflow record, the chain extends through `next_overflow`
(equivalent to setting `BlockHeader.flags` bit 1 = `continuation` on the head). The widths for
`tag_count` / `attr_count` / `relation_count` here are deliberately u16 — they record only the
**per-block** count, never the object-wide total — and a single 4 KiB block cannot hold
anywhere near 65 K of any of them. The owner's u16 totals therefore never need to be reconciled
across blocks: each block's u16 is a lower bound that the reader sums during traversal.

For pathological cases (≥ 200 tags), switching to a per-object B+ tree is more efficient and
is the planned escape hatch.

---

## 6. Location and Backpointer Tables

Two complementary structures translate between logical objects and physical extents:

- **Location table** (§6.1) — forward mapping `oid → physical extent`. Used on every read.
- **Backpointers** (§6.2) — reverse mapping `(disk_id, bucket_no, sector_offset) → owning key`.
  Used by copygc, device evacuation, scrub, resilver, and cluster reconcile. Without
  backpointers these operations cost O(pool size); with them, O(data-on-affected-bucket).

### 6.1 Location Table (forward mapping)

Same COW radix-tree machinery as the object table (§5), parameterised for 48-byte
`ObjectLocation` records and using the same large-node format (§1.5):

- **Leaf node** (`BtreeKind::LocationTable`, level 0): **5440** × `ObjectLocation` (48 B).
  Layout: 64 B header + 680 B occupancy bitmap (5 440 bits exactly, no wastage) + 32 B trailer
  + 5 440 × 48 B = **261 896 B used**, leaving 248 B trailing pad inside the 256 KiB region.
- **Inner node**: identical to §5's inner — 16 380 × `BlockRef`, empty slots sentineled by
  `BlockRef.generation == 0`.

Capacity:

| Depth | Max objects                              |
| ----- | ---------------------------------------- |
| 0     | 5 440                                    |
| 1     | 5 440 × 16 380 ≈ 89 M                    |
| 2     | 5 440 × 16 380² ≈ 1.46 T                 |

A 10 M-object pool fits in **depth 1** (single inner node + leaves). The 48-bit local id space
is reachable at depth 2.

The radix is keyed by the same `ObjectId.local`, so the location table tracks the object table
slot-for-slot. A flush at depth 1 costs **2 node rewrites** at 10 M scale (leaf + root inner),
amortised across pending mutations.

```rust
#[repr(C, align(8))]
struct ObjectLocation {                      // 48 bytes
    disk_id: u16,                            //  [0..2]
    replica_count: u8,                       //  [2..3]
    flags: u8,                               //  [3..4]    bit 0 = chunked, bit 1 = remote-only
    _pad0: u32,                              //  [4..8]    explicit alignment to u64
    extent_offset: u64,                      //  [8..16]
    extent_length: u64,                      // [16..24]
    replicas: [ReplicaRef; 3],               // [24..48]   3 × 8 bytes
}

#[repr(C)]
struct ReplicaRef {                          // 8 bytes
    disk_id: u16,                            // [0..2]
    sector_offset: u16,                      // [2..4]   4 KiB sector within the bucket
    bucket_no: u32,                          // [4..8]   bucket within the disk
}
```

`ReplicaRef` is **bucket-relative**, mirroring the layout of `BackpointerKey` (§6.2) so that
the move path (§17.5), scrub, resilver, and copygc can share field-level conversions instead
of arithmetic on absolute block numbers. The reachable extent space per disk is
`2^32 buckets × bucket_size`: 4 PiB at the default 1 MiB bucket, 16 PiB at the maximum 4 MiB
bucket — well past current and foreseeable HDD capacity. `sector_offset: u16` admits up to
64 K sectors per bucket, which covers any `bucket_size ≤ 256 MiB` (the format caps bucket
size at 4 MiB / 1024 sectors).

Conversion to/from absolute `block_no` (when interfacing with `BlockRef`):

```
bucket_no     = block_no >> (bucket_size_log2 - 12)
sector_offset = block_no & ((1 << (bucket_size_log2 - 12)) - 1)
```

For chunked objects (`flags & 1`), three of the inline fields are reinterpreted:

- `extent_offset` is the `block_no` of the head `ChunkList` region (a §1.5 positional region;
  see §9.3) instead of a physical extent offset.
- `extent_length` is the **plaintext logical length** of the object (the sum of all chunk
  plaintext lengths). Readers use it to size buffers and to bound chunk iteration; it is
  authoritative for object size and matches `ObjectRecord.blob_length`.
- `replicas[]` describes replicas of the **`ChunkList` region**, not of the data itself —
  the chunks themselves are content-addressed and replicated independently via the chunk
  index (§9.3). `disk_id` likewise identifies the `ChunkList`'s home disk.

This keeps `ObjectLocation` a single fixed-size record regardless of chunked/non-chunked,
preserving the radix-leaf positional layout.

### 6.2 Backpointers (reverse mapping)

A single global B+ tree of large nodes (§1.5), `BtreeKind::Backpointer`, keyed by physical
location:

```rust
#[repr(C, packed)]
struct BackpointerKey {                      // 12 bytes (packed via §1.5.6 to ~3-4 B per leaf)
    disk_id: u16,
    bucket_no: u32,
    sector_offset: u32,                      // 4 KiB units within the bucket
    _pad: u16,
}

#[repr(C, packed)]
struct BackpointerValue {                    // 24 bytes
    owner_kind: u8,                          // OwnerKind discriminator
    _pad: u8,
    length_sectors: u16,                     // extent length in 4 KiB sectors
    bucket_gen: u32,                         // bucket generation at insertion time
    owner_key: [u8; 16],                     // owning key (interpreted per OwnerKind)
}

#[repr(u8)]
enum OwnerKind {
    BlobExtent       = 1,                    // owner_key = ObjectId (u64) + extent_index (u64)
    Chunk            = 2,                    // owner_key = first 16 B of chunk_hash (BLAKE3 prefix)
    BtreeNode        = 3,                    // owner_key = (BtreeKind, level, min_key prefix)
    TagBitmapExtent  = 4,                    // owner_key = TagId (u32) + container_idx (u32)
    OverflowRecord   = 5,                    // owner_key = ObjectId (u64)
}
```

The pair `(BackpointerKey, BackpointerValue)` is 36 bytes unpacked; with §1.5.6 key packing
(`disk_id` constant per leaf, `bucket_no` packs to ~16–20 bits, `sector_offset` packs based on
bucket size), per-key disk cost falls to **~26–28 B**. A 256 KiB leaf packs ~9 000 backpointers
per bset.

#### Properties

- **Bucket-prefix scan.** "What lives in `(disk_id, bucket_no)`?" is a B+ tree range scan over
  `(disk_id, bucket_no, *)`. With key packing the entire bucket's backpointers typically sit in
  one or two contiguous leaf bsets — a single large-node load.
- **Generation gating.** `BackpointerValue.bucket_gen` records the bucket's generation at
  insertion. A backpointer whose `bucket_gen` does not match the current bucket generation is
  **stale** (the bucket has been recycled) and is dropped lazily on the next scrub or copygc
  pass. This means we don't have to atomically delete backpointers when freeing extents — a
  generation bump invalidates all of a bucket's backpointers in O(1).

#### Lifecycle

- **Insert** on every blob/chunk/btree-node write. Journalled as `BackpointerInsert` (§3.3).
- **Remove** on object deletion or extent rewrite. Journalled as `BackpointerRemove`. May be
  elided when the bucket's generation will be bumped (lazy invalidation).
- **Update** on copygc / reconcile move. The move path issues an atomic
  `(BackpointerRemove old, BackpointerInsert new)` pair.

#### Operations enabled

| Operation              | Without backpointers     | With backpointers              |
| ---------------------- | ------------------------ | ------------------------------ |
| Copygc bucket reclaim  | Scan all forward indexes | Range scan one bucket prefix   |
| Disk evacuation        | Scan location table      | Range scan all of disk's buckets |
| Scrub bucket           | Scan all forward indexes | Range scan bucket prefix       |
| Cluster resilver       | Replay full sync log     | Backpointer-driven replay of affected buckets only |
| Stale-pointer cleanup  | Track-during-write       | Lazy via generation comparison |

#### Footprint

Per 10 M objects with average 1 extent each: 10 M backpointers × ~28 B packed ≈ **280 MiB**.
Plus a few thousand btree-node backpointers (~100 KiB) and ~5 000 tag-bitmap backpointers
(~140 KiB). Negligible compared to existing per-object metadata.

---

## 7. Forward Index

The forward index maps `oid → [ForwardEntry]` and must support fast per-object listing and
per-object diffing for sync.

On disk it is a **B+ tree of large nodes** (§1.5), keyed by `oid`. Each node uses the standard
multi-bset envelope: new mutations are appended as a fresh bset; lookups merge-search across
all bsets in the node.

### 7.1 Node layout

Both inner and leaf bsets use the §1.5.6 packed-key encoding. Within a single leaf, all `oid`
keys share the leaf's key range (typically a span of 10⁴ – 10⁵ contiguous ids), so the format
descriptor's `bit_width` settles around 16–20 bits — encoding `oid` as 2–3 bytes versus the
unpacked 8 bytes.

- **Inner node** (`BtreeKind::Forward`, level ≥ 1): one or more bsets of `(packed_oid_key,
  child: BlockRef)` pairs. With 2–3 byte packed keys + 16 B BlockRef = ~18–19 B per entry; one
  full bset packs ~14 500 children. Tree depth at 10 M objects: **2 levels** (1 inner + leaves).
- **Leaf node** (level 0): bsets of `LeafEntry` records:

```rust
// Logical (unpacked) shape; on-disk uses the §1.5.6 packed encoding.
struct LeafEntry {                           // variable length on disk
    oid: u64,                                // sort key — packed via BsetKeyFormat
    header: u16,                             // bitfield: see below
    body: union {                            // discriminated by header.is_spill
        inline: [PackedAssertion; header & 0x7FFF],   // when is_spill = 0
        spill_ref: BlockRef,                          // when is_spill = 1; chain via the
                                                      //  spill region's trailing BlockRef
                                                      //  slot per §7.2
    },
}
```

The 16-bit `header` is a single bitfield carrying both the spill flag and the assertion
count:

```
header: u16
  bit 15       is_spill   1 = body is `spill_ref` (BlockRef into a ForwardOverflow chain)
                          0 = body is the inline `[PackedAssertion; total]` array
  bits 0..14   total      object's total assertion count, regardless of is_spill:
                            is_spill = 0 → total inline assertions (typical 0..8,
                                           hard cap ~16 before next-mutation spill)
                            is_spill = 1 → total assertions across the entire spill
                                           chain (up to 32 K)
```

**Total cardinality** is therefore just `header & 0x7FFF` — a single masked read, no
branching on `is_spill`. The query optimiser uses this for intersection planning (§9), where
the smaller-cardinality side is iterated first. The 15-bit ceiling (32 K) is comfortably
above the >200-tag pathological case mentioned in §5.2; objects exceeding 32 K assertions
take the per-object B+ tree escape hatch.

The merge — a single `header` field rather than separate `count` and `spill` — saves 2 bytes
per `LeafEntry` and removes the "either count or spill_count is zero" implicit invariant of
the previous two-field form.

```rust
struct PackedAssertion {                     // 16 bytes (not key-packed; values stay byte-aligned)
    kind: u8,                                // 0=Tag, 1=Attr, 2=Relation
    origin: u8,                              // 0=Direct, 1=Materialized
    _pad: u16,
    a: u32,                                  // tag id (Tag/Attr) or predicate (Relation)
    b: u64,                                  // value_hash (Attr), target oid (Relation), 0 (Tag)
}
```

The **key** (`oid`) is packed; the **value** (`header`, body) stays byte-aligned. Per-leaf
format descriptor records `oid_base = leaf.min_oid` and `oid_bits = ⌈log₂(leaf.max_oid −
leaf.min_oid + 1)⌉`.

A leaf with 8 assertions per object (typical) packs ~2 740 entries per bset (per-entry
≈ 2.5 B packed key + 2 B header + 128 B inline body = ~133 B); with 4 active bsets the leaf
carries up to ~10 000 entries before full compaction.

### 7.2 Spill

Objects with more than 8 assertions store a `BlockRef` to a `ForwardOverflow` region (also a
256 KiB large-node region; positional, no bsets — single rewrite on growth). Each overflow region
holds up to 16 380 × `PackedAssertion`. Further overflow chains via the trailing `BlockRef` slot.

### 7.3 Bset behaviour

- **Append on flush.** When journal-reclaim flushes a forward-index leaf, only the new bset is
  written — typically a few KB to a few tens of KB, not the whole 256 KiB node.
- **In-memory merge.** `LoadedNode.merged_view` builds a `BTreeMap<u64, SmallVec<[PackedAssertion;
  8]>>` lazily on first lookup; subsequent lookups are direct hits.
- **Full compaction** triggers when `payload_used > 75%` or `bset_count > 4`, rewriting the node
  into a fresh region with a single merged bset.

For attributes whose actual `Value` matters (not just its hash), the `value_hash` indirects into
the **value spill table** (a separate B+ tree keyed by `value_hash → CBOR(Value)`), shared with
the KV index (§8). Tag and Relation entries are self-contained.

### 7.4 Properties

- O(log N) lookup by oid (depth 2 at 10 M scale ⇒ ≤ 2 large-node loads).
- O(1) per-assertion diffing via `BsetHeader.journal_seq` — sync streams bsets newer than
  the peer's watermark, exactly the journal-streaming fast path of §11.2.
- Compact in-memory mirror: a `HashMap<u64, SmallVec<[PackedAssertion; 8]>>` over the loaded
  node's `merged_view`.

---

## 8. Tag Inverted Index

The tag index is the heart of query performance. Its on-disk form must:

- Look up a tag's bitmap quickly.
- Allow per-tag COW updates without rewriting unrelated tags.
- Support delta-sync: cheap diff between two snapshots of the same bitmap.

### 8.1 TagIndexDirectory

A **B+ tree of large nodes** (§1.5) keyed by `TagId: u32`. Leaf entries are 40 B, with fields
reordered so every multi-byte field sits at its natural alignment (8-byte struct alignment;
no `#[repr(packed)]` — the entry is read repeatedly during query bitmap algebra and unaligned
loads on `last_modify_lsn` / `store_root` would be a measurable overhead):

```
#[repr(C)]
TagIndexLeafEntry {                          // 40 bytes
    last_modify_lsn: u64,                    //  8  @  0   (8-aligned)
    store_root:      BlockRef,               // 16  @  8   §8.2 / §8.3 — contains a u64 generation
    tag_id:          u32,                    //  4  @ 24   sort key
    cardinality:     u32,                    //  4  @ 28   for fast query-planner stats
    generation:      u32,                    //  4  @ 32   bumped on bitmap rewrite
    store_kind:      u8,                     //  1  @ 36   Simple / Ordered / Ranked
    _pad:            [u8; 3],                //  3  @ 37
                                             // 40 total
}
```

The hot pair `(tag_id, store_root)` lands on the same 32-byte half-cache-line: a query that
loads a leaf entry to dispatch a bitmap fetch gets `tag_id`, `cardinality`, `generation`, and
`store_root` all in a single 64-byte fetch.

On-disk, leaf bsets use the §1.5.6 packed-key encoding: `tag_id` typically packs to 2–3 bytes
per leaf (sparse but clustered ids); the value's 32 bytes after `tag_id` stay byte-aligned.
Per-key on-disk cost is ~34 B. A 256 KiB leaf bset (262 144 B − 64 B node header − 32 B bset
header − 44 B key-format descriptor = 262 004 B payload) holds **~7 700 entries**; the full
ontology of 5 000 tags fits in **a single leaf** (depth 0). For pools with hundreds of
thousands of tags the tree extends to depth 1 (~125 M-tag capacity).

`(cardinality, last_modify_lsn, generation)` enables fast snapshot diffing without dereferencing
the bitmap — the directory's bset stream alone tells a peer which tags changed and how.

### 8.2 Roaring bitmaps on disk

Roaring bitmaps stay on the **4 KiB block format** (not 256 KiB nodes) because:

- The portable Roaring spec is already container-addressable and mmap-friendly at any offset.
- Per-tag bitmaps vary from a few hundred bytes to many megabytes; the 4 KiB granularity matches
  Roaring's natural 4 KiB-class bitmap container.
- Bitmaps are referenced as opaque blobs from the directory; their internal format is a stable
  external standard (Apache Lucene + the `roaring` crate).

We adopt the **Roaring portable serialization spec** framed inside `BlockHeader`-prefixed pages:

```
TagBitmap (one or more 4 KiB blocks):
  BlockHeader { kind = TagBitmapPage, format_version = 1 }
  PortableRoaringHeader (cookie, container count, ...)
  Container directory (4 bytes per container: key + cardinality)
  Container offsets (4 bytes each, since v1.4 of portable spec)
  Containers (array, run-length, or bitmap, 4 KiB-aligned each)
  trailing CRC32C
```

Containers larger than ~64 KiB span multiple consecutive blocks; the directory records the
absolute block_no of each container so they can be loaded individually. For small bitmaps that
fit in one block, the directory and containers are co-located.

### 8.3 Ordered & ranked stores

```
OrderedStore root block:
  members: BlockRef → TagBitmap
  sequence: BlockRef → SequencePages (4 KiB each, 512 ObjectIds per page)
  sequence_count: u64

RankedStore root block:
  members: BlockRef → TagBitmap
  ranked: BlockRef → RankedPages   { (oid: u64, score: f32, _pad: u32) per entry, 256 per page }
  ranked_count: u64
```

The sequence pages form a logical array; updates use a packed log + periodic compaction (small
appends are appended to a tail page, occasional rewrites compact, both COW and reflected in the
checkpoint).

---

## 9. KV Equality Index, Range Index, Chunk Index

### 9.1 KV Equality Index

Hash-based on disk via **extendible hashing** keyed by `(tag_id, value_hash)`. The KV index is
the one structure that does **not** use the §1.5 large-node B+ tree format — point-equality
lookups benefit more from hash-bucket addressing.

```
KvDirectory (single 4 KiB block, addressed by RootPointer.kv_index_root):
   header (BlockHeader, kind = KvHashBucket with directory flag in BlockHeader.flags)  // 32 B
   global_depth: u8                                                                    //  1 B
   _pad0: [u8; 3]                                                                      //  3 B
   bucket_count: u32                  // = 1 << global_depth                           //  4 B
   entries: [BlockRef; 252]           // local-depth tagged buckets                    // 4032 B
   spillover_root: BlockRef           // 0 unless global_depth ≥ 8 (see below)         //  16 B
   _pad_tail: [u8; 4]                                                                  //  4 B
   trailing CRC32C                                                                     //  4 B
                                                                                        // = 4096 B

KvBucket (4 KiB):
   header (BlockHeader, kind = KvHashBucket)                                           // 32 B
   local_depth: u8                                                                     //  1 B
   entry_count: u16                                                                    //  2 B
   _pad: u8                                                                            //  1 B
   entries: [{ tag_id: u32, value_hash: u64, bitmap_ref: BlockRef }; 144]              // 4032 B
   _pad_tail: [u8; 24]                                                                 // 24 B
   trailing CRC32C                                                                     //  4 B
                                                                                        // = 4096 B
```

The inline directory holds 252 entries — sufficient for `global_depth ≤ 7` (i.e. up to 128
hash buckets). At `global_depth ≥ 8`, `spillover_root` points at a §1.5 large-node positional
region (`BtreeKind::KvDirectory`, level 0) holding the full `2^global_depth`-sized BlockRef
array; the inline `entries` array is then ignored. Each 256 KiB spillover region holds 16 380
entries, supporting `global_depth` up to 13 (8 192 buckets) before chaining to a deeper region.

The bitmap referenced by each entry is a `TagBitmap` (§8.2), reused via the same machinery.

The accompanying **value spill table** uses the standard §1.5 large-node B+ tree:
`value_hash → CBOR(Value)`, so the actual value can be reconstructed when needed (display,
faceted enumeration, range comparisons).

### 9.2 Range Index

A **B+ tree of large nodes** (§1.5) keyed by `(attr_id: u32, value: NormalisedKey, oid: u64)` —
28 B unpacked. With per-bset key packing (§1.5.6) this is the structure that benefits most:

- `attr_id` is almost always **constant** within a leaf (a leaf covers one or two adjacent
  attributes) → 0 bits per key.
- `value` (16 B `NormalisedKey`) gets a per-bset base + bit-width; for numeric attributes the
  span within a leaf typically fits in 24–40 bits.
- `oid` packs identically to forward-index keys: 16–20 bits.

Net per-key size: typically **8–12 B** packed. Each 256 KiB leaf packs ~25 000 entries per bset.

Leaf bsets store key → `BlockRef` to a roaring bitmap.

`NormalisedKey` is a fixed-size order-preserving encoding:

| ValueType | Encoding (16 bytes)                                                                       |
| --------- | ----------------------------------------------------------------------------------------- |
| Int/Time  | `i64` flipped sign bit (so unsigned compare = signed compare), zero-padded.               |
| Float     | IEEE 754 with sign-bit-flipped trick: positives flip top bit, negatives flip all bits.    |
| Text      | First 14 bytes of UTF-8, length byte, continuation flag; long strings spill via `value_hash`. |
| Blob      | Stored only by hash; not range-indexed.                                                   |

Leaf values are roaring bitmaps (per `(attr_id, value_prefix)`), enabling cheap range scans.

### 9.3 Chunk Index (for FastCDC objects)

A **B+ tree of large nodes** (§1.5) keyed by `chunk_hash: [u8; 32]`:

```
ChunkIndexLeafEntry {                        // 56 bytes
    chunk_hash: [u8; 32],
    ref_count: u32,
    length: u32,
    blob: BlobRef,                           // 16 B
}
```

A 256 KiB leaf packs ~4 600 chunk entries per bset.

`ChunkList` per object: array of `(chunk_hash, length)` referenced from `ObjectLocation` when
chunked (a positional `ChunkList` region; see §1.5 for the radix layout).

Chunk hashes use BLAKE3 of plaintext (pre-compression) so dedup is content-defined.

---

## 10. Subscriptions, Path Contexts, Ontology

These structures change less frequently and are loaded fully into memory at mount, but persisted
on every checkpoint.

### 10.1 Ontology persistence

The ontology is a graph (tags + implications + tag relations). On disk:

```
OntologyRoot (4 KiB):
  header
  module_count: u32
  tag_count: u32
  implication_count: u32
  modules_root: BlockRef     → §1.5 B+ tree, key = module_id_hash → CBOR(ModuleManifest)
  tags_root:    BlockRef     → §1.5 B+ tree, key = TagId          → TagDefRecord (fixed, 64 bytes)
  tag_names:    BlockRef     → §1.5 B+ tree, key = name_hash      → (TagId, BlockRef → CBOR(TagDef))
  dag_root:     BlockRef     → ImplicationDagPages (sparse adjacency lists, §1.5 large nodes)
```

`TagDefRecord` is 64 bytes with `name_offset` pointing into `tag_names`. The variable-shape parts
(`TagSemantics::OrderedCollection { element_constraint }`, future fields) live in CBOR via
`tag_names`. Hot path queries only touch the fixed records.

Modules ship as TOML, but their **on-disk** form is CBOR — the parser converts TOML → struct →
CBOR at install time. TOML is never seen by the read path.

### 10.2 Subscriptions

```
SubscriptionsRoot:
  §1.5 B+ tree, key = SubscriptionId u64 → SubscriptionRecord (variable, CBOR)
```

A subscription's `cached_result` is a roaring bitmap stored in a `TagBitmap` (§8.2) referenced
from the record. Cursor (LSN), state, retention, debounce config, and the `Query` AST are all
inside the CBOR record — query trees are heterogeneous and infrequently rewritten, so CBOR
overhead is negligible.

### 10.3 Path contexts

```
PathContextRoot:
  §1.5 B+ tree, key = name_hash → PathContextHeader (48 bytes)
  Manifest is a §1.5 B+ tree keyed by path-string-hash → ProjectedEntry (96 bytes inline +
  spill for Symlink targets and long paths).
```

```rust
#[repr(C)]
struct PathContextHeader {                   // 48 bytes
    name_offset: u32,                        //  [0..4]   into the per-context string heap
    name_len: u16,                           //  [4..6]
    flags: u16,                              //  [6..8]   bit 0 = read-only, bit 1 = ephemeral
    manifest_root: BlockRef,                 //  [8..24]  root of the §1.5 manifest tree
    entry_count: u64,                        // [24..32] total ProjectedEntries (manifest size hint)
    last_refresh_ns: i64,                    // [32..40] last full re-projection timestamp
    last_modify_lsn: u64,                    // [40..48] for snapshot diffing
}
```

`entry_count`, `last_refresh_ns`, and `last_modify_lsn` are the per-context "stats" — they
let `mimir context list` answer size and freshness questions without dereferencing
`manifest_root`.

Per-object reverse mappings (which object → which paths in which contexts) live in the forward
index as a special assertion kind, so listing all paths of an object is one forward-index hit.

### 10.4 Pool state

```
PoolStateRoot (4 KiB):
  disk_count: u32
  cluster_node_count: u32
  disks: [DiskDescriptorOnDisk; 56]    // fixed, 64 bytes each
  placement_rules_root: BlockRef       → B+ tree of CBOR rules (heterogeneous)
  cluster_peers_root:   BlockRef       → B+ tree key = NodeId → PeerRecord
```

`DiskDescriptorOnDisk` is fixed **64 bytes**:

```rust
#[repr(C, packed)]
struct DiskDescriptorOnDisk {                // 64 bytes
    disk_id: u16,                            //  [0..2]
    media_type: u8,                          //  [2..3]
    tier: u8,                                //  [3..4]
    state: u8,                               //  [4..5]    DiskState
    _pad: u8,                                //  [5..6]
    path_offset: u16,                        //  [6..8]    into string heap
    capacity_bytes: u64,                     //  [8..16]
    used_bytes: u64,                         // [16..24]
    bucket_count: u32,                       // [24..28]   capacity_bytes >> bucket_size_log2
    first_usable_bucket: u32,                // [28..32]   = bootstrap_buckets
    buckets_root: BlockRef,                  // [32..48]   §12.2 bucket alloc table root
    freespace_root: BlockRef,                // [48..64]   §12.4 freespace LRU root
}
```

The per-disk `buckets_root` and `freespace_root` are COW under the standard checkpoint
machinery — every RootPointer commit captures a self-consistent snapshot of every disk's
allocation state.

---

## 11. Snapshots and Cluster Sync

Snapshots are **key-level**: a snapshot is a 32-bit ID embedded in the position of every key in a
snapshot-aware btree. Multiple versions of the "same" logical key coexist in one btree,
distinguished by their `snapshot` field. Visibility is determined by walking the snapshot tree
(§11.1).

Creation is **O(1)** regardless of pool size — no keys are copied, no checkpoint is forced. Many
thousands or millions of snapshots can exist simultaneously; their cost is the disk space
consumed by keys unique to each snapshot.

### 11.1 Snapshot tree

A `SnapshotId` is a `u32` (4 G snapshots before recycling concerns). Snapshot relationships are
stored in the **snapshots btree** (`BtreeKind::Snapshots`, a §1.5 B+ tree keyed by `SnapshotId`):

```rust
#[repr(C, packed)]
struct SnapshotNode {                        // 64 bytes
    id: u32,                                 // self-id (also the btree key)
    parent: u32,                             // 0 = root snapshot
    first_child: u32,                        // first child id (0 = leaf)
    next_sibling: u32,                       // next sibling under same parent (0 = last)
    depth: u16,                              // distance from root
    flags: u8,                               // bit 0 = leaf (subvolume-bearing),
                                             // bit 1 = deleted (§11.5)
    _pad: u8,
    ancestor_bitmap: u128,                   // bits[i] = "id − i is an ancestor", i ∈ 0..128
    skiplist: [u32; 3],                      // randomised ancestor IDs for O(log n) deep checks
    created_ns: i64,
    label_offset: u32,                       // into a string heap; 0 = unlabelled
    _reserved: u32,
}
```

**Children topology.** The pair `(first_child, next_sibling)` encodes a **first-child /
next-sibling** linked list, supporting an arbitrary number of children per node at fixed
per-node cost. Walking a node's children is `cur = node.first_child; while cur != 0 { yield
cur; cur = tree[cur].next_sibling; }`. This admits the rollback case (§11.9: a snapshot may
spawn N≥3 sibling branches over its lifetime) without changing node size — same 8 bytes as
the previous `[u32; 2]` form.

**Ancestry check.** The 128-bit `ancestor_bitmap` answers `is X an ancestor of Y?` in O(1) when
`Y.id − X.id ≤ 128` — which covers the vast majority of cases (most snapshots reference recent
ancestors). For older queries, the 3-entry randomised skiplist provides O(log n) traversal to
the root. During early recovery, before this data is validated, queries fall back to a simple
parent-pointer walk.

`RootPointer.snapshot_chain_root` is the snapshots btree root.

### 11.2 Snapshot-aware bkey position

In snapshot-aware btrees, every bkey position carries an extra `snapshot: u32` field appended
to the kind-specific key fields. With §1.5.6 packing, `snapshot` is typically a 0-bit field in a
leaf bset (one snapshot dominates the bset's keys), or a few bits at most — its overhead is in
the noise.

| Btree              | Snapshot-aware? | Notes                                         |
| ------------------ | --------------- | --------------------------------------------- |
| `Forward`          | yes             | per-object assertions diverge across snapshots |
| `Range`            | yes             | per-object attributes diverge                  |
| `KvIndex`          | yes             | same                                           |
| `TagDirectory`     | yes             | tags exist or don't per snapshot               |
| `ChunkIndex`       | no              | content-addressed; refcount handles divergence |
| `Backpointer`      | no              | physical state, not logical                    |
| `BucketAlloc`, `FreespaceLru` | no | physical state                              |
| `Ontology`         | yes             | snapshot freezes the ontology version          |
| `PathContext`      | yes             | path projections diverge                       |
| `Subscriptions`    | yes             | per-snapshot watch state                       |
| `ValueSpill`       | no              | content-addressed by `value_hash`              |

For the **positional radix tables** (`ObjectTable` §5, `LocationTable` §6.1), snapshot-versioning
uses a sidecar btree `(BtreeKind::ObjectHistory)`: keyed by `(oid, snapshot)` with values that
shadow the radix entry. The radix always holds the **current** view; reads in a non-current
snapshot consult the sidecar first, falling through to the radix only if no shadowing record
applies. This keeps the hot path (current-snapshot reads) at single-radix-lookup cost while
preserving the snapshot model for older views.

For **roaring tag bitmaps** (§8.2), each tag's `store_root` resolves through the snapshot tree:
the `TagDirectory` is snapshot-aware (the directory itself has snapshot-tagged keys), so each
snapshot has its own pointer to a (possibly shared) bitmap. New writes that diverge a tag bitmap
allocate a fresh `TagBitmap` region and update the directory at the writing snapshot's ID.

### 11.3 Visibility rules (snapshot iteration)

In every snapshot-aware btree, each leaf entry has a 1-byte **value-type discriminator** as its
first byte (the `kind` field of `LeafEntry`/`PackedAssertion`/etc.; for the radix-table sidecar
btrees `ObjectHistory`/`LocationHistory` the discriminator precedes the shadowed record).
The reserved discriminator value `0xFF` is `KEY_TYPE_whiteout`: a tombstone marking that the
key is **explicitly deleted** at that snapshot. Whiteouts carry no payload — the entry's
length stops after the discriminator.

When reading at snapshot `S`, the iterator walks the btree in order. For keys with the same
non-snapshot prefix, it picks the one with the highest `snapshot ≤ S` that is an ancestor of
`S`. A whiteout encountered as the chosen ancestor returns "not visible" — it blocks fall-through
to older versions for descendants of that snapshot. Whiteouts are physically reclaimed during
the next full compaction (§1.5.4) of any leaf where every ancestor of the whiteout's snapshot
is itself in the same leaf and either deleted or whited-out.

Pseudocode:

```
fn visible_at(key: &Bkey, snapshot: SnapshotId) -> bool {
    // key.snapshot must be S itself or an ancestor of S
    snapshot_tree.is_ancestor(key.snapshot, snapshot)
}

fn lookup(prefix: &KeyPrefix, snapshot: SnapshotId) -> Option<Value> {
    let mut best: Option<&Bkey> = None;
    for k in btree.range(prefix..) {
        if k.prefix() != prefix { break; }
        if !visible_at(k, snapshot) { continue; }
        if best.is_none() || k.snapshot > best.unwrap().snapshot {
            best = Some(k);
        }
    }
    best.and_then(|k| match k.value {
        KEY_TYPE_whiteout => None,                      // explicitly deleted in snapshot
        v                 => Some(v),
    })
}
```

In practice, this is implemented as a single forward iteration — keys with the same prefix
cluster together in the btree leaf, and the iterator scans them in `(prefix, snapshot)` order,
choosing the closest ancestor as it goes.

### 11.4 Snapshot creation

Snapshot creation is O(1): allocate two new `SnapshotId`s as children of the current snapshot's
node — one becomes the new snapshot's ID; the other replaces the current view's ID. No keys are
copied. Both children inherit visibility of all ancestor keys through the tree. The new pair
is **prepended** to N's child sibling list (each new child becomes the head; previous head
becomes its `next_sibling`).

```
Before snapshot:                After snapshot:

       N (current)                       N
                                         │
                                  first_child = N₁
                                         │ next_sibling
                                         ▼
                                         N₂   (N₁ = new "current", N₂ = the snapshot)
```

Subsequent writes to the current view are tagged with `N₁`; reads against the snapshot use `N₂`.
Divergence happens only where modifications occur. If N already has children from prior
snapshots / rollbacks, those nodes follow N₂ in the sibling chain (`N₂.next_sibling`) — the
total ordering reflects creation recency, head-first.

A new `BackpointerInsert` is **not** issued for shared extents — the underlying blob is unchanged
and its existing backpointer is valid for both snapshots. Backpointers are physical, not logical
(§6.2 / §11.2 table), so they're snapshot-agnostic.

### 11.5 Snapshot deletion

Deleting a snapshot is **two operations**: a small synchronous step that takes the snapshot
out of visibility, and a long-running background scan that physically reclaims the keys.
The synchronous step's WAL cost is one entry; the scan's WAL cost is a handful of cursor
checkpoints, regardless of how many keys are involved.

**Synchronous step (`SnapshotDelete` WAL op).**
1. In the snapshots btree, set `SnapshotNode.flags` bit 1 (`deleted`). The node remains in
   place — its `depth` and `skiplist` are still consulted during ancestry checks for siblings
   and descendants — but no new reads are accepted *at* the deleted snapshot id.
2. Enqueue a `WorkKind::SnapshotCleanup` work item per snapshot-aware btree (§17.2). Each
   item carries `(deleted_id, surviving_descendant_id, btree_kind)`. The surviving descendant
   is determined by the tree shape: a leaf snapshot has none (keys are dropped or whited
   out); an interior snapshot collapses to whichever child remains live.
3. Checkpoint. The deletion is durable after this; subsequent crashes resume the cleanup
   from the work-item queue.

**Background scan (reconcile-driven, no per-key WAL).** For each `SnapshotCleanup` item:

```
let mut cursor = scan_state.cursor;            // resumed from ReconcileScan record (§17.3)
for leaf in btree.leaves_from(cursor) {
    for entry in leaf.entries_with_snapshot(deleted_id) {
        match classify(entry, child_set, sibling_set) {
            Drop          => leaf.remove(entry),
            Whiteout      => leaf.replace_value(entry, KEY_TYPE_whiteout),
            Retag(target) => leaf.set_snapshot(entry, target),
        }
    }
    if leaf.is_dirty() { dirty_node_set.add(leaf); }
    cursor = leaf.next_key();
    if elapsed_since_checkpoint() > scan_step_interval {
        emit ReconcileScanStep { scan_id, btree, cursor_key: cursor };
    }
}
```

The mutations above are ordinary in-memory btree changes that hit the §3.4 dirty-node path —
flushed lazily, not journalled per key. `ReconcileScanStep` entries fire at most every
~30 s of scan progress (configurable), bounding WAL cost to O(scan duration), not O(keys).

**Idempotency.** The scan classification is a function of the entry's snapshot id and the
snapshot tree's current shape — both stable after `SnapshotDelete` is journalled. A leaf
that has already been re-tagged no longer contains entries at `deleted_id`, so resuming the
scan from a stale cursor after a crash is a no-op for already-processed leaves. This
removes the need for any per-key undo or redo log.

**Completion.** When the scan reaches the end of every snapshot-aware btree, a final
`SnapshotNode` removal is performed: the deleted node is unlinked from its parent's sibling
list (parent's `first_child` is advanced past it, or its predecessor's `next_sibling` is
spliced over it), and the `depth`/`skiplist` fields of descendants are recomputed in a single
batched `SnapshotTreeReorg` pass (deferred to the next checkpoint quiesce — see §3.5 — to
avoid racing with live ancestry queries).

**Constraints.**
- A snapshot with more than one non-deleted child cannot collapse during cleanup; all but one
  child must itself be deleted (or itself be the `current_replacement` for a deeper snapshot)
  first. This is checked at `SnapshotDelete` time and the request is rejected if not
  satisfied. (Walking a node's children to count them is O(children_count), which is bounded
  by the per-node fan-out in practice — typically 2, occasionally 3-5 after rollbacks.)
- Multiple deletions can run concurrently — each `SnapshotCleanup` work item has its own
  cursor and operates independently (the per-leaf rewrites compose because each touches
  disjoint snapshot-id sets).

### 11.6 Bucket retention under key-level snapshots

A bucket is reclaimable when no live snapshot references its contents. Because backpointers
record `bucket_gen` at insertion (§6.2), and snapshots only delay extent-deletion (they don't
prevent generation bumps once *all* snapshots have moved on), retention is enforced
**at the extent level**, not at the bucket level:

- An extent's owning key carries a `snapshot` ID; the extent is logically alive as long as any
  ancestor of any live snapshot can see that key.
- Copy GC (§12.6) treats an extent as live if any backpointer's `bucket_gen` matches the bucket
  *and* its owner's snapshot is still reachable through the snapshot tree.
- When the deleting snapshot pass (§11.5) removes a key, the corresponding backpointer is
  removed too. Once a bucket has no live backpointers, its generation can be bumped.

Snapshot retention scales with **logical changes**, not bucket counts.

### 11.7 Cluster diff between snapshots

With key-level snapshots, the cluster-sync diff between two snapshots `A` and `B` becomes a
**snapshot-set difference**: stream every key whose `snapshot` is in `ancestors(B) \ ancestors(A)`.

```
let from_set = ancestors(A);            // 128-bit bitmap + skiplist walk; small
let to_set   = ancestors(B);
let new_only = to_set - from_set;       // snapshot IDs unique to B's lineage

for tree in snapshot_aware_btrees {
    for key in tree.iter() {
        if new_only.contains(key.snapshot) {
            emit(SyncOp::from(key));
        }
    }
}
```

The diff is a single btree range scan filtered by the small `new_only` set. The same operation
serves recent and old snapshots — no special case for either.

The result is a `SyncBundle`:

```
SyncBundle (CBOR):
   from_snapshot: u32
   to_snapshot: u32
   from_node: NodeId
   ops: [SyncOp]                       // canonical, HLC-ordered
   ontology_delta: Option<OntologyDelta>
   bitmap_deltas: [{ tag_id, snapshot, added/removed }]   // for tag bitmap divergences
```

The bundle is signed and shipped to peers (per §9.4 key hierarchy in DESIGN.md).

**Resilver path.** When a peer comes back from a degraded state with one disk missing, recovery
is driven by the reconcile subsystem (§17). The disk-state change triggers a scan that
backpointer-walks (§6.2) the affected disk and enqueues `ReplicaRepair` work items at high
priority. Backpointers are snapshot-agnostic, so the resilver re-fetches extents irrespective
of which snapshot owns them.

### 11.8 Retention policy

Snapshot space cost is per-key. Per-snapshot overhead in steady state:

- Keys unique to each snapshot: typically a few KB for an "idle" snapshot, scaling with logical
  changes since the parent.
- Snapshot tree node: 64 B + ancestor bitmap maintenance.

**Defaults:**

- Auto-created sync snapshots: kept for `min(grace_period, 7 days)`. Tens of thousands of these
  are cheap because divergence is small.
- Labelled / pinned snapshots: kept until explicitly removed. Cost = sum of unique keys.

### 11.9 Rollback

```
brunnr rollback --to <snapshot_label>
```

1. Resolve the label to a `SnapshotId` `S`.
2. Allocate a fresh `SnapshotId` `S'` and link it as a new child of `S`: prepend it to S's
   sibling list (`S'.next_sibling = S.first_child; S.first_child = S'`). The first-child /
   next-sibling topology (§11.1) admits any number of children, so this works regardless of
   how many sibling branches `S` has accumulated from prior snapshots and rollbacks.
3. Atomically retag the pool's "current" pointer to `S'`. The view now reflects `S`'s state;
   subsequent writes are tagged with `S'` and diverge from `S` at this point. The previously
   current branch remains intact as another sibling of `S'` until explicitly deleted.

No data is copied. The rollback is O(1); subsequent reads pay the snapshot-iteration cost
(§11.3) until the abandoned branch is deleted via §11.5.

---

## 12. Allocation Layer

Allocation is **bucket-based** with **generation numbers**, modelled on bcachefs. Each disk is
divided into fixed-size buckets (typically 1 MiB; configurable 256 KiB – 4 MiB at format time).
Within a bucket, writes are **append-only**: once opened, sectors are written sequentially and
never overwritten. A bucket is reused only after its generation counter has been incremented —
which atomically invalidates every pointer that referenced its previous contents.

This buys four properties simultaneously:

- **Constant-time bucket invalidation.** Bumping a bucket's generation invalidates every pointer
  that referenced the old contents — no scan required.
- **Crash-safe pointer staleness detection.** Every `BlockRef` carries the bucket's expected
  generation; mismatches are silently dropped on read.
- **Native fit for SMR / zoned drives.** Buckets map one-to-one to zones; write-once-then-erase
  is exactly the semantics zoned media require. Glacier-tier media can be addressed without an
  intervening FTL.
- **Filesystem-scoped FTL.** On SSDs, the filesystem becomes the FTL with full visibility into
  what is live — more predictable than the drive's own FTL under our access patterns.

### 12.1 Buckets

A bucket is identified by `(disk_id, bucket_no)`. Bucket size is encoded as
`Superblock.bucket_size_log2`:

| `bucket_size_log2` | Bucket size | Buckets per 16 TiB disk | Alloc table size |
| ------------------ | ----------- | ----------------------- | ---------------- |
| 18                 | 256 KiB     | 64 M                    | 1 GiB            |
| 20 (default)       | 1 MiB       | 16 M                    | 256 MiB          |
| 22                 | 4 MiB       | 4 M                     | 64 MiB           |

`Superblock.bootstrap_buckets` (typically 32–64) pins the leading buckets used for the
superblock, WAL header, and the root pages of the alloc table itself; these are excluded from the
freespace LRU.

### 12.2 Bucket alloc table

One **§1.5 B+ tree** per disk, rooted at `DiskDescriptorOnDisk.buckets_root`, keyed by
`bucket_no: u32`. With key packing (§1.5.6) sequential bucket numbers compress to ~2 B per key,
so a 256 KiB leaf packs ~14 600 BucketAllocKey entries per bset; a 16 M-bucket disk fits in
**depth 1** (single inner node + ~1 100 leaves):

```rust
#[repr(C, packed)]
struct BucketAllocKey {                      // 16 bytes
    generation: u32,                         // monotonic; matched by BlockRef.generation
    data_type: u8,                           // BucketDataType (below)
    flags: u8,                               // bit 0 = needs_discard, bit 1 = pinned-by-snapshot
    dirty_sectors: u16,                      // live 4 KiB sectors written in this bucket
    last_modify_lsn: u64,                    // for snapshot-diffing the alloc table itself
}

#[repr(u8)]
enum BucketDataType {
    Free        = 0,                         // not in use; freespace LRU candidate
    Wal         = 1,                         // WAL ring buckets
    Index       = 2,                         // index-zone btree pages, tag bitmaps
    Metadata    = 3,                         // object table, location table, forward index
    Blob        = 4,                         // user data blobs
    BtreeNode   = 5,                         // dedicated btree-node buckets (post-change [3])
    Stripe      = 6,                         // erasure-coding stripes (future)
    NeedDiscard = 7,                         // freed, awaiting TRIM
    Reserved    = 8,                         // copygc forward-progress reserve
}
```

The B+ tree itself is housed in a self-bootstrapping subset of metadata buckets (tracked as
`BucketDataType::Metadata` with the pinned flag). Updates go through the journal and are
checkpointed in the same A/B atomic-root commit as everything else.

### 12.3 Generation-checked pointers

`BlockRef` (§2.2) already carries a 64-bit generation. With bucket-based allocation:

```
bucket_no   = block_no >> (bucket_size_log2 - 12)        // e.g. block_no >> 8 for 1 MiB buckets
in_bucket   = block_no &  ((1 << (bucket_size_log2 - 12)) - 1)
```

Dereference protocol:

1. Compute `bucket_no` from `block_no`.
2. Look up `BucketAllocKey` (hot buckets pinned in RAM).
3. If `key.generation != ref.generation`, the pointer is **stale**: silently dropped on reads,
   logged as a corruption signal during scrub.

Generation comparison alone resolves free-block tracking, torn-write detection on freed blocks,
and stale-replica handling — no separate bookkeeping is required. The per-block CRC32C catches
in-bucket corruption independently.

### 12.4 Freespace LRU

A second **§1.5 B+ tree** per disk (`DiskDescriptorOnDisk.freespace_root`), keyed by
`(fragmentation_band, bucket_no)`:

```
fragmentation_band: u8     // 0 = empty (full free), 1..255 = band of (dirty_sectors / max_sectors)
bucket_no: u32
```

Used by:

- **Foreground allocator**: scans `fragmentation_band == 0` for fast bump-allocation of new
  write streams.
- **Copy GC**: scans the most-fragmented non-empty buckets to reclaim space (§12.6).
- **Cache eviction**: cached-replica buckets carry their own LRU, layered on top.

Bands are recomputed lazily — a bucket's band is updated when `dirty_sectors` crosses an 8-sector
boundary, keeping freespace-LRU churn proportional to allocation pressure rather than to
write volume.

### 12.5 Allocator behaviour

Per-disk **write points** track a small set of currently-open buckets, segregated by:

- **Data type** — index, metadata, blob, and WAL never share a bucket.
- **Stream tag** — separate write points for placement-target groups, foreground vs. background
  workloads, and per-tag pinning. This is the same trick bcachefs uses to keep unrelated I/O
  patterns from co-mingling and producing correlated fragmentation.

Allocation on the fast path is a bump: the open bucket's write cursor advances by the requested
sectors. When a bucket fills, it is closed (its `dirty_sectors` stops changing until copygc) and
a fresh bucket is opened from the freespace LRU.

Per-zone hints (§2.1) are advisory rather than hard partitions: a zone defines the *preferred*
disk region for a data type, but the allocator can spill across zone boundaries when the zone
runs short. The authoritative classification is `BucketAllocKey.data_type` per bucket.

### 12.6 Copy GC

Copy GC is one work kind in the reconcile subsystem (§17, `WorkKind::Copygc`). When the
freespace LRU's empty-band count drops below the configured reserve
(`Superblock.copygc_reserve_pct`, default 8 %), the reconcile engine enqueues `Copygc` items
for the most-fragmented buckets.

Each `Copygc` work item is processed via the standard move path (§17.5):

1. **Range-scan the backpointers btree (§6.2) at prefix `(disk_id, bucket_no, *)`** to
   enumerate live extents in the bucket. Entries with stale `bucket_gen` are skipped.
2. Read each live extent from disk (CRC32C validated; BLAKE3 verified at the object level if
   the owner is `BlobExtent`).
3. Write to a fresh bucket via the move path.
4. Atomically update the owning index entry and the backpointer:
   `BackpointerRemove(old_key) ∘ BackpointerInsert(new_key, new_value) ∘ owner-update`.
5. Bump the old bucket's generation, transition it to `NeedDiscard` (or directly to `Free`).
   The generation bump implicitly invalidates any backpointers missed during the scan — they
   become detectable-stale on the next pass.

Copy GC cost is proportional to **fragmentation**, not pool size. The bucket-prefix scan is
~1 large-node load per fragmented bucket. The reserve guarantees forward progress: allocation
can never block on GC because at least one fully-empty bucket is always available.

### 12.7 Discard / TRIM

Buckets transitioning to `Free` first sit in `NeedDiscard` until TRIM is issued (mount option
`discard=true|async|off`). On rotational media TRIM is a no-op and the transition is immediate.

### 12.8 Crash recovery

The bucket alloc table is a B+ tree under the standard COW + journal commit machinery (§2.2).
On unclean shutdown:

1. Each disk's `buckets_root` is loaded from the last committed `RootPointer`.
2. WAL replay applies pending bucket-lifecycle entries (`BucketAlloc`, `BucketWrite`,
   `BucketGenBump`, `BucketDiscard`) in LSN order.
3. Any block whose `BlockRef.generation` does not match the recovered `BucketAllocKey.generation`
   is treated as stale and ignored — exactly as it would be at runtime.

The combination of "WAL is authoritative for recent transitions" and "generation mismatches
invalidate pointers atomically" makes recovery proportional to journal size, not pool size.

---

## 13. In-Memory Mirror Structures

The on-disk format is the source of truth. The in-memory layer is a **cache**, structured for fast
queries:

| In-memory type                               | Mirrors on-disk                           | Lifetime                  |
| -------------------------------------------- | ----------------------------------------- | ------------------------- |
| `ObjectTable` (`Vec<ObjectRecord>` + free-list) | radix table of object pages                | mmap-pinned, written via WAL |
| `TagIndex { HashMap<TagId, TagStore> }`      | TagIndexDirectory + TagBitmap pages       | mmap-pinned roaring containers |
| `KvIndex { HashMap<(TagId,u64), RoaringBitmap> }` | KvDirectory + buckets                  | resident, lazy-load buckets |
| `RangeIndex` (`BTreeMap<(TagId, NormKey), Roaring>`) | B+ tree pages                       | resident, paged in     |
| `ForwardIndex { HashMap<u64, SmallVec<...>> }` | B+ tree                                  | LRU-cached pages       |
| `OntologyState`                              | OntologyRoot                              | fully resident         |
| `ImplicationDag` (`petgraph::Graph<TagId, ()>`) | dag pages                              | fully resident         |
| `SubscriptionEngine`                         | SubscriptionsRoot                         | fully resident         |
| `PathContextManager`                         | PathContextRoot                           | fully resident         |
| `PoolManager`                                | PoolStateRoot                             | fully resident         |
| `BucketCache` (`HashMap<(DiskId, u32), BucketAllocKey>`) | per-disk buckets B+ tree    | hot buckets pinned, cold paged in |
| `WritePoints` (`HashMap<(DiskId, DataType, StreamTag), OpenBucket>`) | derived             | resident; ~hundreds of entries |
| `BTreeNodeCache` (`HashMap<BlockRef, LoadedNode>` + LRU) | §1.5 large nodes (256 KiB each) | journal-pinned nodes never evicted; clean nodes LRU. Working-set ≈ 100–500 hot nodes ⇒ 25–125 MiB. |
| `BackpointerCache` (LRU of bucket-prefix scan results) | §6.2 backpointer btree leaves | populated on demand by copygc / scrub / resilver |
| `SnapshotTree` (`BTreeMap<SnapshotId, SnapshotNode>` + ancestor cache) | §11.1 snapshots btree | fully resident — typically thousands of entries; ancestor checks must be in-cache |
| `ReconcileEngine` (priority queue heads, throttle counters, move-path semaphore) | §17 reconcile btrees | resident; queue heads cached, deeper queue paged from btree |
| `ScanRegistry` (`HashMap<ScanId, ScanState>`) | §17.3 ReconcileScan | resident — small (< 100 active scans) |
| `JournalReclaim` (`BinaryHeap` of (pin_pressure, BlockRef)) | derived from BTreeNodeCache | resident; rebuilt on demand |
| `OpLog` (`VecDeque<OpLogEntry>`)             | recent WAL tail                           | trimmed at checkpoint  |

`Engine` (DESIGN §15) owns these and is wrapped by `DiskEngine` which adds `FileBlockDevice`,
superblock, allocator, and snapshot manager. All mutations follow:

```
1. Acquire engine write lock
2. Tag the mutation with the **current snapshot id** (§11.2) — for snapshot-aware btrees, the
   key includes `snapshot = current_snapshot_id`
3. Append WAL entry (fsync if durability mode = sync)
4. Apply to in-memory mirror (idempotent on lsn)
5. Update affected btree nodes' journal pins: set/extend `pending_lsn_max`, increment
   `pending_count`. Page rewrites are deferred to the journal-reclaim thread (§3.4).
6. Release write lock
```

Reads are mostly lock-free against an `arc-swap`'d snapshot of the relevant index handle.

### 13.1 Roaring bitmap representation

Use the `roaring` crate's standard structure (`RoaringBitmap`). Containers can be built directly
from mmapped portable-format bytes via the crate's `RoaringBitmap::deserialize_from_slice` — this
is essentially zero-copy for array and bitmap containers (run containers may copy).

### 13.2 Inode identity for FUSE

The FUSE bridge (`TagVfs`) does **not** use object IDs as inodes — it allocates 64-bit inodes
lazily as paths are traversed (a `BTreeMap<TagSet, u64>` and `BTreeMap<u64, TagVfsEntry>`). Inodes
are session-local; they are never persisted, since the TagVfs view is a derived projection.

---

## 14. Encryption Integration

The on-disk layout is encryption-aware but encryption-agnostic:

- **Index zone, metadata zone**: blocks encrypted with **XTS-AES-256**, tweak = `block_no`. Fixed
  layout is preserved (no expansion). `BlockHeader` is encrypted along with payload — verifiers
  decrypt then check magic.
- **Blob zone**: encrypted with **HCTR2-AES-128**, tweak = `(object_id || block_no_within_extent)`.
  Length-preserving.
- **WAL**: each entry is **AES-256-GCM** authenticated. Nonce = LSN (96 bits = 64-bit LSN || 32-bit
  zero, nonce-misuse-resistant by construction since LSNs never repeat). The `payload_crc` slot in
  the entry header is replaced by the GCM tag in encrypted mode (flag bit 0 set). The trailing
  framing CRC remains plaintext for I/O-error detection.
- **Sync bundles**: **ChaCha20-Poly1305** with random nonces.

Keys never leave the in-memory key hierarchy (`MasterKEK → DiskKey`). The superblock stores only
key *identifiers*. A node booting without the master key still mounts the disk read-only at the
unencrypted-layout level (block headers, but not payloads).

---

## 15. Format Versioning and Migration

The on-disk format evolves through a **two-version superblock**: the pool records both the
version it currently *writes* with and the minimum version of any data still *on disk*. The
two values move independently — new writes use the current version immediately, while older
data retains its original format until rewritten by background migration (§15.4).
`BlockHeader.format_version` (per-block, per-kind) remains the fine-grained per-structure
version.

### 15.1 Superblock version fields

```rust
fs_format_version:   u32,        // version the pool currently writes with
fs_min_on_disk:      u32,        // smallest version any persistent record uses
compat_features:     u64,        // additive features old readers ignore safely
ro_compat_features:  u64,        // features old readers can read but not write
incompat_features:   u64,        // features old readers cannot read at all
downgrade_log_ref:   BlockRef,   // chain of historical features (§15.5)
```

`fs_min_on_disk ≤ fs_format_version` always holds. They are equal on a freshly formatted pool
and after a complete migration; they differ during the gap between bumping the writing version
and finishing the rewrite of older records.

### 15.2 Feature flags

| Class           | Old reader's behaviour                                            |
| --------------- | ----------------------------------------------------------------- |
| `compat`        | Reads and writes correctly; ignores the new field / record.       |
| `ro_compat`     | Reads correctly; mounts read-only because writes might violate the feature. |
| `incompat`      | Cannot interpret the data; refuses to mount.                      |

A new feature is conservatively classified `incompat` until proven otherwise. Each feature has
a stable bit position; `&~ supported` is the missing-feature set.

### 15.3 Mount-time decisions

On mount, the binary:

1. Picks the active superblock copy by `(seq, lsn)` (§2.2).
2. Refuses mount if `superblock.incompat_features &~ self.supported_incompat != 0`.
3. Mounts read-only if `superblock.ro_compat_features &~ self.supported_ro_compat != 0`.
4. Otherwise mounts read-write.
5. Optionally consults `downgrade_log_ref` (§15.5) for historical incompat features that may
   have left residue on disk.

The `mimir mount --version-upgrade <mode>` flag controls upgrade behaviour:

- `none` — never advance `fs_format_version`; new writes use the existing version.
- `compatible` — advance to the latest version reachable without enabling any `incompat`
  feature. Reversible by older binaries.
- `incompatible` — advance to the binary's full supported version, enabling `incompat`
  features. One-way: older binaries will refuse the pool from this point.

### 15.4 Upgrade flow

An upgrade has two distinct steps that proceed independently:

**Step 1 — bump the writing version.** At the next checkpoint, `fs_format_version` advances to
the new value and the appropriate feature bits are set. Subsequent writes use the new format.
Existing data is untouched; `fs_min_on_disk` is unchanged.

**Step 2 — migrate old data.** The reconcile engine (§17) launches a scan that walks all
snapshot-aware btrees, rewriting any record whose `BlockHeader.format_version` is older than
the current writing version. Each rewrite is COW; partial migrations are crash-safe.

When the scan completes, `fs_min_on_disk` advances to match `fs_format_version` at the next
checkpoint.

`mimir fsck --upgrade` triggers step 2 explicitly and blocks until the scan finishes — useful
when an operator wants the migration done before unmount, before snapshotting, or before
relying on a feature that requires the rewrite.

### 15.5 Downgrade

A downgrade to version `V` is **safe** when `V ≥ fs_min_on_disk` and all
`incompat_features` ever-enabled bits map to features that `V` understands. The invariant: an
older binary can mount the pool as long as every record it might encounter is in a version it
can read.

`downgrade_log_ref` points to a small CBOR-encoded list of `(timestamp, feature_bits, fs_format_version)`
tuples — the historical sequence of feature activations. An older binary consults this on mount
to detect "this pool was once written with feature X" even if X has since been disabled — the
data residue may still violate X's invariants. If any historical incompat feature is unknown
to the binary, mount is refused.

If `fs_min_on_disk > V` (i.e. some records have already been rewritten into formats `V` cannot
read), the only path back is `brunnr export` followed by `brunnr import` into a freshly
formatted pool at version `V`.

### 15.6 Per-structure migration rules

For each `BtreeKind` / `BlockKind`, schema evolution follows one of three patterns:

1. **Additive** — new fields appended in reserved/padding regions; bump that kind's
   `format_version`. Old readers ignore unknown bytes. Classified `compat`.
2. **Layout** — new on-disk shape; allocate a new `BtreeKind` (or `BlockKind`) and keep the old
   one defined. The reconcile-driven migration (§15.4) rewrites instances into the new kind.
   Mid-migration the pool may contain both shapes; new writes always use the new one. Typically
   `ro_compat` (old readers can still read the old shape) or `incompat` (depends on whether
   the new kind appears in critical paths).
3. **Semantic** — the meaning of an existing field changes. Always `incompat`; always requires
   `fs_format_version` advance. Old readers must not interpret the field under the old meaning.

---

## 16. Summary of On-Disk Footprint (10 M objects, 5 000 tags, ~100 live snapshots)

Steady-state footprint. The WAL ring holds the unmaterialised journal tail (≤ 64 MiB); per-key
snapshot overhead scales with logical changes since each snapshot's parent.

| Structure              | Size      | Notes                                              |
| ---------------------- | --------- | -------------------------------------------------- |
| Superblock × 3         | 12 KiB    | Fixed                                              |
| WAL                    | 64 MiB    | Btree-update journal (§3); mirrored across devices |
| Bucket alloc table     | ~180 MiB  | 16 M buckets with packed `bucket_no`               |
| Freespace LRU          | ~12 MiB   | Sparse; key packing on `(band, bucket_no)`         |
| Object table (records) | 1.28 GiB  | Positional — no key packing applies                |
| Object table (radix)   | < 1 MiB   | Single inner node (depth 2 total)                  |
| Location table         | 480 MiB   | Positional — no key packing                        |
| Backpointers           | ~280 MiB  | §6.2 — 10 M extents × ~28 B packed                 |
| Forward index          | ~400 MiB  | §1.5 B+ tree with packed `oid`                     |
| Tag inverted index     | 200–400 MiB | Roaring bitmaps (4 KiB framed), 5 000 tags       |
| KV index               | ~100 MiB  | Extendible hash + roaring bitmaps                  |
| Range index            | ~20 MiB   | §1.5 B+ tree, packed (`attr_id` constant per leaf) |
| Ontology               | <10 MiB   | Modules + DAG                                      |
| Subscriptions          | ~700 KiB  | Per 1 000 subs with packed `sub_id`                |
| Path contexts          | 50 MiB    | One large project; path hashes don't pack          |
| Snapshots btree        | ~10 KiB   | 100 snapshot nodes × 64 B + skiplist overhead      |
| Snapshot key overhead  | ~50 MiB   | Per-snapshot unique keys across all snapshot-aware btrees |
| **Total metadata**     | **~2.5 GiB** | Replicated to every node                       |

**Transient overhead** (not in steady state — fills during specific events, drains afterwards):

| Structure              | Size       | Trigger                                           |
| ---------------------- | ---------- | ------------------------------------------------- |
| Reconcile work btrees  | < 1 MiB idle, ~480 MiB during disk evacuation / cluster resilver | §17.10 |
| `ReconcileScan` cursors| < 1 MiB    | Per active scan; ~hundreds of bytes each          |
| `SnapshotCleanup` work | < 1 MiB    | Bounded by the count of snapshot-aware btrees, not key count (§11.5) |

This is the "few hundred megabytes" of the design intent at moderate scale, and at the upper end
of practical scale still well under 1% of pool storage.

---

## 17. Reconcile

The reconcile subsystem is the unified, **state-driven** engine for background data maintenance.
A single mechanism handles operations that in other filesystems are separate threads with
separate state machines: replica repair, tier migration, option propagation, copy GC, disk
evacuation, auto-tiering, and erasure-coding promotion.

The engine compares **actual state** (where each extent is, how many replicas, which tier,
which compression / encryption) against **desired state** (placement rules, ontology-driven
options, replica counts) and queues work to close the gap. Multiple desired-state changes
compose naturally because each extent is evaluated independently.

### 17.1 What reconcile does

| Mismatch                                          | Action                                       |
| ------------------------------------------------- | -------------------------------------------- |
| Replica count below `Replicate { min_replicas }` rule | Re-replicate to additional disks         |
| Object on wrong tier vs. `Pin` / `Prefer` rule    | Migrate via the move path                    |
| Compression / encryption inconsistent with ontology | Rewrite with correct transform pipeline    |
| `AutoTier` access-time threshold crossed          | Migrate Hot → Warm → Cold → Glacier          |
| Bucket fragmentation > copygc threshold           | Run copy GC (§12.6)                          |
| Disk in `Draining` state                          | Evacuate via backpointer scan (§6.2)         |
| Disk failure detected, replicas missing           | Resilver (§11.7) — re-replicate from peers   |
| Erasure-coding policy applies to cold data        | Encode into stripe (future)                  |
| Snapshot marked deleted (§11.5)                   | Scan + drop / whiteout / re-tag keys at the deleted snapshot id |

Each is just a different `WorkKind` in the same queue. New mismatch types are additive.

### 17.2 Work-item btrees

Five §1.5 B+ trees, all `BtreeKind::Reconcile*` (a sixth, `ReconcileScan`, holds resumable
scan cursors and is described in §17.3):

```rust
enum WorkKind {
    ReplicaRepair    = 1,      // under-replicated; raise to target count
    TierMigrate      = 2,      // wrong tier per placement rule
    OptionUpdate     = 3,      // wrong compression/encryption
    Evacuate         = 4,      // on a Draining disk
    Copygc           = 5,      // fragmented bucket reclaim
    AutoTier         = 6,      // age-based migration
    EcEncode         = 7,      // promote to erasure-coding stripe
    SnapshotCleanup  = 8,      // §11.5 — drop / whiteout / re-tag keys of a deleted snapshot
}

#[repr(C, packed)]
struct WorkItem {                            // 48 B base + variable owner_key
    target_kind: u8,                         // OwnerKind from §6.2
    work_kind: u8,                           // WorkKind
    attempt_count: u8,
    last_error_code: u8,
    flags: u32,                              // bit 0 = ratelimited, bit 1 = persistent
    enqueued_lsn: u64,
    desired_state_ref: BlockRef,             // 16 B → CBOR(DesiredState) for variable detail
    owner_key: [u8; 16],                     // owning key in target btree (packed)
}
```

| Btree                  | Ordering                              | Use                                          |
| ---------------------- | ------------------------------------- | -------------------------------------------- |
| `ReconcileWork`        | logical key (target_kind, owner_key)  | Default queue. Cheap on SSD where logical ≈ physical.|
| `ReconcileHipri`       | same                                  | High-priority items processed first.         |
| `ReconcileWorkPhys`    | physical LBA (disk_id, bucket, sector_offset) | HDD-backed pools — sequential processing avoids seeks. Maintained as a parallel index alongside `ReconcileWork`. |
| `ReconcileHipriPhys`   | same                                  | High-priority physical-order index.          |
| `ReconcilePending`     | logical key                           | Failed items. Retried only after device-config events; avoids spin loops on permanently-blocked work. |

Whether to maintain `*_Phys` indexes is set per-disk via `DiskDescriptorOnDisk` (rotational
hint). Pure NVMe pools skip them.

### 17.3 Triggers

Work enters the queue via two paths:

**1. Per-key triggers.** Every snapshot-aware btree carries a trigger callback that fires on
insert / update / delete. The trigger compares the new state against the relevant rules and
emits a `WorkItem` to `ReconcileWork` (or `ReconcileHipri`) if a mismatch is observed.
Triggers are journalled like any other btree mutation (§3.4) — recovery replays them
deterministically.

**2. Scans.** A scan walks one or more btrees end-to-end, evaluating every key against current
desired state. Scans are launched by:

- **Device state change** — disk added, removed, evacuating, or transitioned to faulted.
- **Placement rule change** — admin updates `pool_state.placement_rules` (§10.4).
- **Ontology change** — installation / upgrade alters compression, encryption, or tier
  selection for a tag.
- **Inode option change on a directory subtree** (e.g. a per-context override).
- **Snapshot marked deleted** — one `SnapshotCleanup` scan per snapshot-aware btree (§11.5);
  the scan rewrites in-memory leaves only, never journalling per key.

Scan state lives in `ReconcileScan` records: `(scan_id, btree, cursor_key, originating_event)`.
A scan that crashes mid-walk resumes from `cursor_key` on the next mount. Scans that perform
in-place key mutations (e.g. `SnapshotCleanup`) are required to be idempotent under cursor
re-entry: a leaf that has already been rewritten must satisfy the scan's filter as a no-op,
so resuming from any earlier cursor produces the same final state.

### 17.4 Priority ordering

Work is processed strictly in this order:

1. `ReconcileHipri` — under-replicated metadata, evacuating metadata.
2. `ReconcileHipri` — under-replicated data, evacuating data.
3. `ReconcileWork` — normal metadata reconciliation.
4. `ReconcileWork` — normal data (tiering, option updates, optimisations,
   `SnapshotCleanup`).
5. `ReconcilePending` — retries (only when prerequisite device-config event has fired).

Within each tier, ordering is logical (SSD) or physical (HDD). `SnapshotCleanup` runs at
priority 4 — it reclaims space but never blocks correctness or safety; under sustained
pressure it yields to copygc and tiering work and resumes from its cursor.

### 17.5 Move path

Reconcile shares one **move path** with copygc: read extent → validate (CRC32C + BLAKE3) →
write to fresh location → atomically update the owning key, the location table (§6.1), and the
backpointer (§6.2) → remove old work item.

Throttling: two pool-wide tunables in `pool_state`:

- `move_bytes_in_flight` (default 64 MiB) — total outstanding move I/O
- `move_ios_in_flight` (default 64) — concurrent move requests

Per-work-kind disable flags (`copygc_enabled`, `tiering_enabled`, …) allow administrative
control without unmounting.

### 17.6 Composition properties

Because reconcile is state-driven, multiple in-flight operations compose without coordination:

- **Evacuating two disks simultaneously** — each extent's desired state is evaluated against
  the current device topology; whichever evacuation reaches it first moves it correctly.
- **Tier policy change during evacuation** — the next evaluation of each extent considers both
  the new tier and the avoid-evacuating-disk constraint together.
- **Replica repair during copygc** — under-replicated data discovered mid-copygc gets a
  `ReplicaRepair` work item enqueued at hipri; copygc continues in parallel.

This is what bcachefs gains by replacing event-driven rebalance/resilver/tiering threads with
one state-driven engine.

### 17.7 Self-healing

If reconcile detects an inconsistency with **no obvious cause** (no recent option change, no
device event), it records an error in `pool_state.errors`: something unexpected happened that
needs operator attention. Degraded data is repaired regardless — the lack of a known cause
doesn't block the repair, only flags it for investigation.

Failed work items in `ReconcilePending` are retried only on device-configuration events (disk
added, state changed, capacity expanded). Without such an event, the prerequisites haven't
changed and a retry would just re-fail.

### 17.8 In-memory state

| Structure | Purpose |
| --- | --- |
| `ReconcileEngine` | Owns the four work btrees' write paths, the move-path semaphore, and the throttle counters. |
| `ScanRegistry` (`HashMap<ScanId, ScanState>`) | In-progress scans; cursor + estimated remaining work for status reporting. |
| `MoveInflight` (`Vec<InflightMove>`) | Bounded by the throttle; each entry carries the read buffer, target write point, and rollback handle. |

The engine runs as a small pool of worker threads (default: one per HDD, two per SSD) draining
the priority queue. Workers acquire the engine write lock only at trigger-firing and at
move-completion atomic-update — not during the read/write of moved data.

### 17.9 Operator interface (DESIGN-level)

`mimir reconcile status` reports per-priority queue depth, in-flight bytes, current scan
progress, and any pending-error items.

`mimir reconcile wait --type evacuate --disk N` blocks until all work of the given type for
the given target completes.

Per-workload pause / resume via `pool_state` flags (e.g. `reconcile_on_ac_only` to pause on
battery power for laptops).

### 17.10 Footprint

Work-item btrees are transient — fill during migration events, drain to near-empty in steady
state. Typical bounds:

- Idle pool: < 1 MiB across all reconcile btrees.
- Disk evacuation in progress (~10 M items, all of an evacuated disk's blob extents): ~480 MiB
  while in flight; drains to zero on completion.
- Cluster resilver after a disk failure: similar.

These are listed under "transient" in §16, not in the steady-state footprint.

---

## 18. References

- Roaring portable serialisation: <https://github.com/RoaringBitmap/RoaringFormatSpec>
- BLAKE3: <https://github.com/BLAKE3-team/BLAKE3-specs>
- HCTR2: <https://eprint.iacr.org/2021/1441>
- CBOR: RFC 8949
- CRC32C: RFC 3720 §12.1
- HKDF: RFC 5869
- Argon2id: RFC 9106
