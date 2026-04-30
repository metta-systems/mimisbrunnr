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
    WalSegment,
    AllocBitmap,
    BlockClassMap,
    ObjectTablePage,
    LocationTablePage,
    ForwardIndexPage,
    TagBitmapPage,
    BPlusInner,
    BPlusLeaf,
    KvHashBucket,
    OntologyPage,
    PathContextPage,
    Checkpoint,
}
```

The **format_version** is *per-kind*, not global. Old kinds are evolved independently. A reader
that encounters an unknown `(kind, format_version)` aborts with a clear "format too new" error.

### 1.4 CRC

CRC32C (Castagnoli, hardware-accelerated on x86 SSE 4.2 and ARMv8 CRC). Sufficient for 4 KiB blocks
and faster than CRC32. BLAKE3 is used for *content* integrity (per-object), not for block
checksums.

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
    alloc_bitmap_offset: u64,                // [264..272]
    alloc_bitmap_size: u64,                  // [272..280]
    block_class_map_offset: u64,             // [280..288]
    block_class_map_size: u64,               // [288..296]
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
    snapshot_chain_root:  BlockRef,          // chain of historical roots — §11
    flags: u32,
    crc: u32,                                // CRC32C of bytes [0..60]
}
```

`BlockRef` is `{ disk_id: u16, _pad: u16, block_no: u32, generation: u64 }` — 16 bytes,
self-validating against the destination block's header.

**Commit protocol:**

1. Write all dirty COW pages to free blocks (allocation bitmap mutated in WAL only).
2. Append `Checkpoint` WAL entry referencing the new pages and computing the new `RootPointer`.
3. `fsync` the WAL segment.
4. Write the new `RootPointer` into the **inactive** slot of all superblock copies, flip
   `active_root`, recompute superblock CRC, write all 3 superblock copies, `fsync` the device.
5. Old root and its no-longer-referenced pages are released to the allocator on the next
   checkpoint.

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

