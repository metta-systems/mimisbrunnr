# Metadata tables — design

Scope: `crates/meta` (`ObjectTable`, `ObjectRoots`, extent lists, `BackpointerTable`, radix helpers, leaf layouts).
Governing spec: METADATA-DESIGN§1 (object table), METADATA-DESIGN§2 (replicas and backpointers), SPECIFICATION-DESIGN§6.2, SPECIFICATION-DESIGN§6.3, SPECIFICATION-DESIGN§7.

Plan: [metadata-implementation-plan.md](metadata-implementation-plan.md) · Open decisions: [metadata-decisions.md](metadata-decisions.md) · Completed: [metadata-completed.md](metadata-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

One fixed-size record per object says what the object is and where its content lives. Records are positional: an `ObjectId` addresses a slot in a copy-on-write radix tree of large nodes, with no per-key search.

## Selected design

- **One record per object, content location included.** `ObjectRecord` (128 B) holds state, lengths, content hash, timestamps, the transform applied and a 40-byte **content locator** whose shape depends on how the content is stored: one extent with its replicas, an extent list, a chunk list, a packed small-object slot, the content itself when it is at most 40 bytes, or a remote origin (METADATA-DESIGN§1.2). There is no separate location table: a read is one record lookup, a flush rewrites one tree, and a snapshot has one sidecar.
- **No assertions in the record.** An object's tags, attributes and relations live only in its forward entry (INDEX-DESIGN§2); the record carries no tag ids, counts or overflow chains.
- **One radix per origin node.** `ObjectId`s carry the creating node in their top 16 bits and every node holds every object's metadata, so records are kept in one radix tree per origin node, indexed by the id's local part. The `ObjectRoots` tree maps a `NodeId` to that radix's root, depth and id high-water (METADATA-DESIGN§1.4). Each node's ids are sequential, so each radix stays dense.
- **Radix shape.** Nodes are positional page regions (BTREE-DESIGN§1.7). Leaf: 1 536 × 128 B records (32 per page); a zero record was never allocated. Inner node: 12 288 / R `NodePtr` slots; an empty child is zero. `slot = local % 1536`, `child[level] = (local / 1536) % (12 288 / R)`.
- **Growth and flush.** An id beyond the root's range adds a new root one level up with the old root as its first child; a flush appends a dirty leaf's changed pages, and a leaf moved to a fresh region at compaction updates its parent's slot the same way.
- **Cleared leaves are dropped.** Below a node's id high-water, an empty child means every record under it is `Cleared`; a leaf whose records are all `Cleared` is replaced by an empty child at its next flush, so ids are never reused and deleted history costs nothing.
- **Wire form is the POD byte image** (`bytemuck`), never CBOR-wrapped; every record carries `record_version` so production pools migrate record by record (FORMAT-DESIGN§15).
- **Backpointer table** keys use the packed-key codec (BTREE-DESIGN§1.6).

## Invariants

- ObjectIds are never recycled (SPECIFICATION-DESIGN§7.3); each node's id high-water in `ObjectRoots` only grows.
- A record read returns exactly the committed 128 B image.
- Every physical extent or region an object's content uses is reachable from its content locator, and each has exactly one backpointer per replica (METADATA-DESIGN§2).

## Object deletion

### Why ids are never recycled

An `ObjectId` is referenced from every posting list the object is in, the forward index, its record, the chunk index, ordered-collection sequences, peers' copies of all of these, and sync ops still in flight. If id 42 were reused while a peer had not yet seen the deletion, tags meant for the new 42 would land on the old 42's state. A node's 48-bit local sequence holds 2⁴⁸ ≈ 2.8 × 10¹⁴ ids; at 1 000 creates per second that lasts about 8 900 years, so ids are never reused (SPECIFICATION-DESIGN§7.3).

### Phases

Deletion walks `ObjectRecord.state` (`ObjectState`, discriminants pinned: `Active = 0`, `Tombstoned = 1`, `BlobReclaim = 2`, `Cleared = 3`, and `Writing = 4` for a stream not yet closed (JOURNAL-DESIGN§1.8), which deletion treats like `Active`) through the four phases of SPECIFICATION-DESIGN§7.1:

1. **Tombstone.** `state = Tombstoned`, `state_ns` = deletion time; the `DeleteObject` WAL entry and oplog op are written and subscribers get `Deleted`. The object is invisible to queries from here on. O(1).
2. **Index cleanup.** The forward entry and the ontology's closure tables give exactly what to undo, so cost is O(terms on the object), not O(terms in the pool): the object leaves the postings of every term in its forward entry and of every term those derive (one container each, INDEX-DESIGN§2.4, INDEX-DESIGN§3.1) and, for ordered or ranked collections, their sequences (INDEX-DESIGN§3.3); each term's dictionary statistics are updated; then the forward entry goes. Twenty terms are about 2 µs, so the work is not parallelised.
3. **Blob reclaim**, once every peer has seen the deletion (step 4's watermark condition): `state = BlobReclaim`; the extents, extent-list regions or packed slot the locator names are freed through the allocator (ALLOCATION-DESIGN§1), a chunked blob's chunk references are decremented and chunks reaching zero are freed (INDEX-DESIGN§4.3), and the locator is cleared. Waiting protects a peer still hydrating the blob, and freeing in the same commit that clears the locator keeps a crash from leaving a committed locator that points at freed space.
4. **Tombstone expiry**, when every peer's sync watermark is past the deletion and the minimum grace period has elapsed (SPECIFICATION-DESIGN§7.2): `state = Cleared` and the rest of the record is zeroed. The id is not reused; a leaf left holding only cleared records is dropped (METADATA-DESIGN§1).

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

Deleting a tag is cheaper than deleting objects: its posting list is the member list. Each member's forward entry drops the tag's `TermId`, the postings and the dictionary entry go, the ontology drops the tag, and one sync op records the deletion. Member records are untouched — deleting a playlist removes the grouping, not the music (SPECIFICATION-DESIGN§7.5). Removing a module's tags is the Purge policy (ONTOLOGY-DESIGN "Removal and scrub policies", [ontology-design.md](ontology-design.md)).

### Rejected alternatives

- **Assertions in the object record** (inline tag ids, per-object counts, 4 KiB overflow chains). They duplicated the forward entry, cost a 4 KiB block per object with any attribute, and made every tag change rewrite the record.
- **A separate location table** beside the object table, indexed by the same id. It doubled descents, flushes and snapshot sidecars for no independent access pattern, and its one-extent, sector-granular record could not describe multi-extent, packed or remote content.
- **One radix indexed by the local id alone.** Objects from different origin nodes with the same local id collide.
- **A `generation` field for id reuse.** Ids are never reused.
- **A `Deleted { deleted_at, tombstone_expires }` state variant.** The state is the pinned `u8` discriminant; the deletion time is `state_ns`, and expiry is computed, not stored.
- **Offline object-table compaction that remaps ids**, with every posting, forward entry, sequence, relation and chunk reference rewritten and a cluster-wide epoch bump. Ids are never remapped; cleared leaves are dropped instead.
- **Generation-tagged references (`ObjectRef { id, generation }`) in sync ops and collection sequences.** Without id reuse a bare `ObjectId` is unambiguous.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Object Table

Logically one flat array per origin node, indexed by `ObjectId.local`. Physically, one **COW
radix tree** of large nodes (BTREE-DESIGN§1) per origin node, whose depth grows with that node's
populated id space; `ObjectRoots` (METADATA-DESIGN§1.4) holds the roots.

### Node capacities

Each node is a 256 KiB positional region (BTREE-DESIGN§1.7) of `BtreeKind::ObjectTable` with 48
logical 4 KiB pages; positional updates are journalled via JOURNAL-DESIGN§1.4 and a flush
appends only the changed pages. The kind header in block 0 holds the node's first local id
(`u64`) and origin `NodeId` (`u16`), so a reader can check that a node belongs where the descent
found it.

- **Leaf node** (level 0): 32 `ObjectRecord`s (128 B) per page, **1 536** per leaf. A slot whose
  record is all zero (`record_version == 0`) was never allocated; every written record has
  `record_version ≥ 1`, cleared records included.
- **Inner node** (level ≥ 1): `NodePtr` slots of 16 × R bytes (R = `metadata_replicas`,
  ENGINE-DESIGN§1.2), 256 / R per page, **12 288 / R** per node. An empty child slot is all zero
  (generation 0 never matches a live bucket generation).

### Tree depth and capacity

> **Depth convention.** Throughout this document, *depth* counts the **inner levels** of a B+
> tree or radix tree above the leaves — equivalently, the maximum value of `BtreeNodeHeader.level`
> present in the tree. A **depth-0** tree is leaf-only (one node, `level = 0`); a **depth-*N***
> tree has *N* inner levels plus the leaf level, *N + 1* levels in total. `MAX_LEVELS = 4`
> bounds *inner* depth, so the deepest tree this format addresses is depth 4 (5 levels total),
> covering ≥ 2⁴⁸ objects for every R. A read at depth *N* touches at most `N + 1` nodes; a flush
> at depth *N* appends a page run to at most `N + 1` nodes. The plural *levels* is the
> informal name for the total layer count when prose calls for it; the singular *level* refers
> to a specific layer, matching the `BtreeNodeHeader.level` field value.

| Depth (inner levels + leaf) | Max objects per origin node, R = 1 | R = 2               |
| --------------------------- | ---------------------------------- | ------------------- |
| 0 inner (leaf only)         | 1 536                              | 1 536               |
| 1 inner                     | 1 536 × 12 288 ≈ 18.9 M            | 1 536 × 6 144 ≈ 9.4 M |
| 2 inner                     | ≈ 232 G                            | ≈ 58 G              |
| 3 inner                     | ≈ 2.8 P                            | ≈ 356 T             |

A node that has created 10 M objects has a **single-inner-level radix** at R = 1 (root inner +
6 511 leaves; depth 1) and depth 2 at R = 2. The full 48-bit local-id space (≈ 281 T) is
reachable at depth 3 for R ≤ 2 and at depth 4 for R ≤ 4.

### Address translation (oid → leaf slot)

```rust
// MAX_LEVELS bounds the inner-node depth above the leaf.
const MAX_LEVELS: usize = 4;
const LEAF_RECORDS: u64 = 1536;
let inner_slots: u64 = 12_288 / metadata_replicas;   // Superblock.metadata_replicas

let entry = object_roots.get(oid.node())?;        // METADATA-DESIGN§1.4
let mut idx = oid.local();
let leaf_slot  = (idx % LEAF_RECORDS) as u16; idx /= LEAF_RECORDS;
let mut child_path = [0u16; MAX_LEVELS];
for level in 0..entry.root_level as usize {
    child_path[level] = (idx % inner_slots) as u16;
    idx /= inner_slots;
}
// idx must be zero now; non-zero means the id exceeds the radix's addressable range
// (the writer grows the radix first — "Tree growth").
debug_assert_eq!(idx, 0);
```

A missing child pointer (or a missing `ObjectRoots` entry) on the descent means:

- id local ≥ `entry.next_local`: never allocated;
- id local < `entry.next_local`: every record under that child is `Cleared`.

### Tree growth

When a new id falls outside the root's range, a new inner node one level higher is allocated,
populated with the previous root as its first child, and committed as the node's new root in
`ObjectRoots` at the next checkpoint. Growth is log-amortised — never a wholesale rewrite.

### Cleared leaves

At flush, a leaf whose every allocated slot is `Cleared` is not written; its parent's child slot
becomes empty, and an inner node left with no children is dropped the same way. Reads of ids
under it return a `Cleared` record synthesised from the id. A cleared leaf is never needed
again: ids are not reused, so nothing is ever written under it.

### COW write path

Nodes are written **only when the journal-reclaim thread flushes them** (JOURNAL-DESIGN§1.4),
never per mutation. A flush appends a page run holding the leaf's changed pages — 8 KiB for one
dirty page — and, when the leaf moves to a fresh region at compaction, a page run to its parent
for the changed slot (BTREE-DESIGN§1.7). Assertion changes do not touch the record (they change
the forward entry and postings), so record leaves are dirtied by creation, content writes, state
changes and replica moves only.

Per-mutation cost is the journal entry. Reads consult the in-memory `LoadedNode`
(BTREE-DESIGN§1.3), which holds the on-disk pages plus pending journal entries.

### 1.1 ObjectRecord (128 bytes)

```rust
#[repr(C, align(8))]
struct ObjectRecord {                        // 128 bytes
    state: u8,                               //   [0..1]    ObjectState
    flags: u8,                               //   [1..2]    reserved, zero
    content_kind: u8,                        //   [2..3]    ContentKind (METADATA-DESIGN§1.2)
    replica_count: u8,                       //   [3..4]    copies named by the locator, 0..=4
    compression: u8,                         //   [4..5]    CompressionState (SPECIFICATION-DESIGN§9.2)
    encryption: u8,                          //   [5..6]    EncryptionState (SPECIFICATION-DESIGN§9.3)
    record_version: u16,                     //   [6..8]    layout version of THIS record (FORMAT-DESIGN§15)
    length: u64,                             //   [8..16]   plaintext length; durable length while Writing
    stored_length: u64,                      //  [16..24]   bytes on disk after transforms, one replica
    content_hash: [u8; 32],                  //  [24..56]   BLAKE3 of plaintext; zero while Writing
    created_ns: i64,                         //  [56..64]
    modified_ns: i64,                        //  [64..72]   last payload change (content written,
                                             //             extended or replaced)
    state_ns: i64,                           //  [72..80]   last ObjectState change (tombstone time)
    _reserved: [u8; 8],                      //  [80..88]   zero
    locator: [u8; 40],                       //  [88..128]  ContentLocator, by content_kind
}
```

**Timestamps.** `modified_ns` is the payload's change time, like a Unix mtime: only content
writes move it. Changes to the object's description — tags, attributes, relations — are timed
with the terms themselves, in its forward entry's `modified_ns` (INDEX-DESIGN§2.1), so tagging
never dirties the record. A Unix ctime is the later of the two (and of `state_ns`).

`record_version` is `1`. A reader decodes every record version since the format was declared
stable and upgrades a record when it next rewrites its leaf; `BtreeNodeHeader.format_version`
versions the leaf layout around the records (FORMAT-DESIGN§15.6).

### 1.2 Content locator

`content_kind` selects how the 40-byte `locator` is read. Each variant is a `#[repr(C)]` POD of at
most 40 bytes and alignment ≤ 8, read in place from the record (the locator starts at an
8-aligned offset); unused bytes are zero.

```rust
#[repr(u8)]
enum ContentKind {
    None       = 0,   // no content yet (new object, or a stream before its first extent)
    Extent     = 1,   // one extent
    ExtentList = 2,   // several extents, listed in an extent-list region (METADATA-DESIGN§1.3)
    ChunkList  = 3,   // FastCDC chunks (INDEX-DESIGN§4.3)
    Packed     = 4,   // a slot in a small-object bucket (BLOB-D4)
    Inline     = 5,   // the content itself, at most 40 bytes, in the locator
    Remote     = 6,   // content held by other nodes only (CLUSTER-D3)
}

#[repr(C)]
struct ExtentLocator {                       // content_kind = Extent
    replicas: [ReplicaRef; 4],               //  [0..32]   copies of the extent, [replica_count..4] zero
    _reserved: [u8; 8],                      // [32..40]
}

#[repr(C)]
struct ListLocator {                         // content_kind = ExtentList or ChunkList
    newest: [ReplicaRef; 4],                 //  [0..32]   copies of the list's newest region
    entry_count: u64,                        // [32..40]   extents or chunks in the whole list
}

#[repr(C)]
struct RemoteLocator {                       // content_kind = Remote
    origin_node: u16,                        //  [0..2]    node to fetch from
    _reserved: [u8; 38],                     //  [2..40]   hydration state (CLUSTER-D3)
}
```

- **Extent** — `stored_length` bytes from the first sector of each replica. An object whose
  stored form fits one extent (at most one bucket) uses this.
- **ExtentList** — larger non-chunked objects and streams: the list's regions hold the extents in
  order (METADATA-DESIGN§1.3); `newest` names the replicas of its newest region, from which the
  chain links back (BTREE-DESIGN§1.7).
- **ChunkList** — `newest` names the replicas of the newest `ChunkList` region (INDEX-DESIGN§4.3); the
  chunks are content-addressed and replicated by chunk-level placement, not by `replica_count`.
- **Packed** — the object's bytes are one slot of a small-object block; the locator's layout is
  set by BLOB-D4 and names up to 40 bytes of slot address across its replicas.
- **Inline** — objects of at most 40 bytes: the locator holds the bytes themselves,
  `length = stored_length ≤ 40`, `compression = None`, `replica_count = 0`. Such an object
  uses no blob space, packed slot or backpointer, is read with its record and replicates with
  the record leaf. Its bytes are protected as the record is (SPECIFICATION-DESIGN§9.3), so an
  object whose resolved storage policy sets an encryption mode of its own (SPECIFICATION-DESIGN§3.5)
  is never stored inline; it takes a packed slot or an extent, encrypted as its policy says.
  When an inline object's resolved policy gains an encryption mode, the content moves out:
  - an assertion change on the object (a tag whose policy sets encryption, directly or derived)
    moves it in the same commit as the assertion;
  - an ontology or placement-rule change that gives existing objects an encryption mode moves
    them through reconcile (RECONCILE-DESIGN§1.1), in the commit that installs the change for
    objects found then, and by scan for the rest.

**Residual copies after a move out of the record.** Moving inline content out changes where the
current version lives; it does **not** erase earlier copies of the bytes. Until each is
reclaimed, the bytes stay readable, under the protection of the structure that holds them and
not under the object's own encryption policy, in:

| Copy | Held until | Protection |
|---|---|---|
| Record leaf versions written before the move (copy-on-write leaves) | their bucket is reused and overwritten; freeing does not overwrite | node buckets: XTS-AES-256 under the disk key when the pool is encrypted (ENCRYPTION-DESIGN§1), plaintext otherwise |
| Journal sets whose entries carried the bytes (`WriteInline`, the record's put) | the journal trims past them, which retention (JOURNAL-DESIGN§1.6) can delay indefinitely, and their buckets are reused | journal: AES-256-GCM under the journal key when encrypted, plaintext otherwise |
| `ObjectHistory` records of snapshots taken before the move (SNAPSHOTS-DESIGN§1.2) | every such snapshot is deleted and its cleanup scan has run | node buckets, as above |
| Peers' copies of the record (cluster sync) | each peer moves its copy out and reclaims as above | each peer's own pool protection |

No step overwrites or scrubs these copies, so an object that must never have existed outside
its own encryption has to carry that policy from its first write.
- **Remote** — no local replica; reads go to `origin_node` or a peer holding it.

A write of at most 40 bytes stores `Inline` unless its policy sets an encryption mode; a rewrite that grows past 40 bytes moves the
object to `Packed` or `Extent`, and one that shrinks to 40 bytes or less moves it back,
freeing what the old locator named. A stream (`state = Writing`) starts as `None`, becomes `Extent` with its first extent and
`ExtentList` with its second; `length` is the durable prefix (JOURNAL-DESIGN§1.8).

### 1.3 Extent lists

An extent list is a backward-linked chain of positional regions of `BtreeKind::ExtentList`
(BTREE-DESIGN§1.7): the first region carries `BTREE_NODE_FLAG_HEAD_OF_CHAIN`, each later
region's kind header names its predecessor and its first entry index, and each region holds
`ExtentEntry` records in object order:

```rust
#[repr(C)]
struct ExtentEntry {                         // 40 bytes
    stored_length: u32,                      //  [0..4]    bytes of this extent on disk
    replica_count: u8,                       //  [4..5]
    _pad: [u8; 3],                           //  [5..8]
    replicas: [ReplicaRef; 4],               //  [8..40]
}
```

A page holds 102 entries and a region 48 pages, 4 896 entries — about 19 GiB of 4 MiB extents —
before a new region is added. Appending (`ExtendObject`, JOURNAL-DESIGN§1.3) appends the changed
page of the newest region; a `ReconcileMove` of one extent appends the page holding its entry.

### 1.4 Object roots

`RootPointer.object_table_root` (ENGINE-DESIGN§1.2) is the root of a small B+ tree of
`BtreeKind::ObjectRoots`, snapshot-agnostic, keyed by origin `NodeId` (`u16`, packed-key
codec), with fixed-size values:

```rust
#[repr(C)]
struct ObjectRootEntry {                     // 32 bytes
    root: BlockRef,                          //  [0..16]   the node's radix root; generation 0 = empty
    next_local: u64,                         // [16..24]   one past the largest local id ever seen
    root_level: u8,                          // [24..25]   inner levels above the leaves
    _reserved: [u8; 7],                      // [25..32]
}
```

`next_local` only grows. For the pool's own node it is the id counter (ENGINE-DESIGN§1); for
another node it is raised by every record received from it. A cluster of a few nodes keeps the
whole tree in one leaf.

---

## 2. Replicas and Backpointers

Two complementary structures translate between objects and physical space:

- **Replica references** (METADATA-DESIGN§2.1) — forward: the content locator and extent-list
  entries name each copy of each extent or region.
- **Backpointers** (METADATA-DESIGN§2.2) — reverse mapping `(disk_id, bucket_no, sector_offset) → owning key`.
  Used by copygc, device evacuation, scrub, resilver, and cluster reconcile. Without
  backpointers these operations cost O(pool size); with them, O(data-on-affected-bucket).

### 2.1 Replica references

```rust
#[repr(C)]
struct ReplicaRef {                          // 8 bytes
    disk_id: u16,                            // [0..2]
    sector_offset: u16,                      // [2..4]   4 KiB sector within the bucket
    bucket_no: u32,                          // [4..8]   bucket within the disk
}
```

**Symmetric replicas.** Every physical copy of an extent or region is one `ReplicaRef` in
`[0, replica_count)`; there is no distinguished "primary". `replicas[0]` is the read-preferred
copy by convention, but the others are equally authoritative. Empty slots are zeroed and ignored.

**`replica_count` is the ground-truth cardinality.** A scrub or verifier can enumerate every
replica of an extent from its record or extent entry, look up each replica's backpointer at its
`(disk_id, bucket_no, sector_offset)`, and assert that exactly `replica_count` backpointers name
the owner. Extra backpointers (orphan replicas) or missing ones (lost replicas) are corruption.

**Cap of 4.** The consistency check needs every replica's address to be recoverable from the
forward side (the backpointer tree is keyed by physical location, not by owner), so the inline
array bounds the replication factor at **4**. More durability uses erasure coding
(RECONCILE-DESIGN§1, `WorkKind::EcEncode`), a separate code path.

`ReplicaRef` is **bucket-relative**, mirroring `BackpointerKey` (METADATA-DESIGN§2.2) so that
the move path (RECONCILE-DESIGN§1.5), scrub, resilver, and copygc share field-level conversions
instead of arithmetic on absolute block numbers. The reachable extent space per disk is
`2^32 buckets × bucket_size`: 4 PiB at the default 1 MiB bucket, 16 PiB at the maximum 4 MiB
bucket. `sector_offset: u16` covers any `bucket_size ≤ 256 MiB` (the format caps bucket size at
4 MiB / 1024 sectors).

Conversion to/from absolute `block_no` (when interfacing with `BlockRef`):

```
bucket_no     = block_no >> (bucket_size_log2 - 12)
sector_offset = block_no & ((1 << (bucket_size_log2 - 12)) - 1)
```

### 2.2 Backpointers (reverse mapping)

A single global B+ tree of large nodes (BTREE-DESIGN§1), `BtreeKind::Backpointer`, keyed by physical
location:

```rust
#[repr(C)]
struct BackpointerKey {                      // 8 bytes logical; packed via BTREE-DESIGN§1.6 to ~3-4 B per leaf
    disk_id: u16,                            // [0..2]
    sector_offset: u16,                      // [2..4]   4 KiB units within the bucket
    bucket_no: u32,                          // [4..8]
}

#[repr(C)]
struct BackpointerValue {                    // 24 bytes
    owner_kind: u8,                          //  [0..1]   OwnerKind
    _pad: u8,                                //  [1..2]
    length_sectors: u16,                     //  [2..4]   extent length in 4 KiB sectors
    bucket_gen: u32,                         //  [4..8]   bucket generation at insertion time
    owner_key: [u8; 16],                     //  [8..24]  owning key (interpreted per OwnerKind)
}

#[repr(u8)]
enum OwnerKind {
    BlobExtent       = 1,                    // owner_key = ObjectId (u64) + extent_index (u64);
                                             //   extent_index 0 for an Extent locator
    Chunk            = 2,                    // owner_key = first 16 B of chunk_hash (BLAKE3 prefix)
    BtreeNode        = 3,                    // owner_key = (BtreeKind, level, min_key prefix);
                                             //   covers radix nodes, ChunkList and ExtentList regions
    // 4, 5 unassigned
    PackedBlock      = 6,                    // owner_key = small-object block id (BLOB-D4); the block
                                             //   names the ObjectId of each of its slots
}
```

The key's logical order is `(disk_id, bucket_no, sector_offset)`; the packed-key codec orders by
it whatever the struct's field order.

**`owner_key` padding.** Variants whose logical key is shorter than 16 bytes must **zero the
trailing bytes** of `owner_key`. Equality of two `BackpointerValue` records is byte-wise over the
full 16 B, and replay of a `ReconcileMove` (JOURNAL-DESIGN§1.3, RECONCILE-DESIGN§1.5) decomposes
into `BackpointerRemove(old_key) ∘ BackpointerInsert(new_key) ∘ owner-update` on exact-match keys
— non-zero padding would silently break that match.

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
  pass, so freeing extents does not have to delete backpointers atomically — a generation bump
  invalidates all of a bucket's backpointers in O(1).

#### Lifecycle

- **Insert** on every blob, chunk, packed-block and tree-node write. Journalled as `BackpointerInsert` (JOURNAL-DESIGN§1.3).
- **Remove** on object deletion or extent rewrite. Journalled as `BackpointerRemove`. May be
  elided when the bucket's generation will be bumped (lazy invalidation).
- **Update** on copygc / reconcile move. The move path emits a single `ReconcileMove` WAL op
  (JOURNAL-DESIGN§1.3, RECONCILE-DESIGN§1.5); replay projects it into the equivalent `BackpointerRemove(old) ∘
  BackpointerInsert(new) ∘ owner-update` sequence and applies all three under one LSN.

#### Operations enabled

| Operation              | Without backpointers     | With backpointers              |
| ---------------------- | ------------------------ | ------------------------------ |
| Copygc bucket reclaim  | Scan all forward indexes | Range scan one bucket prefix   |
| Disk evacuation        | Scan the object table    | Range scan all of disk's buckets |
| Scrub bucket           | Scan all forward indexes | Range scan bucket prefix       |
| Cluster resilver       | Replay full sync log     | Backpointer-driven replay of affected buckets only |
| Stale-pointer cleanup  | Track-during-write       | Lazy via generation comparison |

#### Footprint

Per 10 M objects with average 1 extent each: 10 M backpointers × ~28 B packed ≈ **267 MiB**.
Plus a few thousand tree-node backpointers (~100 KiB). Packed small objects cost one
backpointer per 4 KiB block rather than per object.

---
