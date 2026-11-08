# B+ tree regions — design

Scope: `crates/storage/src/btree.rs`, `crates/storage/src/btree/`, `crates/storage/src/btree_node.rs`.
Governing spec: BTREE-DESIGN§1 (BTREE-DESIGN§1.1 region envelope, BTREE-DESIGN§1.2 runs and blocks, BTREE-DESIGN§1.3 in-memory representation, BTREE-DESIGN§1.4 compaction, BTREE-DESIGN§1.5 bucket packing and depth, BTREE-DESIGN§1.6 packed-key codec, BTREE-DESIGN§1.7 positional regions).

Plan: [btree-implementation-plan.md](btree-implementation-plan.md) · Open decisions: [btree-decisions.md](btree-decisions.md) · Completed: [btree-completed.md](btree-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

One region machinery that every persistent tree plugs into: large nodes that are written once and grow by appending, read one 4 KiB block at a time, with values aligned for in-place access.

## Selected design

- **Write-once regions.** A region's first block (the node header) is written once when the region is created; later flushes append runs after the last written block. Nothing in a region is ever overwritten, so the format fits SD cards, SMR and zoned drives, and write-once buckets alike, and a torn write can only damage the run being appended.
- **Runs of self-contained 4 KiB blocks.** Each run is an index block followed by data blocks. The index block carries the run's key format, a checksum per data block, the first key of each block (its fence key) and, in snapshot-aware trees, the range of snapshot ids each block holds. A lookup reads one index block and one data block per run, not the whole node.
- **Aligned, in-place values.** Keys in a block are fixed-width frame-of-reference encodings (fields of 0, 1, 2, 4 or 8 bytes); values start 8-byte aligned, and a value larger than a block occupies whole blocks of its run. Posting containers, forward entries and other arrays are read in place.
- **Full-length separators.** Inner nodes route by the first key of each child, stored whole; nodes carry no truncated key range.
- **One run format for every tree.** CBOR-valued trees (ontology, subscriptions, placement rules) store their CBOR as opaque value bytes in the same runs.
- **Per-run key formats.** Each run picks its own key format when it is written, so a new key never forces an earlier run to change and no format-promotion step exists.
- **Whiteouts are an entry flag** that survives flush and is dropped only by compaction.
- **Positional regions** (radix leaves and inner nodes, forward overflow) are arrays of 4 KiB pages; a flush appends only the pages that changed (BTREE-DESIGN§1.7).
- `BlockKind` and `BtreeKind` discriminants are pinned by the spec.

## Invariants

- A region read returns exactly what was written; an unreadable or unexpected region is an error, never an empty node.
- A node is flushed only with entries whose journal sets are durable, so every intact run is valid.
- A flush never loses a pending insert or delete.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Large node format

All B+ tree nodes and positional regions are **256 KiB** (configurable 128–512 KiB at format
time; `Superblock.btree_node_size_log2`, default 18 = 256 KiB), divided into 4 KiB blocks:
block 0 holds the node header, and the remaining blocks are written strictly in order as the
node grows. This is the bcachefs model — shallow trees, large nodes, node-internal logs — with
two changes: nothing is rewritten in place, and every read is block-sized.

A 256 KiB node in a 1 MiB bucket means **4 nodes per bucket**, which is the typical packing.
Nodes do not span buckets; when a node has no room for its next run, compaction (BTREE-DESIGN§1.4)
writes it into a fresh region.

Two variants share the envelope:

- **B+ tree node** (forward index, term dictionary, term table, postings, sequences, alloc
  table, freespace LRU, ontology, subscriptions, chunk index, snapshots, disk descriptors,
  placement rules, cluster peers, reconcile queues, object roots): a sequence of **runs** of
  keyed entries (BTREE-DESIGN§1.2).
- **Positional region** (object-table radix leaves and inner nodes, `ForwardOverflow`): a fixed-size array of records, kept as 4 KiB pages (BTREE-DESIGN§1.7).

> **Terminology.** A *run* is the bcachefs `bset` ("btree set") — one append-only, sorted
> commit unit within a node. Its on-disk magic is `"BSET"`.

### 1.1 Region envelope

Block 0 of every region begins with a 64-byte `BtreeNodeHeader`, followed by the kind-specific
fixed header the owning section defines (for example `ChunkParamsRecord`, INDEX-DESIGN§4.3);
the rest of block 0 is zero.

```rust
#[repr(C)]
struct BtreeNodeHeader {                     // 64 bytes, block 0, written once
    pre: BlockPreamble,                      //  [0..8]   magic = "MIMB", kind ∈ BtreeKind (FORMAT-DESIGN§1.3)
    node_seq: u64,                           //  [8..16]  bumped each time the node moves to a fresh region
    created_lsn: u64,                        // [16..24]  journal LSN when the region was created
    region_size_log2: u8,                    // [24..25]  18 = 256 KiB
    level: u8,                               // [25..26]  0 = leaf, ≥ 1 = inner
    layout: u8,                              // [26..27]  0 = runs, 1 = positional pages
    flags: u8,                               // [27..28]  BTREE_NODE_FLAG_*
    logical_pages: u16,                      // [28..30]  positional: pages in the record array
    kind_header_len: u16,                    // [30..32]  bytes of kind-specific header after this one
    _reserved: [u8; 28],                     // [32..60]  zero
    crc: u32,                                // [60..64]  checksum of block 0, this field zeroed
}

// BtreeNodeHeader.flags bits
const BTREE_NODE_FLAG_HEAD_OF_CHAIN: u8 = 1 << 1;  // first region of a positional chain
                                                   // (BTREE-DESIGN§1.7); bit 0 unassigned
```

`BlockPreamble` is the 8-byte common prefix shared with `BlockHeader` (FORMAT-DESIGN§1.3), so a generic
reader can identify any persistent header by its first 8 bytes. `BlockHeader` describes a
write-once 4 KiB unit with a trailing CRC; `BtreeNodeHeader` describes a region that grows by
appending, whose later blocks carry their own checksums.

Checksums in this section (`crc` fields) use the pool's block checksum (OPERATIONS-D4).

### 1.2 Runs and blocks

After block 0 a region holds runs back to back. A run is one **index block** followed by
`block_count` data blocks:

```rust
#[repr(C)]
struct RunHeader {                           // 32 bytes, start of the index block
    magic: u32,                              //  [0..4]   "BSET"
    seq: u32,                                //  [4..8]   1 for the first run, +1 per run
    journal_seq: u64,                        //  [8..16]  newest journal LSN merged into this run
    block_count: u16,                        // [16..18]  data blocks after this index block
    flags: u16,                              // [18..20]  RUN_FLAG_*
    entry_count: u32,                        // [20..24]
    _reserved: u32,                          // [24..28]
    crc: u32,                                // [28..32]  checksum of the index block, this field zeroed
}
// followed in the index block by:
//   KeyFormat                                   (BTREE-DESIGN§1.6)
//   [RunBlockEntry; block_count]
//   fence keys: block_count × KeyFormat.key_width bytes — the first key of each data block

#[repr(C)]
struct RunBlockEntry {                       // 16 bytes
    crc: u32,                                //  [0..4]   checksum of the data block
    entry_count: u16,                        //  [4..6]   0 for a value block
    flags: u16,                              //  [6..8]   RUN_BLOCK_FLAG_*
    min_snapshot: u32,                       //  [8..12]  snapshot-aware trees: smallest snapshot id
    max_snapshot: u32,                       // [12..16]  … and largest, in the block; 0 otherwise
}

const RUN_FLAG_ENCRYPTED:        u16 = 1 << 0;  // data blocks encrypted; whether encryption is
                                                //   marked here, per pool or per entry is
                                                //   ENCRYPTION-D6
const RUN_BLOCK_FLAG_VALUE:      u16 = 1 << 0;  // raw bytes of large values, no block head
```

A run holds as many data blocks as its index block can describe. At load, a reader walks the
runs from block 1: each run's index block must carry the magic, the next `seq` and a valid
checksum; the first block that does not ends the node. A torn append therefore loses only the
run being written, whose entries the journal still holds.

**Data blocks.** Each entry block is self-contained:

```rust
#[repr(C)]
struct DataBlockHead {                       // 8 bytes
    entry_count: u16,                        // [0..2]
    value_area: u16,                         // [2..4]  offset of the first value, 8-aligned
    flags: u16,                              // [4..6]  reserved
    _reserved: u16,                          // [6..8]
}
// followed by:
//   keys:          entry_count × key_width bytes, ascending
//   entry flags:   entry_count × u8          RUN_ENTRY_FLAG_*
//   value ends:    entry_count × u16         end offset of each value (absent for fixed-size values)
//   zero pad to value_area
//   values:        each starting 8-byte aligned

const RUN_ENTRY_FLAG_WHITEOUT: u8 = 1 << 0;  // delete; no value bytes
const RUN_ENTRY_FLAG_LARGE:    u8 = 1 << 1;  // value is LargeValueRef, bytes live in value blocks

#[repr(C)]
struct LargeValueRef {                       // 8 bytes
    first_block: u16,                        // [0..2]  data-block index within the run
    block_count: u16,                        // [2..4]
    length: u32,                             // [4..8]  value bytes
}
```

A value too large for the block (an 8 KiB bitmap container, a long forward entry) is stored
block-aligned in consecutive value blocks of the same run. Inner-node entries are separator
keys — the first key of each child — with a `NodePtr` value (ENGINE-DESIGN§1.2).

**Lookup** in a node: for each run, newest first, binary-search its fence keys, read the one
data block that can hold the key (one 4 KiB read; the index block is cached), and binary-search
its keys in place. The first run holding the key decides; a whiteout there means absent.

### 1.3 In-memory representation

```rust
struct LoadedNode {
    header: BtreeNodeHeader,
    runs: SmallVec<[RunIndex; 4]>,           // index blocks; data blocks loaded on demand
    pending_journal: Vec<JournalEntry>,      // JOURNAL-DESIGN§1.4 entries not yet in a run
    next_block: u16,                         // first unwritten block of the region
    dirty: bool,
    pin: JournalPin,                         // journal pin for the pending entries
}
```

Data blocks are cached by `(region, block)` like any other block; a node never needs all of its
blocks in memory. Inner-node descent compares against separator keys.

**Flush.** The journal-reclaim thread (JOURNAL-DESIGN§1.4) materialises the node's pending
entries into one new run — keys, whiteouts and values — chooses the run's key format from those
keys alone (BTREE-DESIGN§1.6), and writes the index block and data blocks at `next_block`.
Nothing else in the region is written.

### 1.4 Compaction

When a node lacks the blocks for its next run, holds more than 4 runs, or has more than 75 % of
its blocks in use, compaction:

1. Allocates a fresh region (ALLOCATION-DESIGN§1.5).
2. Merges all runs and the pending entries into one run, dropping whiteouts whose deletion no
   older run or snapshot still needs (SNAPSHOTS-DESIGN§1.3), and splits the node if the run
   would not fit.
3. Updates the parent's `NodePtr` (a pending entry in the parent, flushed in turn).
4. Leaves the old region to be freed when a checkpoint commits the new pointer; its bucket's
   live sectors drop then (ALLOCATION-DESIGN§1.2).

Compaction is COW like any flush: the committed root keeps pointing at the old region until a
checkpoint commits the new one, so an interrupted compaction needs no recovery step.

### 1.5 Bucket packing and depth

A node occupies a quarter of a default 1 MiB bucket; the allocator hands out successive regions
of one open bucket (ALLOCATION-DESIGN§1.5). For a 10 M-object pool with 5 000 tags and ~100
snapshots, every tree is shallow:

| Tree                | Depth  | Levels | Notes                                      |
| ------------------- | ------ | ------ | ------------------------------------------ |
| Object table (METADATA-DESIGN§1)   | 1      | 2      | 6 511 leaves under 1 inner (R = 1)       |
| Forward index (INDEX-DESIGN§2)  | 1      | 2      | ~3 000 leaves (~58 B per object at 8 direct assertions) under 1 inner |
| Term dictionary (INDEX-DESIGN§4.1) | 0–1 | 1–2    | 5 000 tag entries fit one leaf; depth 1 once attribute values number in the millions |
| Postings (INDEX-DESIGN§3.1)     | 1      | 2      | one entry per (term, 65 536-id window)     |

(See the depth convention in METADATA-DESIGN§1: *depth* counts inner levels above the leaves; *levels* is the
total count — a depth-*N* tree has *N + 1* levels.) A cold lookup costs one or two 4 KiB reads
per level and run — on an SD card or HDD that is the difference between reading 8 KiB and
256 KiB — while compaction and flushes stay large sequential writes.

### 1.6 Key formats (the packed-key codec)

Each run's index block carries the **key format** of all its keys:

```rust
#[repr(C)]
struct KeyFormat {                           // 8 + nr_fields × 16 bytes
    nr_fields: u8,                           // [0..1]  1..=8
    key_width: u8,                           // [1..2]  bytes per encoded key: Σ field widths
    value_kind: u8,                          // [2..3]  0 = fixed size, 1 = variable
    _pad: u8,                                // [3..4]
    fixed_value_len: u32,                    // [4..8]  value bytes when value_kind = 0
    fields: [FieldFormat; nr_fields],
}

#[repr(C)]
struct FieldFormat {                         // 16 bytes
    width: u8,                               //  [0..1]  0, 1, 2, 4 or 8 bytes; 0 ⇒ constant (= base)
    flags: u8,                               //  [1..2]  FIELD_FORMAT_FLAG_*
    _pad: [u8; 6],                           //  [2..8]
    base: u64,                               //  [8..16] subtracted from the field before encoding
}

const FIELD_FORMAT_FLAG_SIGNED:    u8 = 1 << 0;
const FIELD_FORMAT_FLAG_MSB_FIRST: u8 = 1 << 1;  // field holds big-endian bytes compared lexicographically
```

**Encoding.** A key is its fields in declaration order, each `field − base` stored in `width`
bytes, little-endian unless `FIELD_FORMAT_FLAG_MSB_FIRST`; keys are fixed-width within a run and
compare field by field. A field whose values are all equal in the run has width 0 and costs
nothing per key. Byte strings that order lexicographically — `NormalisedValue`, hashes — are
split into 8-byte `MSB_FIRST` fields; a 32-byte chunk hash is four such fields with base 0, its
bytes unchanged.

**Format choice.** The writer of a run computes each field's `max − min` over the run's keys
and takes the smallest width that holds it. Because a run is written whole and never extended,
each run's format fits its own keys exactly, and runs in one node may differ.

**Typical sizes** (snapshot-aware trees append `snapshot: u32`, usually constant in a run):

| Tree              | Unpacked key | Typical encoded | Driver                            |
| ----------------- | ------------ | --------------- | --------------------------------- |
| Forward           | 12 B (oid + snapshot) | 2–4 B  | Sequential oid; snapshot constant |
| Postings          | 20 B (term, window, snapshot) | 3–6 B | Term constant across a term's windows |
| Term dictionary   | ~49 B        | 12–24 B         | kind and key constant; value bytes kept |
| Alloc table       | 4 B (bucket) | 2 B             | Sequential bucket numbers         |
| Subscriptions     | 8 B (sub_id) | 1–2 B           | Sequential ids                    |
| Snapshots         | 4 B          | 2 B             | Sequential snapshot ids           |
| Chunk index       | 32 B         | 32 B            | Hashes do not compress            |

### 1.7 Positional regions

A positional region holds a fixed-size record array of `logical_pages` 4 KiB pages
(`logical_pages` = ¾ of the region's blocks, rounded down: 48 for 256 KiB). Pages are written in
**page runs**, appended after block 0 like key runs:

```rust
#[repr(C)]
struct PageRunHeader {                       // 32 bytes, start of the page-run index block
    magic: u32,                              //  [0..4]   "PAGE"
    seq: u32,                                //  [4..8]   1 for the first run, +1 per run
    journal_seq: u64,                        //  [8..16]
    page_count: u16,                         // [16..18]  pages in this run
    _reserved: [u8; 10],                     // [18..28]
    crc: u32,                                // [28..32]  checksum of the index block, this field zeroed
}
// followed by [PageRunEntry; page_count]

#[repr(C)]
struct PageRunEntry {                        // 8 bytes
    page_no: u16,                            // [0..2]  index into the record array
    _pad: u16,                               // [2..4]
    crc: u32,                                // [4..8]  checksum of the page
}
```

The newest copy of each page wins; a page never written reads as zeros. A flush writes one page
run holding only the pages that changed since the last run — one dirty page costs two blocks,
8 KiB — and compaction (BTREE-DESIGN§1.4) rewrites the region's live pages as one run in a fresh
region when the next page run would not fit.

**Chains.** A forward-overflow array longer than one region (INDEX-DESIGN§2.2) continues in
further regions linked **backwards**: each region's kind header names the previous region's
`NodePtr` (absent in the first region, which carries `BTREE_NODE_FLAG_HEAD_OF_CHAIN`) and the
index of its first record. The owner points at the newest region, so appending never rewrites
an earlier one; a reader walks back from the newest region once to find the region holding a
given record. Moving an earlier region rewrites every region after it, since each predecessor
pointer is in a header written once; this is acceptable only because a multi-region overflow
needs tens of thousands of direct terms on one object. Per-object maps that grow without bound —
extents and chunks — are keys of shared trees instead (METADATA-DESIGN§1.3, INDEX-DESIGN§4.3).

---