The WAL is a 64 MiB **circular** byte log (size configurable; must be a multiple of 4 KiB) on the
fastest disk, mirrored to a second disk. Internal layout is segmented to allow parallel truncation
and replay.

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
CreateObject  : { oid: u64, generation: u32, created_ns: i64 }
DeleteObject  : { oid: u64, lsn: u64 }
AddTag        : { oid: u64, tag: u32, origin: u8 }
RemoveTag     : { oid: u64, tag: u32 }
SetAttr       : { oid: u64, key: u32, value: Value }       // Value tagged-union (§4.2)
RemoveAttr    : { oid: u64, key: u32, value_hash: u64 }
AddRelation   : { oid: u64, predicate: u32, target: u64 }
RemoveRelation: { oid: u64, predicate: u32, target: u64 }
WriteBlob     : { oid: u64, content_hash: [u8;32], extent: ExtentRef, size: u64 }
Checkpoint    : { new_root: RootPointer, freed_blocks_root: BlockRef, alloc_delta: BlockRef }
```

Replay applies entries strictly in LSN order. Each in-memory mutation is idempotent under
`(lsn ≤ structure.lsn)` shortcutting, so replay is safe across crashes mid-replay.

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

Logically a flat array indexed by `ObjectId.local`. Physically, a **COW radix tree** of 4 KiB
pages whose depth grows with the populated id space.

### Page capacities

A 4 KiB block carries a 32-byte `BlockHeader` plus a 4-byte trailing CRC, leaving **4060 bytes**
of payload.

- **Leaf page** (`ObjectTablePage`): 31 × `ObjectRecord` (128 B) = 3968 B, followed by a 92-byte
  tail holding a 31-bit occupancy bitmap, a leaf generation counter, and reserved space.
  → **31 records per leaf.**
- **Inner page** (`ObjectTableInner`): up to 253 × `BlockRef` (16 B) = 4048 B, with the page's
  tree level encoded in `BlockHeader.flags` (4 bits, supports depths 0–15) and 12 bytes of
  trailing pad.
  → **253 children per inner page.**

### Tree depth and capacity

| Depth (inner levels + leaf) | Max objects                      |
| --------------------------- | -------------------------------- |
| 0 inner (leaf only)         | 31                               |
| 1 inner                     | 31 × 253 ≈ 7 843                 |
| 2 inner                     | 31 × 253² ≈ 1.98 M               |
| 3 inner                     | 31 × 253³ ≈ 502 M                |
| 4 inner                     | 31 × 253⁴ ≈ 127 G                |

A 10 M-object pool sits comfortably in a 3-inner-level tree (≤ 502 M). The 48-bit local id space
is reachable at 5 inner levels (≈ 32 T objects) — well below the 16-level limit imposed by the
4-bit level field.

`RootPointer.object_table_root` is a `BlockRef` to the topmost page; its `BlockHeader.flags`
identify whether it is a leaf (small pool) or an inner page at level *N*.

### Address translation (oid → leaf slot)

```rust
let mut idx = oid_local;
let leaf_slot = (idx % 31) as usize;  idx /= 31;
let mut child_path = [0u16; MAX_LEVELS];
for level in 0..root_level {
    child_path[level] = (idx % 253) as u16;
    idx /= 253;
}
debug_assert_eq!(idx, 0);  // any remaining bits would mean the tree is too shallow
```

Object IDs are allocated sequentially per node, so populated leaves cluster densely and the tree
stays compact. A missing child pointer in any inner page marks an unallocated id range (cleared
slots are likewise sparse — the leaf occupancy bitmap distinguishes "never allocated" from
"cleared", §5.1 / DESIGN §7.3).

### Tree growth

When the root is full and a new id falls outside its range, a new inner page one level higher is
allocated, populated with the previous root as its first child, and committed as the new
`object_table_root` in the next `Checkpoint`. Tree growth is therefore log-amortised and never
requires a wholesale rewrite.

### COW write path

A page is rewritten copy-on-write: any update allocates a fresh page, writes it, then each parent
inner page is replaced up to the root. A single record edit at depth *D* (= root level + 1) costs
*D* × 4 KiB writes — for a 10 M-object pool, **4 page writes per record edit** (leaf + 3 inner).
A run of edits within one transaction batches the rewrite at each level, so contiguous edits over
a full leaf still cost only 4 pages.

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
    header: BlockHeader,                     // kind = ObjectTablePage, but flag = overflow
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

## 6. Location Table

Same COW radix-tree machinery as the object table (§5), parameterised for 48-byte
`ObjectLocation` records:

- **Leaf page** (`LocationTablePage`): 84 × `ObjectLocation` (48 B) = 4032 B, followed by a
  28-byte tail (84-bit occupancy bitmap = 11 B, leaf generation, reserved).
  → **84 records per leaf.**
- **Inner page**: 253 × `BlockRef`, identical to §5's `ObjectTableInner`.

Capacity at depth *d* (= inner levels above the leaf, plus the leaf): 84 × 253^(d−1). A 10 M-pool
fits in 4 levels (≈ 1.36 G capacity); the 48-bit local id space is reachable at 5 levels.

The radix is keyed by the same `ObjectId.local`, so the location table tracks the object table
slot-for-slot — a record edit costs the same 4 page writes at 10 M scale.

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

---

## 7. Forward Index

The forward index maps `oid → [ForwardEntry]` and must support fast per-object listing and
per-object diffing for sync.

On disk, it is a **B+ tree** keyed by `oid`, with leaf values that are **inline-or-spill** vectors:

```
B+ Tree:
  Inner page (4 KiB): { keys: [u64; N], children: [BlockRef; N+1] }, N ~= 250
  Leaf  page (4 KiB): { entries: [LeafEntry] }
                      LeafEntry { oid: u64, count: u16, _pad: u16,
                                  inline: [PackedAssertion; 8] | spill: BlockRef }
