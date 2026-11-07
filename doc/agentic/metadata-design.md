# Metadata tables — design

Scope: `crates/meta` (`ObjectTable`, `LocationTable`, `BackpointerTable`, radix helpers, leaf layouts).
Governing spec: METADATA-DESIGN§1 (object record table), METADATA-DESIGN§2 (location and backpointer tables), SPECIFICATION-DESIGN§6.2, SPECIFICATION-DESIGN§6.3, SPECIFICATION-DESIGN§7.

Plan: [metadata-implementation-plan.md](metadata-implementation-plan.md) · Open decisions: [metadata-decisions.md](metadata-decisions.md) · Completed: [metadata-completed.md](metadata-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Object records and locations are positional: `oid_local` addresses a slot in a copy-on-write radix tree of 256 KiB nodes, with no per-key search.

## Selected design

- **Object table leaf:** `BtreeNodeHeader` + 256 B occupancy bitmap + 32 B trailer + 2 044 × 128 B `ObjectRecord`.
- **Location table leaf:** same envelope, 5 440 × 48 B records.
- **Inner node:** `BtreeNodeHeader` + 16 380 × `BlockRef`; an empty child has generation 0.
- **Addressing:** `slot = oid_local % leaf_fanout`, `child[level] = (oid_local / leaf_fanout) % 16 380`, through the existing `radix.rs` helpers.
- **Growth:** when an `oid_local` overflows the current `root_level`, a new inner root points its first child at the old root.
- **Flush:** dirty leaves are written to fresh regions and every ancestor is rewritten (COW).
- **Wire form is the POD byte image** (`bytemuck`), never CBOR-wrapped.
- **Backpointer table** keys use the packed-key codec (BTREE-DESIGN§1.6).

## Invariants

- ObjectIds are never recycled (SPECIFICATION-DESIGN§7.3).
- A record read returns exactly the committed 128 B image.

## Object deletion

### Why ids are never recycled

An `ObjectId` is referenced from every posting list the object is in, the forward index, its object and location records, the chunk index, ordered-collection sequences, peers' copies of all of these, and sync ops still in flight. If id 42 were reused while a peer had not yet seen the deletion, tags meant for the new 42 would land on the old 42's state. A node's 48-bit local sequence holds 2⁴⁸ ≈ 2.8 × 10¹⁴ ids; at 1 000 creates per second that lasts about 8 900 years, so ids are never reused (SPECIFICATION-DESIGN§7.3). `ObjectRecord.generation` remains as insurance should reuse ever be introduced.

### Phases

Deletion walks `ObjectRecord.state` (`ObjectState`, discriminants pinned: `Active = 0`, `Tombstoned = 1`, `BlobReclaim = 2`, `Cleared = 3`, and `Writing = 4` for a stream not yet closed (JOURNAL-DESIGN§1.8), which deletion treats like `Active`) through the four phases of SPECIFICATION-DESIGN§7.1:

1. **Tombstone.** `state = Tombstoned`, `modified_ns` = deletion time; the `DeleteObject` WAL entry and oplog op are written and subscribers get `Deleted`. The object is invisible to queries from here on. O(1).
2. **Index cleanup.** The forward entry and the ontology's closure tables give exactly what to undo, so cost is O(terms on the object), not O(terms in the pool): the object leaves the postings of every term in its forward entry and of every tag those imply (one container each, INDEX-DESIGN§2.4, INDEX-DESIGN§3.1) and, for ordered or ranked collections, their sequences (INDEX-DESIGN§3.3); each term's dictionary statistics are updated; then the forward entry goes. Twenty terms are about 2 µs, so the work is not parallelised.
3. **Blob reclaim**, once every peer has seen the deletion (step 4's watermark condition): `state = BlobReclaim`; a direct blob's extents are freed through the allocator (ALLOCATION-DESIGN§1), a chunked blob's chunk references are decremented and chunks reaching zero are freed (INDEX-DESIGN§4.3), and the location record is cleared. Waiting protects a peer still hydrating the blob, and freeing in the same commit that clears the location keeps a crash from leaving a committed location that points at freed space.
4. **Tombstone expiry**, when every peer's sync watermark is past the deletion and the minimum grace period has elapsed (SPECIFICATION-DESIGN§7.2): `state = Cleared` and the rest of the record is zeroed. The slot stays allocated and the id is not reused; accumulation of cleared slots is [METADATA-D1](metadata-decisions.md).

When phase 2 runs is [METADATA-D3](metadata-decisions.md); phases 3 and 4 are [METADATA6](metadata-implementation-plan.md).

### Deletion and cluster sync

Ops are applied in HLC order, so a deletion and a concurrent mutation resolve deterministically (SPECIFICATION-DESIGN§7.4):

| Time | Event |
|---|---|
| 0 | A creates 42 and tags it `music` |
| 1 | B learns about 42 |
| 2 | A deletes 42 |
| 3 | B, not yet aware, tags 42 `favorite` |
| 4 | B receives the deletion |
| 5 | A receives B's tag op |

- A mutation op for a tombstoned object whose timestamp is **earlier** than the deletion was valid when issued: it is dropped as stale. Applied in HLC order it would have been undone by the deletion anyway.
- A mutation op **later** than the deletion (step 3 above) should not occur under correct HLC propagation; it is dropped and logged as a conflict naming the origin node.
- An op earlier than the deletion that is applied before the deletion arrives is simply undone by it: the tag existed briefly, then went with the object.

The full per-assertion conflict rules are [CLUSTER-D2](cluster-decisions.md).

### Deleting a tag

Deleting a tag is cheaper than deleting objects: its posting list is the member list. Each member's forward entry drops the tag's `TermId`, the postings and the dictionary entry go, the ontology drops the tag, and one sync op records the deletion. Member objects are untouched — deleting a playlist removes the grouping, not the music (SPECIFICATION-DESIGN§7.5). The per-object tag count is [METADATA-D2](metadata-decisions.md); removing a module's tags is the Purge policy (ONTOLOGY-DESIGN "Removal and scrub policies", [ontology-design.md](ontology-design.md)).

### Rejected alternatives

- **A `Deleted { deleted_at, tombstone_expires }` state variant.** The state is the pinned `u8` discriminant; the deletion time is `modified_ns`, and expiry is computed, not stored.
- **Offline object-table compaction that remaps ids**, with every bitmap, forward entry, sequence, relation and chunk reference rewritten and a cluster-wide epoch bump. Ids are never remapped; sparsity is [METADATA-D1](metadata-decisions.md).
- **Generation-tagged references (`ObjectRef { id, generation }`) in sync ops and collection sequences.** Without id reuse a bare `ObjectId` is unambiguous.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Object Record Table

Logically a flat array indexed by `ObjectId.local`. Physically, a **COW radix tree** of large
nodes (BTREE-DESIGN§1) whose depth grows with the populated id space.

### Node capacities

Each node is a 256 KiB region (BTREE-DESIGN§1) with a 64-byte `BtreeNodeHeader`. For the radix variants
there are no internal sorted runs — positional updates are journalled via JOURNAL-DESIGN§1.4 and merged on flush.

- **Leaf node** (`BtreeKind::ObjectTable`, level 0): **2044** × `ObjectRecord` (128 B). Layout:
  64 B header + 256 B occupancy bitmap (one bit per slot, ≥ 2044 bits) + 32 B trailer
  (generation, version, reserved) + 2044 × 128 B = **261 984 B used**, leaving 160 B trailing
  pad inside the 256 KiB region. The bitmap distinguishes "never allocated" from "cleared"
  slots (SPECIFICATION-DESIGN§7.3).
- **Inner node** (level ≥ 1): **16 380** × `BlockRef` (16 B) = 262 080 B = exactly 256 KiB −
  64 B header. No bitmap and no trailer: empty child slots are denoted by the
  `BlockRef.generation == 0` sentinel, which never matches a live bucket generation.

### Tree depth and capacity

> **Depth convention.** Throughout this document, *depth* counts the **inner levels** of a B+
> tree or radix tree above the leaves — equivalently, the maximum value of `BtreeNodeHeader.level`
> present in the tree. A **depth-0** tree is leaf-only (one node, `level = 0`); a **depth-*N***
> tree has *N* inner levels plus the leaf level, *N + 1* levels in total. `MAX_LEVELS = 3`
> bounds *inner* depth, so the deepest tree this format addresses is depth 3 (4 levels total),
> covering ≥ 2⁴⁸ objects. A read at depth *N* touches at most `N + 1` nodes; a flush at depth
> *N* rewrites at most `N + 1` nodes (`(N + 1) × node_size` bytes). The plural *levels* is the
> informal name for the total layer count when prose calls for it; the singular *level* refers
> to a specific layer, matching the `BtreeNodeHeader.level` field value.

| Depth (inner levels + leaf) | Max objects                              |
| --------------------------- | ---------------------------------------- |
| 0 inner (leaf only)         | 2 044                                    |
| 1 inner                     | 2 044 × 16 380 ≈ 33 M                    |
| 2 inner                     | 2 044 × 16 380² ≈ 548 G                  |
| 3 inner                     | 2 044 × 16 380³ ≈ 9 P                    |

A 10 M-object pool sits in a **single-inner-level tree** (root inner + leaves; depth 1). The
full 48-bit local-id space is reachable at depth 3 (≈ 9 P objects). The `level` field in
`BtreeNodeHeader` is a `u8`, but only 4 bits are used by convention, representing levels 0–15
which is *plenty* for all intents and purposes.

`RootPointer.object_table_root` is a `BlockRef` to the topmost node; the node's
`BtreeNodeHeader.level` identifies whether it is a leaf (very small pool) or an inner node.

### Address translation (oid → leaf slot)

```rust
// MAX_LEVELS bounds the inner-node depth above the leaf.
// BtreeNodeHeader.level is u8 (BTREE-DESIGN§1.1), so the absolute upper bound is 15;
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

Object IDs are allocated sequentially per node (SPECIFICATION-DESIGN§2.1), so populated leaves cluster densely
and the tree stays compact. A missing child pointer in any inner node marks an unallocated id
range; cleared slots are sparse and distinguished from "never allocated" by the leaf occupancy
bitmap (METADATA-DESIGN§1.1 / SPECIFICATION-DESIGN§7.3).

### Tree growth

When the root is full and a new id falls outside its range, a new inner node one level higher is
allocated, populated with the previous root as its first child, and committed as the new
`object_table_root` at the next checkpoint. Tree growth is log-amortised — never a wholesale
rewrite.

### COW write path

Nodes are rewritten copy-on-write **only when the journal-reclaim thread flushes them** (JOURNAL-DESIGN§1.4),
never per mutation. A flush at depth *D* costs roughly *(D + 1) × 256 KiB* of writes (one
rewrite per level, leaf included). For the 10 M-object pool (depth 1) that's **2 node
rewrites per flush** (leaf + root inner), amortised across however many mutations have
accumulated against that leaf since its last flush.

Per-mutation cost is still the WAL append (~80 B). Reads consult the in-memory `LoadedNode`
(BTREE-DESIGN§1.3), which holds the on-disk state plus pending journal entries past
`BtreeNodeHeader.last_persisted_lsn`; cold reads materialise the merged view at load time.

Because positional radix leaves don't use internal sorted runs, every flush rewrites the whole leaf
into a fresh region. This is acceptable here: a leaf holding 2044 records absorbs hundreds to
thousands of pending mutations before journal-reclaim chooses to flush it, so the per-mutation
amortised write cost is well under 1 KiB. (The B+ tree variants in INDEX-DESIGN§2+ avoid even this by
appending sorted runs — for keyed structures that's cheaper than rebuilding a leaf.)

### 1.1 ObjectRecord (128 bytes, version 1)

Identical to SPECIFICATION-DESIGN§6.2 but with explicit POD layout and a small generation-tracking header:

```
#[repr(C)]
struct ObjectRecord {                        // 128 bytes
    id: u64,                                 //  [0..8]
    generation: u32,                         //  [8..12]
    state: u8,                               //  [12..13]    ObjectState
    flags: u8,                               //  [13..14]    OBJECT_FLAG_*
    record_version: u16,                     //  [14..16]    structural version of THIS record
    content_hash: [u8; 32],                  //  [16..48]    BLAKE3 of plaintext
    blob_offset: u64,                        //  [48..56]
    blob_length: u64,                        //  [56..64]
    created_ns: i64,                         //  [64..72]
    modified_ns: i64,                        //  [72..80]
    tag_count: u16,                          //  [80..82]    total tags on this object (object-wide)
    attr_count: u16,                         //  [82..84]    total attrs on this object (object-wide)
    compression: u8,                         //  [84..85]
    encryption: u8,                          //  [85..86]
    relation_count: u16,                     //  [86..88]    total relations on this object (object-wide)
    inline_tags: [u32; 4],                   //  [88..104]   inline tag IDs; valid iff !(flags & OBJECT_FLAG_HAS_OVERFLOW)
    overflow_offset: u64,                    // [104..112]   block_no in metadata zone
    stored_size: u64,                        // [112..120]
    last_modify_lsn: u64,                    // [120..128]   for snapshot diffing
}

// ObjectRecord.flags bits
const OBJECT_FLAG_HAS_OVERFLOW: u8 = 1 << 0;  // tags/attrs/relations spilled to OverflowRecord
const OBJECT_FLAG_CHUNKED:      u8 = 1 << 1;  // blob is FastCDC-chunked (see INDEX-DESIGN§4.3)
```

`record_version` in addition to `BlockHeader.format_version` lets a single page mix old and new
records when format changes; the page is rewritten to homogenise on the next checkpoint touching
it.

### 1.2 Overflow records (when tags > 4 or attrs > 0)

For objects with more than 4 tags, any attributes, or any relations, a separate
**OverflowRecord** lives in the metadata zone, addressed by `overflow_offset`.
`OBJECT_FLAG_HAS_OVERFLOW` is set in `ObjectRecord.flags`; while it's set, `inline_tags` is
**ignored** and **all** tags + attrs + relations for the object live in the overflow chain
(not split between inline and overflow). When the flag is clear, both `attr_count` and
`relation_count` are guaranteed `0` (no inline storage exists for either — only the four
`inline_tags` slots, gated by `tag_count ≤ 4`). The object-wide totals stay in
`ObjectRecord.{tag,attr,relation}_count` (each capped at u16 max ≈ 65 K — beyond that, switch
to the per-object B+ tree escape hatch).

```
struct OverflowRecord {                      // 4096 bytes
    header: BlockHeader,                     // [0..32]   kind = OverflowRecord (FORMAT-DESIGN§1.3)
    object_id: u64,                          // [32..40]
    tag_count: u16,                          // [40..42]  count IN THIS BLOCK only
    attr_count: u16,                         // [42..44]  count IN THIS BLOCK only
    relation_count: u16,                     // [44..46]  count IN THIS BLOCK only
    _pad: u16,                               // [46..48]
    next_overflow: u64,                      // [48..56]  block_no of the next overflow block,
                                             //           0 if last
    // [56..4092] = 4036 B variable payload, written in order with NO inter-element padding
    // (each section is byte-packed; readers advance the cursor by the consumed bytes):
    //   tag_count × u32                                  (extra tag IDs)
    //   attr_count × OverflowAttr                        (variable: 32 B spilled, 112 B inline)
    //   relation_count × { predicate: u32, target: u64 } (12 B each, packed)
    // [4092..4096] trailing CRC32C inside BlockHeader's frame
}

#[repr(C)]
struct OverflowAttr {                        // 32 bytes (spill) or 112 bytes (inline)
    flags: u8,                               // [0..1]   OVERFLOW_ATTR_FLAG_*
    _pad0: [u8; 3],                          // [1..4]   align `key` to u32
    key: u32,                                // [4..8]   attribute id
    term_id: u64,                            // [8..16]  the attribute term, INDEX-DESIGN§1.2
    // [16..) — body, discriminated by `flags & OVERFLOW_ATTR_FLAG_SPILL`:
    //   spilled (flag set):    spill: BlobRef               (16 B; total record = 32 B)
    //   inline (flag clear):   inline_value: [u8; 96]       (96 B; total record = 112 B)
}

// OverflowAttr.flags bits
const OVERFLOW_ATTR_FLAG_SPILL: u8 = 1 << 0;  // body is 16 B BlobRef, not 96 B inline value
```

`OverflowAttr.flags` is the discriminator that tells a reader how many bytes to consume for the
body — without it the variant is unrecoverable. Other `flags` bits are reserved for future per-attr metadata (e.g. compression
hints) and zeroed in `format_version = 1`.

If an object outgrows a single 4 KiB overflow record, the chain extends through `next_overflow`
(equivalent to setting `BLOCK_FLAG_CONTINUATION` in the head's `BlockHeader.flags`). The widths
for `tag_count` / `attr_count` / `relation_count` here are deliberately u16 — they record only
the **per-block** count, never the object-wide total — and a single 4 KiB block cannot hold
anywhere near 65 K of any of them. The owner's u16 totals therefore never need to be
reconciled across blocks: each block's u16 is a lower bound that the reader sums during
traversal.

For pathological cases (≥ 200 tags), switching to a per-object B+ tree is more efficient and
is the planned escape hatch.

---

## 2. Location and Backpointer Tables

Two complementary structures translate between logical objects and physical extents:

- **Location table** (METADATA-DESIGN§2.1) — forward mapping `oid → physical extent`. Used on every read.
- **Backpointers** (METADATA-DESIGN§2.2) — reverse mapping `(disk_id, bucket_no, sector_offset) → owning key`.
  Used by copygc, device evacuation, scrub, resilver, and cluster reconcile. Without
  backpointers these operations cost O(pool size); with them, O(data-on-affected-bucket).

### 2.1 Location Table (forward mapping)

Same COW radix-tree machinery as the object table (METADATA-DESIGN§1), parameterised for 48-byte
`ObjectLocation` records and using the same large-node format (BTREE-DESIGN§1):

- **Leaf node** (`BtreeKind::LocationTable`, level 0): **5440** × `ObjectLocation` (48 B).
  Layout: 64 B header + 680 B occupancy bitmap (5 440 bits exactly, no wastage) + 32 B trailer
  + 5 440 × 48 B = **261 896 B used**, leaving 248 B trailing pad inside the 256 KiB region.
- **Inner node**: identical to METADATA-DESIGN§1's inner — 16 380 × `BlockRef`, empty slots sentineled by
  `BlockRef.generation == 0`.

Capacity:

| Depth | Max objects                              |
| ----- | ---------------------------------------- |
| 0     | 5 440                                    |
| 1     | 5 440 × 16 380 ≈ 89 M                    |
| 2     | 5 440 × 16 380² ≈ 1.46 T                 |
| 3     | 5 440 × 16 380³ ≈ 23.9 P                 |

A 10 M-object pool fits in **depth 1** (single inner node + leaves). The 48-bit local id space
(≈ 281 T) is reachable at depth 3 (capacity ≈ 23.9 P, comfortably above 2⁴⁸). `MAX_LEVELS = 3`
in METADATA-DESIGN§1 bounds both the object and location radix trees.

The radix is keyed by the same `ObjectId.local`, so the location table tracks the object table
slot-for-slot. A flush at depth 1 costs **2 node rewrites** at 10 M scale (leaf + root inner),
amortised across pending mutations.

```rust
#[repr(C, align(8))]
struct ObjectLocation {                      // 48 bytes
    flags: u8,                               //  [0..1]    LOCATION_FLAG_*
    replica_count: u8,                       //  [1..2]    1..=4; total physical copies
    _pad: [u8; 6],                           //  [2..8]    align to u64
    extent_length: u64,                      //  [8..16]   non-chunked: extent length in bytes
                                             //           chunked:     plaintext logical length
    replicas: [ReplicaRef; 4],               // [16..48]   4 × 8 B; slots [replica_count..4]
                                             //           are zeroed and ignored
}

// ObjectLocation.flags bits
const LOCATION_FLAG_CHUNKED:     u8 = 1 << 0;  // replicas[] are ChunkList region copies (INDEX-DESIGN§4.3)
const LOCATION_FLAG_REMOTE_ONLY: u8 = 1 << 1;  // no local replica; reads must go cross-node

#[repr(C)]
struct ReplicaRef {                          // 8 bytes
    disk_id: u16,                            // [0..2]
    sector_offset: u16,                      // [2..4]   4 KiB sector within the bucket
    bucket_no: u32,                          // [4..8]   bucket within the disk
}
```

**Symmetric replicas.** Every physical copy of an extent lives in `replicas[i]` for some
`i ∈ [0, replica_count)`; there is no distinguished "primary" location. `replicas[0]` is the
read-preferred copy by convention, but the others are equally authoritative. Empty inline
slots (when `replica_count < 4`) are zeroed and ignored.

**`replica_count` is the ground-truth cardinality.** It records the **total number of
physical copies** of the extent, all of which are visible inline. This makes consistency
checking exact: a scrub or verifier can enumerate every replica of an oid by reading
`replicas[0..replica_count]`, look up each replica's backpointer at its `(disk_id,
bucket_no, sector_offset)`, and assert that exactly `replica_count` backpointers match the
oid as their `owner_key`. Any mismatch — extra backpointers (orphan replicas) or missing
backpointers (lost replicas) — is a corruption signal.

**Cap of 4 inline.** Since the consistency invariant requires every replica's address to be
recoverable from `ObjectLocation` alone (the backpointer btree is keyed by physical
location, not by oid — there is no efficient "find every backpointer for oid X" path), the
maximum replication factor representable here is bounded by the inline array size: **4**.
Workloads that need more durability than 4-way replication use erasure coding (RECONCILE-DESIGN§1,
`WorkKind::EcEncode`), which is a separate code path and does not share this struct.

`ReplicaRef` is **bucket-relative**, mirroring the layout of `BackpointerKey` (METADATA-DESIGN§2.2) so that
the move path (RECONCILE-DESIGN§1.5), scrub, resilver, and copygc can share field-level conversions instead
of arithmetic on absolute block numbers. The reachable extent space per disk is
`2^32 buckets × bucket_size`: 4 PiB at the default 1 MiB bucket, 16 PiB at the maximum 4 MiB
bucket. `sector_offset: u16` admits up to 64 K sectors per bucket, which covers any
`bucket_size ≤ 256 MiB` (the format caps bucket size at 4 MiB / 1024 sectors).

Conversion to/from absolute `block_no` (when interfacing with `BlockRef`):

```
bucket_no     = block_no >> (bucket_size_log2 - 12)
sector_offset = block_no & ((1 << (bucket_size_log2 - 12)) - 1)
```

**Chunked objects** (`flags & LOCATION_FLAG_CHUNKED`) reinterpret the meaning of what
`replicas[]` points at, not the layout:

- `replicas[0..replica_count]` describe replicas of the head **`ChunkList` region** (a BTREE-DESIGN§1
  positional region; see INDEX-DESIGN§4.3), not the user data. The chunks themselves are
  content-addressed and replicated independently via the chunk index — their replication
  factor is governed by chunk-level placement, not by `ObjectLocation.replica_count`.
- `extent_length` is the **plaintext logical length** of the object (the sum of all chunk
  plaintext lengths), used to size read buffers and bound chunk iteration; it matches
  `ObjectRecord.blob_length`.

This keeps `ObjectLocation` a single fixed-size 48 B record regardless of chunked /
non-chunked, preserving the radix-leaf positional layout.

### 2.2 Backpointers (reverse mapping)

A single global B+ tree of large nodes (BTREE-DESIGN§1), `BtreeKind::Backpointer`, keyed by physical
location:

```rust
#[repr(C, packed)]
struct BackpointerKey {                      // 8 bytes (packed via BTREE-DESIGN§1.6 to ~3-4 B per leaf)
    disk_id: u16,
    bucket_no: u32,
    sector_offset: u16,                      // 4 KiB units within the bucket; u16 covers
                                             // bucket_size ≤ 256 MiB (format caps at 4 MiB)
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
    // 4 unassigned (posting containers live in B+ tree nodes, covered by BtreeNode)
    OverflowRecord   = 5,                    // owner_key = ObjectId (u64)
}
```

**`owner_key` padding.** Variants whose logical key is shorter than 16 bytes
(`OverflowRecord` uses 8; `BtreeNode` may use less, depending on
the `min_key prefix` length) must **zero the trailing bytes** of `owner_key`. Equality
of two `BackpointerValue` records is byte-wise over the full 16 B, and replay of a
`ReconcileMove` (JOURNAL-DESIGN§1.3, RECONCILE-DESIGN§1.5) decomposes into `BackpointerRemove(old_key) ∘
BackpointerInsert(new_key) ∘ owner-update` on exact-match keys — non-zero padding would
silently break that match. Writers fill the logical bytes per the `OwnerKind` table
above and zero the rest; readers ignore bytes past the kind-specific length but must
treat the full 16 B as canonical for equality and BTREE-DESIGN§1.6 packing.

The pair `(BackpointerKey, BackpointerValue)` is 32 bytes unpacked; with BTREE-DESIGN§1.6 key packing
(`disk_id` constant per leaf, `bucket_no` packs to ~16–20 bits, `sector_offset` packs based on
bucket size), per-key disk cost falls to **~26–28 B**. A 256 KiB leaf packs ~9 000 backpointers
per sorted run.

#### Properties

- **Bucket-prefix scan.** "What lives in `(disk_id, bucket_no)`?" is a B+ tree range scan over
  `(disk_id, bucket_no, *)`. With key packing the entire bucket's backpointers typically sit in
  one or two contiguous leaf sorted runs — a single large-node load.
- **Generation gating.** `BackpointerValue.bucket_gen` records the bucket's generation at
  insertion. A backpointer whose `bucket_gen` does not match the current bucket generation is
  **stale** (the bucket has been recycled) and is dropped lazily on the next scrub or copygc
  pass. This means we don't have to atomically delete backpointers when freeing extents — a
  generation bump invalidates all of a bucket's backpointers in O(1).

#### Lifecycle

- **Insert** on every blob/chunk/btree-node write. Journalled as `BackpointerInsert` (JOURNAL-DESIGN§1.3).
- **Remove** on object deletion or extent rewrite. Journalled as `BackpointerRemove`. May be
  elided when the bucket's generation will be bumped (lazy invalidation).
- **Update** on copygc / reconcile move. The move path emits a single `ReconcileMove` WAL op
  (JOURNAL-DESIGN§1.3, RECONCILE-DESIGN§1.5); replay projects it into the equivalent `BackpointerRemove(old) ∘
  BackpointerInsert(new) ∘ owner-update` sequence and applies all three under one LSN. This
  keeps moves single-LSN atomic without duplicating the primitive ops, which remain available
  for non-move callers (object deletion, fresh writes, lazy btree-node free).

#### Operations enabled

| Operation              | Without backpointers     | With backpointers              |
| ---------------------- | ------------------------ | ------------------------------ |
| Copygc bucket reclaim  | Scan all forward indexes | Range scan one bucket prefix   |
| Disk evacuation        | Scan location table      | Range scan all of disk's buckets |
| Scrub bucket           | Scan all forward indexes | Range scan bucket prefix       |
| Cluster resilver       | Replay full sync log     | Backpointer-driven replay of affected buckets only |
| Stale-pointer cleanup  | Track-during-write       | Lazy via generation comparison |

#### Footprint

Per 10 M objects with average 1 extent each: 10 M backpointers × ~28 B packed ≈ **267 MiB**.
Plus a few thousand btree-node backpointers (~100 KiB) and ~5 000 tag-bitmap backpointers
(~140 KiB). Negligible compared to existing per-object metadata.

---

