# Indices — design

Scope: `crates/index` (forward, tag, KV, range, chunk indices; tag bitmap pages; KV directory and buckets), and the bitmap representation shared with `crates/query`, `crates/fuse`, `crates/watch`.
Governing spec: INDEX-DESIGN§2 (forward), INDEX-DESIGN§3 (tag inverted index, stores, bitmap pages), INDEX-DESIGN§4 (KV equality, range, chunk), SPECIFICATION-DESIGN§5.

Plan: [index-implementation-plan.md](index-implementation-plan.md) · Open decisions: [index-decisions.md](index-decisions.md) · Completed: [index-completed.md](index-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Every index persists in the spec's native byte layout through the packed-key codec, so footprint and copy-on-write granularity match FORMAT-DESIGN§16.

## Selected design

- **Forward index.** One packed `VARINT` sorted-run entry per `(oid, snapshot)` whose value is the §7.1 `LeafEntry` byte image; more than 8 assertions spill into a chain of 256 KiB `ForwardOverflow` regions linked by a trailing `next_page`. `PackedAssertion` is 16 B with `b: u64`.
- **Tag index.** Directory entries are 48 B `TagIndexLeafEntry` (8 B packed key + 40 B value) whose `store_root` points at a chain of 4 KiB `TagBitmapPage`s (Simple), `SequencePage`s (Ordered) or `RankedPage`s (Ranked).
- **KV index.** Native extendible hash: 4 KiB `KvDirectory` (inline 252 `BlockRef`s, `global_depth ≤ 7`, spillover region beyond), 4 KiB `KvBucket` blocks of 144 entries, bitmaps in `TagBitmapPage`s.
- **Range index.** Key `(attr_id, value: NormalisedKey, oid, snapshot)` per INDEX-DESIGN§4.2.
- **Chunk index.** 32 B hash key + 24 B value tail (`ref_count`, `length`, `BlobRef`) reproducing the 56 B `ChunkIndexLeafEntry`.
- `snapshot` is 0 in every key until [snapshots-design.md](snapshots-design.md) lands.

## Invariants

- Bitmaps carry the full object identity; no index assumes `node = 0` or truncates `ObjectId.local` (48 bits).

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Heterogeneous Value Encoding

`Value` (the contents of attributes) is the only data type whose shape varies enough that fixed
binary layout is wasteful. It is encoded as positional **CBOR** (FORMAT-DESIGN§1.6): a
two-element array of an integer type tag and the payload, or three elements for `Scoped`:

```
Value ::= [0, text]                      // Text(string)      payload: CBOR major type 3
        | [1, int]                       // Int(i64)          payload: CBOR major type 0/1
        | [2, float]                     // Float(f64)        payload: CBOR major type 7, 64-bit
        | [3, int]                       // Timestamp(i64)    payload: CBOR major type 0/1
        | [4, bytes]                     // Blob(bytes)       payload: CBOR major type 2; large values spill — see below
        | [5, context, inner]            // Scoped { context: TagId (u32), inner: Value }  INDEX-DESIGN§1.3
```

Integers use the shortest CBOR form; a float is always written as a 64-bit CBOR float, so one
`Value` has exactly one encoding. `encode(v)` below means this encoding.

`Scoped` is the canonical way to carry "this attribute applies in the context of tag *C*" —
e.g. an object's `unix-path` value scoped by a `unix-path-context:rpi4-sdcard` tag. It is
recursive (the inner Value can be any variant *except* another `Scoped` — nested scopes are a
format error). The pattern generalises: any attribute can be scoped by any tag, not only the
unix-path projection use case.

### 1.1 Value table

Indices identify an attribute value by its `value_hash` (INDEX-DESIGN§1.2): KV entries and
forward-index assertions carry the hash, never the value. Every distinct value is stored once, in
the **value table** — a BTREE-DESIGN§1 large-node tree of `BtreeKind::ValueSpill`, rooted at
`RootPointer.value_spill_root`, keyed by `value_hash`:

- A value whose encoding is ≤ 96 bytes is stored **inline** as the entry's value tail. The
  96-byte threshold keeps ≥ 16 entries per 4 KiB of leaf.
- A larger value is stored **out of line**: the entry holds `BlobRef { disk_id, block_no,
  length }` and the encoding lives as a blob-zone extent.

**The hash is the value's identity.** Writing an attribute computes its `value_hash` and probes
the value table. No entry: the value is inserted. An entry holding an equal value: it is reused.
An entry holding a *different* value: the write fails with a defined collision error and changes
nothing, so a hash names exactly one value pool-wide and no read path compares values. Entries
that no assertion references any longer are reclaimable; finding them is a background scan.

### 1.2 Value hash for KV index

`value_hash(v) = SipHash-2-4(domain="mimir-kv", encode(v))` truncated to 64 bits. The type tag
at the head of `encode(v)` keeps `Int(42)` from colliding with `Text("42")`. SipHash-2-4 is keyed
by a 16-byte per-pool secret to defeat hash flooding: random bytes generated when the pool is
created, stored as `Superblock.value_hash_secret` (ENGINE-DESIGN§1.1) identically in every
member's superblock, and never changed, since every stored hash depends on it. The secret is
local to the pool: `value_hash` never appears in data sent to other nodes
([cluster-design.md](cluster-design.md)).

### 1.3 Scoped values

`Value::Scoped { context, inner }` lets the same attribute key carry different values under
different scopes without inflating the tag namespace. The hash and comparison rules are:

- `value_hash(Scoped { context, inner })` is the same function over `[5, context, inner]`.
  Two scoped values with the same `context` and equal inner values hash identically; differing
  contexts hash differently. KV equality lookups against a scoped attribute key (e.g. "find every
  object whose `unix-path` value is `Scoped(rpi4, "/boot/vesper")`") are therefore exact hash hits.
- For range / prefix scans, `NormalisedKey` (INDEX-DESIGN§4.2) prepends the 4-byte `context` TagId before the
  inner value's normalised encoding. Prefix scans over `(context, *)` give the manifest of every
  scoped value under one context — no separate manifest tree needed.
- The query optimiser is free to rewrite `attr=K AND scope=C` as a KV/Range probe with a synthetic
  `Scoped { C, * }` key.

A `Scoped` whose inner is also `Scoped` is rejected at parse time. Attributes that are *never*
context-scoped (e.g. `created_ns`) simply never use this variant; readers see plain values and
do not need to be scope-aware.

**Type validation.** `Scoped` is **transparent to ontology type-checking**: an attribute
declared with `value_type = "text"` accepts both `Text(...)` and `Scoped { context, inner: Text(...) }`.
The validator strips the `Scoped` wrapper and recurses on `inner`. Scoping is therefore
**orthogonal** to value-type — any attribute can be scoped without altering its declared
type, and the ontology does not need a separate `scopable` flag. The validator additionally
checks that `context` is a `TagId` whose definition has `Grouping` semantics; non-grouping
tags cannot be used as scopes.

---

## 2. Forward Index

The forward index maps `oid → [ForwardEntry]` and must support fast per-object listing and
per-object diffing for sync.

On disk it is a **B+ tree of large nodes** (BTREE-DESIGN§1), keyed by `oid`. Each node uses the standard
multi-run envelope: new mutations are appended as a fresh sorted run; lookups merge-search across
all sorted runs in the node.

### 2.1 Node layout

Both inner and leaf sorted runs use the BTREE-DESIGN§1.6 packed-key encoding. Within a single leaf, all `oid`
keys share the leaf's key range (typically a span of 10⁴ – 10⁵ contiguous ids), so the format
descriptor's `bit_width` settles around 16–20 bits — encoding `oid` as 2–3 bytes versus the
unpacked 8 bytes.

- **Inner node** (`BtreeKind::Forward`, level ≥ 1): one or more sorted runs of
  `(packed_(oid, snapshot)_key, child: BlockRef)` pairs. The `snapshot` field is part of the
  separator key — without it, an oid whose `(oid, *)` cluster spans a leaf boundary would be
  unrepresentable (the inner separator would collide with the same `oid` on both sides). With
  BTREE-DESIGN§1.6 packing the trailing `snapshot` packs to ~0 bits when one snapshot dominates a sorted
  run, so the cost is marginal: a 3 B ceiled key + 16 B BlockRef = 19 B per entry; one full
  sorted run packs 13 789 children (⌊262 008 / 19⌋). Tree at 10 M objects: **depth 1** (1 inner
  level + leaves; 2 levels total).
- **Leaf node** (level 0): sorted runs of `LeafEntry` records:

```rust
// Logical (unpacked) shape; on-disk uses the BTREE-DESIGN§1.6 packed encoding.
struct LeafEntry {                           // variable length on disk
    oid: u64,                                // sort key — packed via SortedRunKeyFormat
    snapshot: u32,                           // sort key (SNAPSHOTS-DESIGN§1.2) — packs to ~0 bits when one
                                             //   snapshot dominates a sorted run
    header: u16,                             // bitfield: see below
    body: union {                            // discriminated by header & LEAF_ENTRY_SPILL_FLAG
        inline: [PackedAssertion; header & LEAF_ENTRY_TOTAL_MASK],   // !is_spill
        spill_ref: BlockRef,                                         //  is_spill; chain via
                                                                     //  spill region's trailing
                                                                     //  BlockRef slot per INDEX-DESIGN§2.2
    },
}

// LeafEntry.header bitfield
const LEAF_ENTRY_SPILL_FLAG: u16 = 1 << 15;  // body is spill_ref (BlockRef) when set
const LEAF_ENTRY_TOTAL_MASK: u16 = 0x7FFF;   // bits 0..14: total assertion count
```

The 16-bit `header` is a single bitfield carrying both the spill flag and the assertion
count:

```
header: u16
  LEAF_ENTRY_SPILL_FLAG (bit 15)   1 = body is `spill_ref` (BlockRef into a ForwardOverflow
                                       chain)
                                   0 = body is the inline `[PackedAssertion; total]` array
  LEAF_ENTRY_TOTAL_MASK (bits 0..14)  total — object's total assertion count, regardless
                                       of is_spill:
                                         not spilled → total inline assertions (typical
                                                       0..8, hard cap ~16 before next-
                                                       mutation spill)
                                         spilled     → total assertions across the entire
                                                       spill chain (up to 32 K)
```

**Total cardinality** is therefore `header & LEAF_ENTRY_TOTAL_MASK` — a single masked read,
no branching on the spill bit. The query optimiser uses this for intersection planning (INDEX-DESIGN§4), where
the smaller-cardinality side is iterated first. The 15-bit ceiling (32 K) is comfortably
above the >200-tag pathological case mentioned in METADATA-DESIGN§1.2; objects exceeding 32 K assertions
take the per-object B+ tree escape hatch.

The merge — a single `header` field rather than separate `count` and `spill` — saves 2 bytes
per `LeafEntry` and removes the "either count or spill_count is zero" implicit invariant of
the previous two-field form.

The directory's sorted-run encoding sets `SORTED_RUN_FLAG_PACKED_KEYS` with the 2-field
key `(oid, snapshot)` and `value_size_kind = VALUE_SIZE_KIND_VARINT` (BTREE-DESIGN§1.6), since the
`LeafEntry` value tail is variable-length: 2 B `header` followed by either a
`[PackedAssertion; total]` inline body (sized `16 × total` bytes) or a 16 B spill
`BlockRef`. The codec's per-entry varint length prefix adds 1–2 B per leaf entry —
dwarfed by the savings over the CBOR fallback for this shape.

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

At "8 assertions per object" (the INDEX-DESIGN§2.2 inline-spill threshold; per-entry = 3 B ceiled
`(oid, snapshot)` key (the average is 2.5 B; integer math uses the ceiling) + 2 B header
+ 128 B inline body = 133 B), a leaf packs 1 969 entries total. Sorted runs share the region's payload bytes (BTREE-DESIGN§1.2 appends them into the same 256 KiB
region), so adding sorted runs does not multiply capacity — each new run consumes 72 B of
overhead (32 B `SortedRunHeader` + 40 B `SortedRunKeyFormat` for the 2-field key) and slightly
reduces the entry budget. With 4 active sorted runs the leaf still carries 1 968 entries
before BTREE-DESIGN§1.4 triggers full compaction. Smaller objects pack denser: 4 assertions per entry
→ 3 797 entries per leaf (3 794 with 4 active sorted runs).

### 2.2 Spill

Objects with more than 8 assertions store a `BlockRef` to a `ForwardOverflow` region (also a
256 KiB large-node region; positional, no sorted runs — single rewrite on growth). Each
overflow region holds up to 16 379 × `PackedAssertion` and chains to the next via a fixed
trailing `BlockRef` slot.

```rust
#[repr(C, packed)]
struct ForwardOverflowRegion {               // 262 144 B = 256 KiB
    header:    BtreeNodeHeader,              //      [0..64]      kind = ForwardOverflow;
                                             //                   payload_used = live_count × 16
    entries:   [PackedAssertion; 16_379],    //     [64..262_128] 16 379 × 16 B = 262 064 B
    next_page: BlockRef,                     // [262_128..262_144] ZERO = tail of chain
}
```

The region is **positional**, not a sorted-run BTREE-DESIGN§1 node — `BtreeNodeHeader.payload_used`
records `live_count × 16` so partially-filled tail regions are unambiguous. The `next_page`
link sits at the fixed offset `262 128` regardless of `payload_used`. Reconstruction of an
object's full assertion list walks the chain, consuming up to `payload_used / 16` entries
per region, until the leaf entry's `total` is exhausted; the tail region's `next_page` must
equal `BlockRef::zeroed()` for a consistent chain — a mismatch signals format corruption.

### 2.3 Sorted-run behaviour

- **Append on flush.** When journal-reclaim flushes a forward-index leaf, only the new sorted run is
  written — typically a few KB to a few tens of KB, not the whole 256 KiB node.
- **In-memory merge.** `LoadedNode.merged_view` builds a `BTreeMap<u64, SmallVec<[PackedAssertion;
  8]>>` lazily on first lookup; subsequent lookups are direct hits.
- **Full compaction** triggers when `payload_used > 75%` or `sorted_run_count > 4`, rewriting the node
  into a fresh region with a single merged sorted run.

For attributes whose actual `Value` matters (not just its hash), the `value_hash` indirects into
the value table (INDEX-DESIGN§1.1), shared with the KV index (INDEX-DESIGN§4.1). Tag and Relation
entries are self-contained.

### 2.4 Properties

- O(log N) lookup by oid (depth 1 at 10 M scale ⇒ ≤ 2 large-node loads: 1 inner + 1 leaf).
- O(1) per-assertion diffing via `SortedRunHeader.journal_seq` — sync streams sorted runs newer than
  the peer's watermark, exactly the journal-streaming fast path of SNAPSHOTS-DESIGN§1.2.
- Compact in-memory mirror: a `HashMap<u64, SmallVec<[PackedAssertion; 8]>>` over the loaded
  node's `merged_view`.

---

## 3. Tag Inverted Index

The tag index is the heart of query performance. Its on-disk form must:

- Look up a tag's bitmap quickly.
- Allow per-tag COW updates without rewriting unrelated tags.
- Support delta-sync: cheap diff between two snapshots of the same bitmap.

### 3.1 TagDirectory

A **B+ tree of large nodes** (BTREE-DESIGN§1) keyed by `(TagId: u32, snapshot: u32)` — snapshot-aware per
SNAPSHOTS-DESIGN§1.2. Leaf entries are 48 B, with fields reordered so every multi-byte field sits at its
natural alignment (8-byte struct alignment; no `#[repr(packed)]` — the entry is read repeatedly
during query bitmap algebra and unaligned loads on `last_modify_lsn` / `store_root` would be a
measurable overhead):

```
#[repr(C)]
TagIndexLeafEntry {                          // 48 bytes
    last_modify_lsn: u64,                    //  8  @  0   (8-aligned)
    store_root:      BlockRef,               // 16  @  8   INDEX-DESIGN§3.2 / INDEX-DESIGN§3.3 — contains a u64 generation
    tag_id:          u32,                    //  4  @ 24   sort key (with `snapshot`, SNAPSHOTS-DESIGN§1.2)
    snapshot:        u32,                    //  4  @ 28   sort key (suffix); ~0 bits packed
                                             //                       when one snapshot dominates
    cardinality:     u32,                    //  4  @ 32   for fast query-planner stats
    generation:      u32,                    //  4  @ 36   bumped on bitmap rewrite
    store_kind:      u8,                     //  1  @ 40   Simple / Ordered / Ranked
    _pad:            [u8; 7],                //  7  @ 41   (8-byte alignment tail)
                                             // 48 total
}
```

The hot key+pointer pair still lands within a single 64-byte cache line: a query that loads a
leaf entry to dispatch a bitmap fetch gets `store_root`, `tag_id`, `snapshot`, `cardinality`,
`generation`, and `store_kind` all in `[0..48]` — one 64 B fetch.

On-disk, leaf sorted runs use the BTREE-DESIGN§1.6 packed-key encoding with **two key fields**
(`tag_id`, `snapshot`); the format descriptor occupies 8 + 2 × 16 = **40 B per sorted run**
(down from 56 B for a 3-field shape). `tag_id` typically packs to 2–3 bytes per leaf (sparse
but clustered ids); `snapshot` packs to ~0 bits when one snapshot dominates, a few bits
otherwise. The 40 bytes of value following the key stay byte-aligned. Per-key on-disk cost is
~37 B. A 256 KiB leaf sorted run (262 144 − 64 − 32 − 40 = 262 008 B payload) holds
**7 081 entries**. For 5 000 tags × ~100 snapshots without divergence (one entry per tag,
shared across snapshots) the full directory fits in **a single leaf** (depth 0); pools where
many tags diverge across snapshots, or pools with hundreds of thousands of tags, extend the
tree to depth 1 (~115 M-entry capacity).

`(cardinality, last_modify_lsn, generation)` enables fast snapshot diffing without dereferencing
the bitmap — the directory's sorted run stream alone tells a peer which tags changed and how.

### 3.2 Roaring bitmaps on disk

Roaring bitmaps stay on the **4 KiB block format** (not 256 KiB nodes) because:

- The portable Roaring spec is already container-addressable and mmap-friendly at any offset.
- Per-tag bitmaps vary from a few hundred bytes to many megabytes; the 4 KiB granularity matches
  Roaring's natural 4 KiB-class bitmap container.
- Bitmaps are referenced as opaque blobs from the directory; their internal format is a stable
  external standard (Apache Lucene + the `roaring` crate).

We adopt the **Roaring portable serialization spec** as the on-the-wire bitmap image, split
across a singly-linked chain of `BlockHeader`-prefixed 4 KiB pages. Each `TagBitmapPage`
carries a slice of the full roaring byte image plus a `next_page` pointer (zero on the tail):

```rust
#[repr(C, packed)]
struct TagBitmapPage {                       // 4 096 bytes
    header:       BlockHeader,               //    [0..32]   kind = TagBitmapPage
    bitmap_len:   u32,                       //   [32..36]   bytes carried in this page
                                             //               (0..=4036)
    next_page:    BlockRef,                  //   [36..52]   ZERO = tail of chain
    bitmap_bytes: [u8; 4036],                //   [52..4088] roaring portable bytes;
                                             //               trailing bytes zeroed
    _pad:         [u8; 4],                   // [4088..4092]
    crc:          u32,                       // [4092..4096] CRC32C over [0..4092]
}
```

Bitmaps that fit in a single page set `next_page = BlockRef::ZERO` and carry the full
portable image in `bitmap_bytes[..bitmap_len]`. Larger bitmaps split the portable image
across N pages in order: page *i* holds `image[i_start..i_end]`, the head page is the one
reachable via the tag-directory leaf's `store_root`, and the chain terminates at the page
whose `next_page` is zero. Reconstruction concatenates each page's
`bitmap_bytes[..bitmap_len]` slice in chain order and feeds the result to
`roaring::RoaringBitmap::deserialize_from`. Per-page payload cap is 4 036 B, so an N-page
chain holds up to 4 036 × N bytes of roaring image.

### 3.3 Ordered & ranked stores

```
OrderedStore root block:
  members:        BlockRef → TagBitmap
  sequence_head:  BlockRef → first SequencePage in a singly-linked chain
  sequence_count: u64                  // total ObjectIds across the chain

RankedStore root block:
  members:        BlockRef → TagBitmap
  ranked_head:    BlockRef → first RankedPage in a singly-linked chain
  ranked_count:   u64                  // total entries across the chain
```

Each `SequencePage` / `RankedPage` ends with a `next: BlockRef` slot (zero on the tail page);
walking the logical array is a forward chain traversal. Pages are sized to hold a whole 4 KiB
block of entries; appending to the array writes a new page (when the tail is full) and updates
the previous tail's `next` pointer in the same checkpoint as the new page.

Each `SequencePage` and `RankedPage` is a 4 KiB block under the standard FORMAT-DESIGN§1.3 framing
(`BlockHeader` + payload + trailing CRC32C):

```
SequencePage (4 KiB):
  BlockHeader  { kind = SequencePage, format_version = 1 }        // 32 B
  entry_count: u16                                                //  2 B
  _pad: [u8; 6]                                                   //  6 B
  next: BlockRef                   // 0 = tail of chain            // 16 B
  entries: [u64; 504]              // ObjectIds                    // 4032 B
  _pad_tail: [u8; 4]                                              //  4 B
  trailing CRC32C                                                 //  4 B
                                                                  // = 4096 B

RankedPage (4 KiB):
  BlockHeader  { kind = RankedPage, format_version = 1 }          // 32 B
  entry_count: u16                                                //  2 B
  _pad: [u8; 6]                                                   //  6 B
  next: BlockRef                   // 0 = tail of chain            // 16 B
  entries: [{ oid: u64, score: f32, _pad: u32 }; 252]   // 16 B   // 4032 B
  _pad_tail: [u8; 4]                                              //  4 B
  trailing CRC32C                                                 //  4 B
                                                                  // = 4096 B
```

The sequence pages form a logical array; updates use a packed log + periodic compaction (small
appends are appended to a tail page, occasional rewrites compact, both COW and reflected in the
checkpoint).

---

## 4. KV Equality Index, Range Index, Chunk Index

### 4.1 KV Equality Index

Hash-based on disk via **extendible hashing** keyed by `(tag_id, value_hash)`. The KV index is
the one structure that does **not** use the BTREE-DESIGN§1 large-node B+ tree format — point-equality
lookups benefit more from hash-bucket addressing.

`value_hash` is computed transparently over `Value::Scoped` wrappers — see INDEX-DESIGN§1.3. A scoped
attribute and an unscoped one with the same inner content hash to **different** values, so
they land in different KV buckets without any branching at the lookup site.

```
KvDirectory (single 4 KiB block, addressed by RootPointer.kv_index_root):
   header (BlockHeader, kind = KvHashDirectory)                                        // 32 B
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
hash buckets). At `global_depth ≥ 8`, `spillover_root` points at a BTREE-DESIGN§1 large-node positional
region (`BtreeKind::KvDirectory`, level 0) holding the full `2^global_depth`-sized BlockRef
array; the inline `entries` array is then ignored. Each 256 KiB spillover region holds 16 380
entries, supporting `global_depth` up to 13 (8 192 buckets) before chaining to a deeper region.

The bitmap referenced by each entry is a `TagBitmap` (INDEX-DESIGN§3.2), reused via the same machinery.

The value behind each `value_hash` is in the value table (INDEX-DESIGN§1.1), for display, faceted
enumeration and range comparisons. Because a hash names one value (INDEX-DESIGN§1.1), an
equality lookup returns its entry's bitmap without comparing values.

### 4.2 Range Index

A **B+ tree of large nodes** (BTREE-DESIGN§1) keyed by `(attr_id: u32, value: NormalisedKey, oid: u64,
snapshot: u32)` — 32 B unpacked (snapshot-aware per SNAPSHOTS-DESIGN§1.2). For scoped attribute values
(INDEX-DESIGN§1.3), `NormalisedKey` prepends the 4-byte context `TagId` before the inner value's
encoding, so prefix scans over `(attr_id, context, *)` give the manifest of every scoped
value under one context — no separate manifest tree needed.

With per-run key packing (BTREE-DESIGN§1.6) this is the structure that benefits most:

- `attr_id` is almost always **constant** within a leaf (a leaf covers one or two adjacent
  attributes) → 0 bits per key.
- `value` (16 B `NormalisedKey`) gets a per-run base + bit-width; for numeric attributes the
  span within a leaf typically fits in 24–40 bits.
- `oid` packs identically to forward-index keys: 16–20 bits.
- `snapshot` packs to ~0 bits when one snapshot dominates a sorted run (SNAPSHOTS-DESIGN§1.2).

Net per-key size: typically **8–12 B** packed. Leaf sorted runs store key → `BlockRef` to a
roaring bitmap (16 B value), so per-entry on-disk cost is 24–28 B. Per-run payload budget is
`262 144 − 64 (BtreeNodeHeader) − 32 (SortedRunHeader) − 72 (4-field SortedRunKeyFormat:
attr_id, value, oid, snapshot) = 261 976 B`, holding **~9 350–10 900 entries per sorted run**
depending on key-packing efficiency.

`NormalisedKey` is a fixed-size order-preserving encoding:

| ValueType | Encoding (16 bytes)                                                                       |
| --------- | ----------------------------------------------------------------------------------------- |
| Int/Time  | `i64` flipped sign bit (so unsigned compare = signed compare), zero-padded.               |
| Float     | IEEE 754 with sign-bit-flipped trick: positives flip top bit, negatives flip all bits.    |
| Text      | First 14 bytes of UTF-8, length byte, continuation flag; long strings spill via `value_hash`. |
| Blob      | Stored only by hash; not range-indexed.                                                   |

Leaf values are roaring bitmaps (per `(attr_id, value_prefix)`), enabling cheap range scans.

### 4.3 Chunk Index and Chunk List (for FastCDC objects)

FastCDC-chunked objects are stored across **two complementary structures** with deliberately
opposing key shapes. Both are needed; neither subsumes the other.

| Structure    | Keyed by             | Role                                                   | Snapshot? |
| ------------ | -------------------- | ------------------------------------------------------ | --------- |
| `ChunkIndex` | `chunk_hash`         | **Content-addressed dedup directory.** One entry per unique chunk in the entire pool. Tells you "where does the chunk with hash X live?" → `BlobRef` to its physical extent, plus a `ref_count`. | snapshot-agnostic |
| `ChunkList`  | position (per-object) | **Per-object recipe.** One region (chain) per chunked object. Tells you "to reconstruct object O, fetch chunks `[hash₀, hash₁, hash₂, …]` in this order". The hashes are pointers *into* `ChunkIndex` — not the chunk bytes. | snapshot-agnostic (shared prefix via COW) |

The split is the same pattern as Git (object database vs. tree object) or ZFS dedup (DDT vs.
file extent map):

- **Read** a chunked object: walk its `ChunkList` in order → for each `chunk_hash` look up
  `ChunkIndex` → get `BlobRef` → read bytes from the blob zone.
- **Write** a chunked object: FastCDC splits plaintext → for each chunk, hash it and probe
  `ChunkIndex` → if hit, bump `ref_count`; if miss, write the bytes and insert. Either way,
  append the hash to the new object's `ChunkList`.
- **Dedup** falls out for free: two unrelated objects whose plaintext shares chunks share the
  storage at the byte level via the shared `ChunkIndex` entries — `ref_count` tracks how
  many `ChunkList`s reference each chunk; physical reclaim happens when it drops to zero.

You can't merge the two: a hash-keyed structure can't preserve order, and a position-keyed
structure can't dedup. The reverse direction (chunk → which objects use it) is covered by
the backpointers btree (METADATA-DESIGN§2.2) under `OwnerKind::Chunk`.

#### `ChunkIndex` — content-addressed dedup directory

A **B+ tree of large nodes** (BTREE-DESIGN§1) keyed by `chunk_hash: [u8; 32]`:

```
ChunkIndexLeafEntry {                        // 56 bytes
    chunk_hash: [u8; 32],
    ref_count: u32,
    length: u32,
    blob: BlobRef,                           // 16 B
}
```

A 256 KiB leaf packs **4 678 chunk entries per sorted run** —
`(262 144 − 64 [BtreeNodeHeader] − 32 [SortedRunHeader] − 72 [4-field key format])
÷ 56 = 4 678`. Random-looking BLAKE3 hashes bypass BTREE-DESIGN§1.6 packing (each 32 B hash uses
four `bit_width = 64` field formats), so per-key cost is the unpacked 56 B.

Chunk hashes use BLAKE3 of plaintext (pre-compression) so dedup is content-defined. FastCDC
(content-defined chunking) decides the chunk boundaries; the chunk index records each unique
chunk once, refcounted, so two objects sharing common chunks share their storage.

#### `ChunkList` — per-object FastCDC recipe

A chunked object's `ObjectLocation` (METADATA-DESIGN§2.1, with `LOCATION_FLAG_CHUNKED`) points at the **head**
of a `BtreeKind::ChunkList` chain. The head region carries `BTREE_NODE_FLAG_HEAD_OF_CHAIN`
in `BtreeNodeHeader.flags` (BTREE-DESIGN§1.1) and a fixed 16 B `ChunkParamsRecord` immediately after
the header — recording the chunking parameters used to produce the chain. Chained
(non-head) regions carry only the `BtreeNodeHeader`; their chunking parameters are inherited
from the head.

```rust
#[repr(C, packed)]
struct ChunkParamsRecord {                   // 16 bytes — head region only
    algo: u8,                                //  [0..1]   ChunkingAlgo discriminant
    flags: u8,                               //  [1..2]   reserved
    _pad: [u8; 2],                           //  [2..4]
    min_size: u32,                           //  [4..8]   plaintext bytes; ignored when algo == None
    avg_size: u32,                           //  [8..12]
    max_size: u32,                           // [12..16]
}

#[repr(u8)]
enum ChunkingAlgo {
    None      = 0,                           // not chunked — record exists for re-chunk overrides
    FixedSize = 1,                           // for resumability without CDC overhead
    FastCDC   = 2,                           // content-defined; default for the chunked path
    // 3+ reserved for future algorithms
}

struct ChunkListEntry {                      // 40 bytes (8-aligned)
    chunk_hash: [u8; 32],                    //  [0..32]   BLAKE3 of plaintext, indexes ChunkIndex
    length: u32,                             // [32..36]   plaintext length of this chunk
    flags: u32,                              // [36..40]   reserved (e.g. inline-tiny-chunk hint)
}
```

Region capacity:

```
Head region:
  262 144 − 64 (BtreeNodeHeader) − 16 (ChunkParamsRecord) − 16 (trailing chain BlockRef)
    = 262 048 B  ÷ 40 B/entry  = 6 551 entries

Chained regions:
  262 144 − 64 (BtreeNodeHeader) − 16 (trailing chain BlockRef)
    = 262 064 B  ÷ 40 B/entry  = 6 551 entries
```

Both shapes pack the same 6 551 entries — the head's `ChunkParamsRecord` occupies space that
is trailing pad in a chained region, so it costs no slots. The head region's first
entry sits at offset 80 (`64 + 16`); chained regions' first entry sits at offset 64.
Readers determine which shape they're looking at by inspecting
`BTREE_NODE_FLAG_HEAD_OF_CHAIN`.

Each entry covers a single FastCDC chunk (typical 4 – 64 KiB plaintext); a single region
therefore covers ≈ 26 MiB – 410 MiB of plaintext at the average chunk size. Larger objects
chain through the trailing `BlockRef` slot (same convention as `ForwardOverflow`, INDEX-DESIGN§2.2).
Reads stream chunks in order: walk the list, look up each chunk in `ChunkIndex` to find its
physical extent, fetch.

**Re-chunking after policy change.** When the resolved storage policy (SPECIFICATION-DESIGN§3.5) for an
object changes — typically because the operator updated a `PlacementRule::Chunk` or because
an ontology install bumped a tag's `storage.chunking` — reconcile (RECONCILE-DESIGN§1.1) detects the
mismatch by comparing the head region's `ChunkParamsRecord` to the resolved policy and
enqueues a `WorkKind::OptionUpdate` item. The re-chunk pass produces a fresh ChunkList
chain (with new params in its head) and atomically swaps `ObjectLocation.replicas[0]` to
the new head via `ReconcileMove`; the old chain becomes unreferenced and is reclaimed via
bucket-gen GC.

The list is **snapshot-agnostic** because chunks are content-addressed: two snapshots of the
same chunked object that happen to share a prefix simply share the prefix portion of their
ChunkList chains by the usual COW mechanism (a divergent suffix gets its own region; the
shared prefix is reachable from both).

---