```

A `PackedAssertion` is fixed at 16 bytes:

```
struct PackedAssertion {                     // 16 bytes
    kind: u8,                                // 0=Tag, 1=Attr, 2=Relation
    origin: u8,                              // 0=Direct, 1=Materialized
    _pad: u16,
    a: u32,                                  // tag id (Tag/Attr) or predicate (Relation)
    b: u64,                                  // value_hash (Attr), target oid (Relation), 0 (Tag)
}
```

For attributes whose actual `Value` matters (not just its hash), the `value_hash` indirects into
the **value spill table** (a separate B+ tree keyed by `value_hash → CBOR(Value)`), shared with
the KV index (§8). Tag and Relation entries are self-contained.

Spill threshold: more than 8 assertions per object → leaf entry stores `BlockRef` to a 4 KiB
**ForwardOverflow** block holding up to 252 `PackedAssertion`s; further overflow chains.

This layout supports:

- O(log N) lookup by oid.
- O(1) per-assertion diffing via `last_modify_lsn` on the object record.
- Compact in-memory mirror: a `HashMap<u64, SmallVec<[PackedAssertion; 8]>>` mirrors hot pages.

---

## 8. Tag Inverted Index

The tag index is the heart of query performance. Its on-disk form must:

- Look up a tag's bitmap quickly.
- Allow per-tag COW updates without rewriting unrelated tags.
- Support delta-sync: cheap diff between two snapshots of the same bitmap.

### 8.1 TagIndexDirectory

```
TagIndexDirectory (B+ tree, key = TagId u32):
   leaf entry: { tag_id: u32, store_kind: u8, _pad: u8,
                 stats: TagStats (16 bytes),
                 store_root: BlockRef }
```

`store_kind` ∈ { `Simple`, `Ordered`, `Ranked` } selects the layout pointed to by `store_root`.
`TagStats { cardinality: u32, last_modify_lsn: u64, generation: u32 }` enables fast snapshot
diffing without dereferencing the bitmap.

### 8.2 Roaring bitmaps on disk

We adopt the **Roaring portable serialization spec** (the same format used by the `roaring` crate
and Apache Lucene) but framed inside `BlockHeader`-prefixed pages:

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
absolute block_no of each container so they can be loaded individually (mmap-friendly random
access). For small bitmaps that fit in one block, the directory and containers are co-located in
the single block.

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

Hash-based on disk via **extendible hashing** keyed by `(tag_id, value_hash)`:

```
KvDirectory (BlockRef array, 4 KiB block, doubles when global depth grows):
   entries: [BlockRef; 512]    // local-depth tagged buckets
KvBucket (4 KiB):
   header
   local_depth: u8
   entry_count: u16
   entries: [{ tag_id: u32, value_hash: u64, bitmap_ref: BlockRef }; ~120]
```

The bitmap referenced by each entry is itself a `TagBitmap` (§8.2), reused via the same machinery.
Extendible hashing is chosen over a B+ tree here because lookups are exact-match only and the
constant factor on point queries is roughly half that of a B+ tree.

The accompanying **value spill table** is a B+ tree `value_hash → CBOR(Value)` so the actual value
can be reconstructed when needed (display, faceted enumeration, range comparisons).

### 9.2 Range Index

Standard **B+ tree** keyed by `(attr_id: u32, value: NormalisedKey, oid: u64)`. `NormalisedKey`
is a fixed-size order-preserving encoding:

| ValueType | Encoding (16 bytes)                                                                       |
| --------- | ----------------------------------------------------------------------------------------- |
| Int/Time  | `i64` flipped sign bit (so unsigned compare = signed compare), zero-padded.               |
| Float     | IEEE 754 with sign-bit-flipped trick: positives flip top bit, negatives flip all bits.    |
| Text      | First 14 bytes of UTF-8, length byte, continuation flag; long strings spill via `value_hash`. |
| Blob      | Stored only by hash; not range-indexed.                                                   |

Leaf values are roaring bitmaps (per `(attr_id, value_prefix)`), enabling cheap range scans.

### 9.3 Chunk Index (for FastCDC objects)

```
ChunkIndex (B+ tree, key = chunk_hash [u8; 32]):
   leaf entry: { chunk_hash, ref_count: u32, blob: BlobRef, length: u32 }
ChunkList per object: array of (chunk_hash, length) — referenced from ObjectLocation when chunked.
```

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
  modules_root: BlockRef     → B+ tree key = module_id_hash → CBOR(ModuleManifest)
  tags_root:    BlockRef     → B+ tree key = TagId          → TagDefRecord (fixed, 64 bytes)
  tag_names:    BlockRef     → B+ tree key = name_hash      → (TagId, BlockRef → CBOR(TagDef))
  dag_root:     BlockRef     → ImplicationDagPages (sparse adjacency lists)
```

`TagDefRecord` is 64 bytes with `name_offset` pointing into `tag_names`. The variable-shape parts
(`TagSemantics::OrderedCollection { element_constraint }`, future fields) live in CBOR via
`tag_names`. Hot path queries only touch the fixed records.

