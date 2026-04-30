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
4. **Snapshottable indices.** Index updates are copy-on-write at the page level. Every committed
   superblock pins a self-consistent index root. Snapshots are taken by retaining the old root;
   diffs between snapshots drive cluster sync.
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

Every block-sized persistent structure that is written or replaced as a unit begins with a 32-byte
header:

```rust
#[repr(C, packed)]
struct BlockHeader {              // 32 bytes
    magic: [u8; 4],               // "MIMR"
    kind: u16,                    // BlockKind discriminator
    format_version: u16,          // structure-specific version
    payload_length: u32,          // bytes following the header (excl. trailing CRC)
    generation: u64,              // monotonic per-block generation, for COW
    lsn: u64,                     // WAL LSN that produced this block
    flags: u32,                   // bit 0 = encrypted, bit 1 = continuation
}
```

TODO: payload_length cannot be u32, since block is at most 4Kb?

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
    TagDirectory,       // §8.1 tag directory B+ tree (snapshot-aware)
    Range,              // §9.2 range index B+ tree (snapshot-aware)
    ChunkIndex,         // §9.3 chunk index B+ tree (content-addressed; snapshot-agnostic)
    ValueSpill,         // value-hash → CBOR(Value); content-addressed
    Ontology,           // §10.1 ontology / dag B+ tree (snapshot-aware)
    PathContext,        // §10.3 path context B+ tree (snapshot-aware)
    Subscriptions,      // §10.2 subscription B+ tree (snapshot-aware)
    Snapshots,          // §11.1 snapshot tree (SnapshotId → SnapshotNode)
    BucketAlloc,        // §12.2 per-disk bucket alloc B+ tree (physical)
    FreespaceLru,       // §12.4 per-disk freespace LRU B+ tree (physical)
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
    magic: [u8; 4],                          // "MIMB"
    kind: u16,                               // BtreeKind: ObjectTable, Forward, Range, …
    format_version: u16,                     // per-kind structural version
    seq: u64,                                // monotonic per-region; bumped on each rewrite
    last_persisted_lsn: u64,                 // BlockHeader.lsn analogue
    region_size_log2: u8,                    // 18 = 256 KiB
    level: u8,                               // 0 = leaf, ≥1 = inner
    bset_count: u8,                          // number of bsets present
    flags: u8,                               // bit 0 = compaction-in-progress (recovery hint)
    payload_used: u32,                       // bytes consumed by all bsets so far (≤ region_size − 64)
    min_key: [u8; 16],                       // covered key range (interpreted per-kind)
    max_key: [u8; 16],                       // ditto
}
```

Subsequent bytes are a **stream of bsets**, each preceded by:

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

For a 10 M-object pool, the radix object table goes from 4 levels (4 KiB pages) to **2 levels**
(256 KiB pages). Tag, range, and forward indexes drop from 4 levels to **2–3 levels**. Cache
working set shrinks by 64× in page count, with the same total bytes. Sequential I/O on every
node access — critical for HDD performance.

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

    // Two alternating root pointers — atomic commit.
    root_a: RootPointer,                     // [112..176]  64 bytes
    root_b: RootPointer,                     // [176..240]
    active_root: u8,                         // [240..241]  0 = a, 1 = b
    _pad2: [u8; 7],                          // [241..248]

    // Static layout pointers (set at format time, not written again).
    wal_offset: u64,                         // [248..256]
    wal_size: u64,                           // [256..264]
    bucket_size_log2: u8,                    // [264..265]   e.g. 20 = 1 MiB bucket
    copygc_reserve_pct: u8,                  // [265..266]   default 8 (range 5..=21)
    btree_node_size_log2: u8,                // [266..267]   default 18 = 256 KiB (§1.5)
    _pad3: [u8; 5],                          // [267..272]
    bootstrap_buckets: u32,                  // [272..276]   reserved leading buckets (sb + WAL + …)
    _pad4: [u8; 4],                          // [276..280]
    _reserved_alloc: [u8; 16],               // [280..296]   space freed by removed bitmap fields
    zone_map_offset: u64,                    // [296..304]   0 until any zone is grown

    // Initial-extent zone descriptors. Always authoritative for the first extent;
    // additional extents (if any) are listed in the ZoneMap block.
    index_zone:    ZoneExtent,               // [304..328]   24 bytes
    metadata_zone: ZoneExtent,               // [328..352]
    blob_zone:     ZoneExtent,               // [352..376]

    encryption_keyid: [u8; 16],              // [376..392]   key identifier (not the key)
    _reserved: [u8; 3700],                   // [392..4092]  zeroed, available for future fields
    // trailing CRC32C lives inside BlockHeader's frame
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

### 2.2 Atomic root commit

The two-root scheme is the only way to update the filesystem state durably without TOCTTOU
windows:

```rust
#[repr(C, packed)]
RootPointer {                                // 64 bytes
    seq: u64,                                // monotonic; larger seq wins
    lsn: u64,                                // WAL LSN this root corresponds to
    object_table_root:    BlockRef,          // 16 bytes — see §2.3
    location_table_root:  BlockRef,
    forward_index_root:   BlockRef,
    tag_index_root:       BlockRef,          // root of TagIndexDirectory
    kv_index_root:        BlockRef,
    range_index_root:     BlockRef,
    ontology_root:        BlockRef,
    path_context_root:    BlockRef,
    subscriptions_root:   BlockRef,
    pool_state_root:      BlockRef,
    snapshot_chain_root:  BlockRef,          // root of the snapshots btree — §11.1
    flags: u32,
    crc: u32,                                // CRC32C of bytes [0..60]
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

This shifts the per-mutation cost from "rewrite a 4-deep COW path" (≈ 16 KiB of writes) to "append
one ≈ 80-byte journal entry". Btree page rewrites are amortised across all the mutations that
touched each node since its last flush. For our workload — where one tag mutation can touch four
index trees — the savings are ≥ 100×.

Internal layout is segmented to allow parallel truncation and replay.

### 3.1 WAL headers (two 4 KiB blocks at `wal_offset`)

```rust
#[repr(C, packed)]
struct WalHeader {
    header: BlockHeader,                     // kind = WalSegment, format_version = 1
    next_lsn: u64,
    write_cursor: u64,                       // byte offset within WAL ring
    read_cursor: u64,                        // oldest entry not yet checkpointed
    used_bytes: u64,
    last_checkpoint_lsn: u64,
    last_checkpoint_offset: u64,             // block_no of newest Checkpoint block
    segment_size: u32,                       // typically 1 MiB
    encryption_keyid: [u8; 16],
    _reserved: [u8; ...],
}
```

The header itself is updated using the same A/B alternation as the superblock root — the WAL
header lives in two adjacent blocks and the active one is selected by `(seq, crc)`.

### 3.2 WAL entry

Entries are byte-packed, never crossing a 4 KiB boundary unless `payload_length` > 4060, in which
case the entry is split into `Continuation` frames (flag bit 1 in `BlockHeader.flags`).

```rust
#[repr(C, packed)]
struct WalEntryHeader {                      // 32 bytes
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
SnapshotKeyMove  : { from_snap: u32, to_snap: u32, btree: BtreeKind, key: bytes }
                   // re-tags a key during deletion's runtime phase (§11.5)

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

Each node is a 256 KiB region (§1.5) with a 64-byte `BtreeNodeHeader` and per-bset framing.
For the radix variants, the header is followed by a single positional payload (no internal bsets
— positional updates are journalled via §3.4 and merged on flush):

- **Leaf node** (`BtreeKind::ObjectTable`, level 0): up to **2044** × `ObjectRecord` (128 B)
  packed as a positional array. With a 64 B header + 32 B leaf metadata trailer (occupancy
  bitmap, generation, reserved): 256 KiB − 96 B = 261 952 B / 128 B = 2046, rounded down to 2044
  for alignment headroom.
- **Inner node** (level ≥ 1): up to **16 380** × `BlockRef` (16 B) = 262 080 B; minus the 64 B
  header that's 16 376 effective entries, rounded to 16 380 with a small trailing slot table.

### Tree depth and capacity

| Depth (inner levels + leaf) | Max objects                              |
| --------------------------- | ---------------------------------------- |
| 0 inner (leaf only)         | 2 044                                    |
| 1 inner                     | 2 044 × 16 380 ≈ 33 M                    |
| 2 inner                     | 2 044 × 16 380² ≈ 549 G                  |

A 10 M-object pool sits in a **single-inner-level tree** (root inner + leaves; depth 2). The
full 48-bit local-id space is reachable at depth 3 (≈ 9 P objects). The 4-bit `level` field in
`BtreeNodeHeader` supports depths 0–15.

`RootPointer.object_table_root` is a `BlockRef` to the topmost node; the node's
`BtreeNodeHeader.level` identifies whether it is a leaf (very small pool) or an inner node.

### Address translation (oid → leaf slot)

```rust
let mut idx = oid_local;
let leaf_slot  = (idx % 2044) as u16; idx /= 2044;
let mut child_path = [0u16; MAX_LEVELS];
for level in 0..root_level {
    child_path[level] = (idx % 16380) as u16;
    idx /= 16380;
}
debug_assert_eq!(idx, 0);  // remaining bits would mean the tree is too shallow
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
    tag_count: u16,                          //  [80..82]
    attr_count: u16,                         //  [82..84]
    compression: u8,                         //  [84..85]
    encryption: u8,                          //  [85..86]
    _pad0: u16,                              //  [86..88]
    inline_tags: [u32; 4],                   //  [88..104]   first 4 tags inline
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
metadata zone, addressed by `overflow_offset`. It is a self-contained 4 KiB block:

```
struct OverflowRecord {
    header: BlockHeader,                     // kind = OverflowRecord (§1.3)
    object_id: u64,
    tag_count: u32,
    attr_count: u32,
    relation_count: u32,
    _pad: u32,
    // Followed by:
    //   tag_count × u32                                      (extra tag IDs)
    //   attr_count × { key: u32, value_hash: u64,
    //                  inline_value: [u8; 96] | spill: BlobRef }
    //   relation_count × { predicate: u32, target: u64 }
    // ... up to 4032 bytes payload, then trailing CRC.
}
```

If an object outgrows even a 4 KiB overflow record, a continuation chain is used (`flags` bit 1).
At that point switching to a B+ tree per-object is more efficient and is the planned escape hatch
for the rare wide objects (≥ 200 tags).

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

- **Leaf node** (`BtreeKind::LocationTable`, level 0): 256 KiB region holds 5 458 ×
  `ObjectLocation` (48 B) = 261 984 B, with a 64 B `BtreeNodeHeader` and a small trailer.
  → **5 458 records per leaf.**
- **Inner node**: identical to §5's inner — 16 380 × `BlockRef`.

Capacity:

| Depth | Max objects                              |
| ----- | ---------------------------------------- |
| 0     | 5 458                                    |
| 1     | 5 458 × 16 380 ≈ 89 M                    |
| 2     | 5 458 × 16 380² ≈ 1.46 T                 |

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
struct ReplicaRef {                          // 8 bytes (DESIGN's 7-byte form padded for alignment)
    disk_id: u16,
    _pad: u16,
    offset_blocks: u32,                      // 4 KiB units → up to 16 TiB per disk; widen later
}
```

For chunked objects (`flags & 1`), `extent_offset` instead points to a `ChunkList` block
referencing N (chunk_hash, BlobRef) pairs — see §9.

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
    count: u16,                              // number of inline assertions
    spill: u16,                              // bit 15 = is_spill
    body: union {
        inline: [PackedAssertion; count],    // count × 16 B (when not spilled)
        spill_ref: BlockRef,                 // 16 B BlockRef into ForwardOverflow region
    },
}

struct PackedAssertion {                     // 16 bytes (not key-packed; values stay byte-aligned)
    kind: u8,                                // 0=Tag, 1=Attr, 2=Relation
    origin: u8,                              // 0=Direct, 1=Materialized
    _pad: u16,
    a: u32,                                  // tag id (Tag/Attr) or predicate (Relation)
    b: u64,                                  // value_hash (Attr), target oid (Relation), 0 (Tag)
}
```

The **key** (`oid`) is packed; the **value** (count, spill, body) stays byte-aligned. Per-leaf
format descriptor records `oid_base = leaf.min_oid` and `oid_bits = ⌈log₂(leaf.max_oid −
leaf.min_oid + 1)⌉`.

A leaf with 8 assertions per object (typical) packs ~2 700 entries per bset (vs ~1 800 before
key packing); with 4 active bsets the leaf carries up to ~10 000 entries before full compaction.

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

A **B+ tree of large nodes** (§1.5) keyed by `TagId: u32`. Leaf entries are 32 B:

```
TagIndexLeafEntry {                          // 32 bytes
    tag_id: u32,                             // sort key
    store_kind: u8,                          // Simple / Ordered / Ranked
    _pad: u8,
    cardinality: u32,                        // for fast snapshot stats
    last_modify_lsn: u64,
    generation: u32,                         // bumped on bitmap rewrite
    store_root: BlockRef,                    // 16 B → §8.2 / §8.3
}
```

With key packing (§1.5.6) the `tag_id` typically packs to 2–3 bytes per leaf bset (sparse but
clustered ids). A 256 KiB leaf holds ~9 000 entries per bset; the full ontology of 5 000 tags
fits in **a single leaf** (depth 0). For pools with hundreds of thousands of tags the tree
extends to depth 1 (~150 M-tag capacity).

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
KvDirectory (4 KiB block, doubles when global depth grows):
   header (BlockHeader, kind = KvHashBucket with directory flag in BlockHeader.flags)
   global_depth: u8
   _pad: u8
   bucket_count: u16
   entries: [BlockRef; 512]    // local-depth tagged buckets

KvBucket (4 KiB):
   header (BlockHeader, kind = KvHashBucket)
   local_depth: u8
   entry_count: u16
   _pad: u8
   entries: [{ tag_id: u32, value_hash: u64, bitmap_ref: BlockRef }; ~120]
```

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

Net per-key size: typically **8–12 B** packed vs 28 B unpacked (60–70% saving). Each 256 KiB
leaf packs ~25 000 entries per bset, vs ~7 500 unpacked.

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
  §1.5 B+ tree, key = name_hash → PathContextHeader { name_offset, manifest_root, _stats }
  Manifest is a §1.5 B+ tree keyed by path-string-hash → ProjectedEntry (96 bytes inline +
  spill for Symlink targets and long paths).
```

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

Snapshots are **key-level**, modelled on bcachefs: a snapshot is a 32-bit ID embedded in the
position of every key in a snapshot-aware btree. Multiple versions of the "same" logical key
coexist in one btree, distinguished by their `snapshot` field. Visibility is determined by walking
the snapshot tree (§11.1).

This replaces the page-COW snapshot model. Creation is **O(1)** regardless of pool size — no
keys are copied, no checkpoint forced. Many thousands or millions of snapshots can exist
simultaneously, limited only by the disk space their per-key overhead consumes.

### 11.1 Snapshot tree

A `SnapshotId` is a `u32` (4 G snapshots before recycling concerns). Snapshot relationships are
stored in the **snapshots btree** (`BtreeKind::Snapshots`, a §1.5 B+ tree keyed by `SnapshotId`):

```rust
#[repr(C, packed)]
struct SnapshotNode {                        // 64 bytes
    id: u32,                                 // self-id (also the btree key)
    parent: u32,                             // 0 = root snapshot
    children: [u32; 2],                      // bcachefs-style binary structure
    depth: u16,                              // distance from root
    flags: u8,                               // bit 0 = leaf (subvolume-bearing)
    _pad: u8,
    ancestor_bitmap: u128,                   // bits[i] = "id − i is an ancestor", i ∈ 0..128
    skiplist: [u32; 3],                      // randomised ancestor IDs for O(log n) deep checks
    created_ns: i64,
    label_offset: u32,                       // into a string heap; 0 = unlabelled
    _reserved: u32,
}
```

**Ancestry check.** The 128-bit `ancestor_bitmap` answers `is X an ancestor of Y?` in O(1) when
`Y.id − X.id ≤ 128` — which covers the vast majority of cases (most snapshots reference recent
ancestors). For older queries, the 3-entry randomised skiplist provides O(log n) traversal to
the root. During early recovery, before this data is validated, queries fall back to a simple
parent-pointer walk.

`RootPointer.snapshot_chain_root` is repurposed as the snapshots btree root — the chain is no
longer a linked list of `SnapshotRecord` blocks but a btree of `SnapshotNode` keys.

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

When reading at snapshot `S`, the iterator walks the btree in order. For keys with the same
non-snapshot prefix, it picks the one with the highest `snapshot ≤ S` that is an ancestor of `S`.
A key with a `KEY_TYPE_whiteout` value in an ancestor snapshot blocks visibility of older
versions for descendants of that snapshot.

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
copied. Both children inherit visibility of all ancestor keys through the tree.

```
Before snapshot:                After snapshot:

       N (current)                       N
                                         │
                                       split
                                       /   \
                                     N₁     N₂  (N₁ = new "current", N₂ = the snapshot)
```

Subsequent writes to the current view are tagged with `N₁`; reads against the snapshot use `N₂`.
Divergence happens only where modifications occur.

A new `BackpointerInsert` is **not** issued for shared extents — the underlying blob is unchanged
and its existing backpointer is valid for both snapshots. Backpointers are physical, not logical
(§6.2 / §11.2 table), so they're snapshot-agnostic.

### 11.5 Snapshot deletion

Deleting a snapshot marks it for asynchronous cleanup by a background pass (DESIGN §7.5
analogue). The cost is proportional to the volume of data unique to the deleted snapshot:

1. **Runtime phase**: walk every snapshot-aware btree. For keys with `snapshot == deleted_id`,
   either drop them outright (if a child snapshot has a newer overriding key) or convert them to
   whiteouts (if ancestor visibility must be preserved for siblings). For interior nodes that
   lose all but one child, keys are moved to the surviving child (re-tagging from
   `deleted_id` to the surviving descendant's id).
2. **Next-mount phase**: interior `SnapshotNode` removal — updating `depth` and `skiplist` fields
   atomically across the affected subtree — is deferred to recovery's single-threaded context.

A snapshot with two children cannot be deleted directly; one child must be deleted first.
Multiple snapshot deletions are batched and processed in a single pass.

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

This eliminates the per-snapshot `pinned_buckets` set the page-COW model used. Snapshot
retention now scales with **logical changes**, not bucket counts.

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

This subsumes both former diff paths:

- The **hot path** (formerly journal streaming) is now "iterate keys with `snapshot ∈ new_only`"
  — equivalent to a btree range scan filtered by the small `new_only` set.
- The **cold path** (formerly COW tree walk) is the same operation; no special case needed.

The result is the same `SyncBundle` shape:

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

**Resilver path.** Unchanged from the prior section: when a peer comes back from a degraded
state with one disk missing, recovery is **backpointer-driven** (§6.2). Backpointers are
snapshot-agnostic, so resilver enumerates all backpointers on the affected disk and re-fetches
the corresponding extents irrespective of which snapshot owns them.

### 11.8 Retention policy

Snapshot space cost is now per-key (not per-page). Per-snapshot overhead in steady state:

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
2. Atomically retag the "current" pointer to `S`'s child slot (or create a new child of `S`).
3. The current view now reflects `S`'s state; subsequent writes diverge at the new child.

No data is copied. The rollback is O(1); subsequent reads pay the snapshot-iteration cost
(§11.3) until garbage collection (§11.5) tidies away the abandoned-side keys.

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

Generation comparison replaces all the bookkeeping we previously needed for free-block tracking,
torn-write detection on freed blocks, and stale-replica handling. The per-block CRC32C catches
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

When the freespace LRU's empty-band count drops below the configured reserve
(`Superblock.copygc_reserve_pct`, default 8 %), copy GC:

1. Selects the most-fragmented bucket(s) from the freespace LRU.
2. **Range-scans the backpointers btree (§6.2) at prefix `(disk_id, bucket_no, *)`** to
   enumerate live extents in the bucket. Entries with stale `bucket_gen` are skipped.
3. For each live extent, reads it from disk (CRC32C validated; BLAKE3 verified at the object
   level if the owner is `BlobExtent`).
4. Writes the extent into a fresh bucket via the move path.
5. Atomically updates the owning index entry and the backpointer:
   `BackpointerRemove(old_key) ∘ BackpointerInsert(new_key, new_value) ∘ owner-update`.
6. Bumps the old bucket's generation, transitions it to `NeedDiscard` (or directly to `Free`).
   The generation bump implicitly invalidates any backpointers we missed — they become
   detectable-stale on the next scan.

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

Every persistent structure carries `format_version` (in its `BlockHeader`) and, for
super-structures, the superblock's `format_version` gates ensemble layout.

Rules:
1. **Additive changes** — new fields added in reserved/padding regions; bump structure
   `format_version`. Old readers continue to function (they ignore unknown bytes); old writers
   should not be used after format bump.
2. **Layout changes** — new structure variant; old kind retained, new kind allocated, on-disk
   migration tool walks structures and rewrites in place via COW (so a partial migration can be
   rolled back via snapshot).
3. **Semantic changes** — bump superblock `format_version` major; refuse mount with old code.

A `mimir fsck --upgrade` command walks the COW trees, re-emitting any structures whose version is
older than the current build's preferred version, batching the rewrites into normal checkpoints so
the migration is crash-safe and resumable.

---

## 16. Summary of On-Disk Footprint (10 M objects, 5 000 tags, ~100 live snapshots)

Steady-state footprint with the journal-of-btree-updates model, packed-key bsets (§1.5.6), and
key-level snapshots (§11): per-snapshot overhead is per-key, not per-page, so 100 sync
snapshots add a few MB of unique keys rather than hundreds of MB of COW pages. B+ tree
footprints reflect the ~30 % compression from key packing; positional radix tables and bitmap
structures are unaffected. The WAL ring holds the unmaterialised tail (≤ 64 MiB).

| Structure              | Size      | Notes                                              |
| ---------------------- | --------- | -------------------------------------------------- |
| Superblock × 3         | 12 KiB    | Fixed                                              |
| WAL                    | 64 MiB    | Btree-update journal (§3); mirrored across devices |
| Bucket alloc table     | ~180 MiB  | 256 MiB unpacked × ~70 % from `bucket_no` packing  |
| Freespace LRU          | ~12 MiB   | Sparse; key packing on `(band, bucket_no)`         |
| Object table (records) | 1.28 GiB  | Positional — no key packing applies                |
| Object table (radix)   | < 1 MiB   | Single inner node (depth 2 total)                  |
| Location table         | 480 MiB   | Positional — no key packing                        |
| Backpointers           | ~280 MiB  | §6.2 — 10 M extents × ~28 B packed                 |
| Forward index          | ~400 MiB  | §1.5 B+ tree with packed `oid`; was ~600 MiB       |
| Tag inverted index     | 200–400 MiB | Roaring bitmaps (4 KiB framed), 5 000 tags       |
| KV index               | ~100 MiB  | Extendible hash + roaring bitmaps                  |
| Range index            | ~20 MiB   | §1.5 B+ tree, packed (`attr_id` constant per leaf) |
| Ontology               | <10 MiB   | Modules + DAG                                      |
| Subscriptions          | ~700 KiB  | Per 1 000 subs with packed `sub_id`                |
| Path contexts          | 50 MiB    | One large project; path hashes don't pack          |
| Snapshots btree        | ~10 KiB   | 100 snapshot nodes × 64 B + skiplist overhead      |
| Snapshot key overhead  | ~50 MiB   | Per-snapshot unique keys across all snapshot-aware btrees (vs. 500 MiB COW chain) |
| **Total metadata**     | **~2.5 GiB** | Net win: snapshot overhead drops by ~450 MiB vs. page-COW model |

This is the "few hundred megabytes" of the design intent at moderate scale, and at the upper end
of practical scale still well under 1% of pool storage.

---

## 17. References

- Roaring portable serialisation: <https://github.com/RoaringBitmap/RoaringFormatSpec>
- BLAKE3: <https://github.com/BLAKE3-team/BLAKE3-specs>
- HCTR2: <https://eprint.iacr.org/2021/1441>
- CBOR: RFC 8949
- CRC32C: RFC 3720 §12.1
- HKDF: RFC 5869
- Argon2id: RFC 9106