Modules ship as TOML, but their **on-disk** form is CBOR — the parser converts TOML → struct →
CBOR at install time. TOML is never seen by the read path.

### 10.2 Subscriptions

```
SubscriptionsRoot:
  B+ tree, key = SubscriptionId u64 → SubscriptionRecord (variable, CBOR)
```

A subscription's `cached_result` is a roaring bitmap stored in a `TagBitmap` (§8.2) referenced
from the record. Cursor (LSN), state, retention, debounce config, and the `Query` AST are all
inside the CBOR record — query trees are heterogeneous and infrequently rewritten, so CBOR
overhead is negligible.

### 10.3 Path contexts

```
PathContextRoot:
  B+ tree, key = name_hash → PathContextHeader { name_offset, manifest_root, _stats }
  Manifest is a B+ tree keyed by path-string-hash → ProjectedEntry (96 bytes inline + spill for
  Symlink targets and long paths).
```

Per-object reverse mappings (which object → which paths in which contexts) live in the forward
index as a special assertion kind, so listing all paths of an object is one forward-index hit.

### 10.4 Pool state

```
PoolStateRoot (4 KiB):
  disk_count: u32
  cluster_node_count: u32
  disks: [DiskDescriptorOnDisk; 64]    // fixed, 56 bytes each
  placement_rules_root: BlockRef       → B+ tree of CBOR rules (heterogeneous)
  cluster_peers_root:   BlockRef       → B+ tree key = NodeId → PeerRecord
```

`DiskDescriptorOnDisk` is fixed 56 bytes with `path_offset` into a string heap block.

---

## 11. Snapshots and Cluster Sync

Every committed `RootPointer` is itself a snapshot — by construction, all index roots inside it
form a self-consistent COW tree. Snapshots are made cheap to retain and diff:

### 11.1 Snapshot chain

```
SnapshotChainRoot (4 KiB):
   chain_length: u32
   newest_snapshot: BlockRef         → SnapshotRecord (linked list, newest-first)

SnapshotRecord (4 KiB):
   header
   seq: u64
   lsn: u64
   created_ns: i64
   label: [u8; 64]                   // optional human label
   root: RootPointer                  // 64 bytes
   parent: BlockRef                   // previous SnapshotRecord, or zero
   freed_blocks: BlockRef             // bitmap of blocks released *between* parent and this snapshot
   sync_metadata: BlockRef            // peer-watermarks, HLC, see below
```

A snapshot is created by:
1. Performing a normal checkpoint.
2. Linking the new `RootPointer` into the snapshot chain instead of (or in addition to)
   discarding the old root.

Freed-block tracking ensures GC of blocks reachable from no live snapshot. The allocator only
reclaims a block when `block_no` is in **none** of `[oldest_live_snapshot.freed_blocks, ...,
newest.freed_blocks]`'s union complement — a standard reference-tracked COW scheme.

### 11.2 Cluster diff between snapshots

Two snapshots `A` and `B` (with `A.lsn < B.lsn`) on the same node produce a delta:

1. Walk the COW trees of `A.root` and `B.root` in parallel. Identical `BlockRef.generation` →
   skip subtree (entire branch unchanged).
2. Differing branches recurse to the leaf level:
   - `ObjectTablePage` diff → list of (oid, new ObjectRecord).
   - `TagBitmap` diff → roaring `xor` produces added/removed bitmaps directly (Δsize ≪ |bitmap|).
   - `ForwardIndex` leaf diff → per-oid assertion delta.
3. Result is a `SyncBundle`:

```
SyncBundle (CBOR):
   from_lsn: u64
   to_lsn: u64
   from_node: NodeId
   ops: [SyncOp]                       // canonical, HLC-ordered
   bitmap_deltas: [{ tag_id, added: TagBitmap, removed: TagBitmap }]
   metadata_pages: [{ oid_range, records: [ObjectRecord] }]    // changed records only
   ontology_delta: Option<OntologyDelta>
```

The bundle is signed (per §9.4 key hierarchy in DESIGN.md) and shipped to peers. The bitmap
`xor`-based diff is what makes daily incremental sync ~850 KB regardless of pool size.

### 11.3 Snapshot retention policy

- Default: keep snapshots for **min(grace_period, 7 days)** to satisfy the deletion grace window
  (DESIGN §7.2).
- Configurable: pin snapshots by label for backups.
- Each retained snapshot costs the changed-page footprint between it and its successor — typically
  <1% of pool size per day on a normal workload.

### 11.4 Recovering to a previous snapshot

```
brunnr rollback --to <snapshot_label>
```

1. Verify the snapshot's `RootPointer` and all reachable BlockRefs validate (deep CRC check).
2. Mark all current-but-not-in-target pages as freed in a new checkpoint.
3. Atomically swap `active_root` in the superblock to the snapshot's root.

This is the same primitive that drives cluster-sync conflict resolution: a node can revert to a
known-good snapshot, then re-apply HLC-ordered ops from peers.

---

## 12. Allocation Layer

### 12.1 Allocation bitmap

One bit per 4 KiB block. Stored as a fixed array of `AllocBitmap` blocks (4096 bytes → 32 768 bits
→ 128 MiB of addressable space per block). For a 16 TiB device that's 4096 bitmap blocks = 16 MiB,
loaded into RAM at mount and dirtied via WAL.

```
AllocBitmap block:
   header (kind = AllocBitmap)
   bits: [u8; 4060]
   trailing CRC
```

Updates go through the WAL (`AllocDelta` op), then are flushed to bitmap blocks during checkpoint.
Bitmap blocks are themselves COW'd via the `pool_state_root` so a torn write doesn't lose the
allocation map.

### 12.2 Block class map

Nibble-packed `BlockClass` per block: `Free=0`, `Index=1`, `Metadata=2`, `Blob=3`,
`Wal=4`, `Reserved=5`. Stored adjacent to the allocation bitmap, same COW machinery. Used by the
zone-aware allocator and by fsck.

### 12.3 Allocator behavior

Per-zone first-fit with extent-size hints. The allocator favours:

- **Index zone**: 4-block extents (sufficient for most B+ tree pages; large bitmaps span more).
- **Metadata zone**: 1-block extents (single-page records).
- **Blob zone**: 64+ block extents (256 KiB minimum) to keep fragmentation manageable.

Rebalance / disk evacuation walks extents in `block_class_map` order so a draining disk's blobs
are migrated zone-by-zone.

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
| `OpLog` (`VecDeque<OpLogEntry>`)             | recent WAL tail                           | trimmed at checkpoint  |

`Engine` (DESIGN §15) owns these and is wrapped by `DiskEngine` which adds `FileBlockDevice`,
superblock, allocator, and snapshot manager. All mutations follow:

```
1. Acquire engine write lock
2. Append WAL entry (fsync if durability mode = sync)
3. Apply to in-memory mirror (idempotent on lsn)
4. Mark dirty pages for next checkpoint
5. Release write lock
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

## 16. Summary of On-Disk Footprint (10 M objects, 5 000 tags)

| Structure              | Size      | Notes                                              |
| ---------------------- | --------- | -------------------------------------------------- |
| Superblock × 3         | 12 KiB    | Fixed                                              |
| WAL                    | 64 MiB    | Configurable, mirrored                             |
| Allocation bitmap      | 16 MiB    | Per 16 TiB                                         |
| Block class map        | 8 MiB     | Nibble-packed, per 16 TiB                          |
| Object table (records) | 1.28 GiB  | 10 M × 128 B                                       |
| Object table (radix)   | <16 MiB   | Three indirection levels                           |
| Location table         | 480 MiB   | 10 M × 48 B                                        |
| Forward index          | ~600 MiB  | B+ tree, ~60 B/object                              |
| Tag inverted index     | 200–400 MiB | Roaring bitmaps, 5 000 tags                      |
| KV index               | ~100 MiB  | Extendible hash + bitmaps                          |
| Range index            | ~50 MiB   | B+ tree                                            |
| Ontology               | <10 MiB   | Modules + DAG                                      |
| Subscriptions          | ~1 MiB    | Per 1 000 subs                                     |
| Path contexts          | 50 MiB    | One large project                                  |
| Snapshot chain (7 days) | ~500 MiB | Diff-only                                          |
| **Total metadata**     | **~3 GiB** | Replicated to every node                          |

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
