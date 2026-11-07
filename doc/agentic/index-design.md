# Indices — design

Scope: `crates/index` (term dictionary, term table, postings, forward index, sequences, chunk index and chunk lists), and the posting-container representation shared with `crates/query`, `crates/fuse`, `crates/watch`.
Governing spec: INDEX-DESIGN§1 (values and terms), INDEX-DESIGN§2 (forward), INDEX-DESIGN§3 (postings and sequences), INDEX-DESIGN§4 (dictionary, range scans, chunks), SPECIFICATION-DESIGN§5.

Plan: [index-implementation-plan.md](index-implementation-plan.md) · Open decisions: [index-decisions.md](index-decisions.md) · Completed: [index-completed.md](index-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Every assertion an object makes is a **term**; the index is an inverted index over terms — a sorted term dictionary and per-term posting lists — plus a forward list per object, all in one B+ tree format, read in place from mapped sectors with memory bounded independently of pool size.

## Selected design

- **Terms.** A tag, an attribute value under its key, and a relation to a target are terms. Tags keep their `TagId` as `TermId`; attribute and relation terms get dense `TermId`s when first written. A term is identified exactly by its dictionary key and value: there is no value hash, no hash secret and no collision case.
- **Six trees**, all BTREE-DESIGN§1 large-node trees:

  | Tree | Key → value | Section |
  |---|---|---|
  | Term dictionary | `(kind, key, context, NormalisedValue, term_id, snapshot)` → statistics + inline postings | INDEX-DESIGN§4.1 |
  | Term table | `term_id` → kind, key and full value | INDEX-DESIGN§4.1 |
  | Postings | `(term_id, id >> 16, snapshot)` → one roaring container | INDEX-DESIGN§3.1–3.2 |
  | Forward | `(oid, snapshot)` → the object's term ids | INDEX-DESIGN§2 |
  | Sequences | `(term_id, position, snapshot)` → oid | INDEX-DESIGN§3.3 |
  | Chunk index, chunk lists | chunk hash → extent; per-object chunk recipe | INDEX-DESIGN§4.3 |

- **Equality, range and prefix** queries are all dictionary lookups or scans: one ordered structure replaces a hash index and a separate range tree.
- **Posting lists** are roaring containers keyed by the high 48 bits of the object id, so ids carry full node and local identity, a small update rewrites one container, and snapshot diffs and sync deltas work per container. Lists of up to four objects live inline in their dictionary entry.
- **Bounded memory.** Readers walk trees through a page cache; no index is mirrored whole in memory.
- **TermIds are local to their pool.** Sync carries values, never term ids ([cluster-design.md](cluster-design.md)).

## Invariants

- Every posting of a term is in exactly one place: its dictionary entry's inline list or the postings tree, never both.
- Postings and forward lists agree: at each snapshot, an object is in a term's postings iff the term is in the object's forward list or is a tag implied by its direct tags under that snapshot's ontology (INDEX-DESIGN§2.4).
- A term's `TermId` is never reused, and two dictionary entries never denote the same term.
- Posting ids are full `ObjectId`s (16-bit node, 48-bit local).

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Values and terms

### 1.1 Value encoding

`Value` (the contents of attributes) is the only data type whose shape varies enough that fixed
binary layout is wasteful. It is encoded as positional **CBOR** (FORMAT-DESIGN§1.6): a
two-element array of an integer type tag and the payload, or three elements for `Scoped`:

```
Value ::= [0, text]                      // Text(string)      payload: CBOR major type 3
        | [1, int]                       // Int(i64)          payload: CBOR major type 0/1
        | [2, float]                     // Float(f64)        payload: CBOR major type 7, 64-bit
        | [3, int]                       // Timestamp(i64)    payload: CBOR major type 0/1
        | [4, bytes]                     // Blob(bytes)       payload: CBOR major type 2
        | [5, context, inner]            // Scoped { context: TagId (u32), inner: Value }  INDEX-DESIGN§1.3
```

Integers use the shortest CBOR form; a float is always written as a 64-bit CBOR float, so one
`Value` has exactly one encoding. Floats are canonicalised before encoding: `-0.0` becomes
`0.0`, and NaN is rejected as an attribute value.

### 1.2 Terms and TermIds

A **term** is one thing an object can assert:

| Kind | Term | `TermId` |
|---|---|---|
| `0` Tag | tag `t` | `t` (the `TagId`, `< 2³²`) |
| `1` Attr | `(key: TagId, value: Value)` | allocated, `≥ 2³²` |
| `2` Relation | `(predicate: TagId, target: ObjectId)` | allocated, `≥ 2³²` |
| `3`–`255` | reserved (derived terms such as substring n-grams) | allocated |

**Allocation.** Attribute and relation terms take `TermId`s from a per-pool counter starting at
2³², assigned in write order and never reused; the counter is not persisted but derived at boot
as one past the largest key in the term table, high-water markers included (see
"Reclamation"). Journal replay re-interns values in LSN order from the counter derived at the
committed root, so it assigns the same `TermId`s.

**Interning.** Writing an assertion looks its term up in the term dictionary (INDEX-DESIGN§4.1).
If present, the existing `TermId` is used; otherwise a `TermId` is allocated and a dictionary
entry and a term-table entry are inserted. Identity is exact — the dictionary compares full
values — so two different values are never the same term.

**Locality.** `TermId`s are meaningful only within their pool. Nothing that leaves the node
carries a `TermId`; sync carries values and receivers intern them.

**Reclamation.** A background scan reclaims a term when both hold:

- its cardinality is zero in every live snapshot, so no tree refers to it; and
- its dictionary entry's `last_modify_lsn` is older than the oldest retained journal entry
  (JOURNAL-DESIGN§1.6), so no retained history refers to it either.

Reclaiming removes the term's dictionary entries and its term-table record, journaled as one
`ReclaimTerms` entry (JOURNAL-DESIGN§1.3) so replay removes exactly the same terms. Its `TermId`
is never reused: if a reclaimed term holds the largest key in the term table, its record is
replaced by a **high-water marker** — a `TermRecord` of kind `TERM_KIND_HIGH_WATER` with no
value — in the same entry, so the boot-time counter cannot fall back below it. A marker is
removed once a larger key exists, by the next reclamation that touches the table. At most one
marker exists at a time.

### 1.3 Scoped values

`Value::Scoped { context, inner }` lets the same attribute key carry different values under
different scopes without inflating the tag namespace — e.g. an object's `unix-path` value
scoped by a `unix-path-context:rpi4-sdcard` tag. The pattern generalises: any attribute can be
scoped by any tag.

- A scoped value is its own term: `(key, Scoped { C, v })` and `(key, v)` are different terms,
  so they have different postings without branching at the lookup site.
- The dictionary key carries the scope in its `context` field (INDEX-DESIGN§4.1; `0` for unscoped
  values), ahead of the normalised inner value. A scan over `(Attr, key, context, *)` therefore
  lists every scoped value under one context — the manifest of a path projection — with no
  separate manifest structure.
- The query optimiser may rewrite `attr = K AND scope = C` as a lookup of `Scoped { C, K }`.

A `Scoped` whose inner value is also `Scoped` is a format error. Attributes that are never
context-scoped (e.g. `created_ns`) never use this variant.

**Type validation.** `Scoped` is **transparent to ontology type-checking**: an attribute
declared with `value_type = "text"` accepts both `Text(...)` and `Scoped { context, inner: Text(...) }`.
The validator strips the `Scoped` wrapper and checks `inner`. It additionally checks that
`context` is a `TagId` whose definition has `Grouping` semantics; non-grouping tags cannot be
used as scopes.

---

## 2. Forward index

The forward index maps each object to the `TermId`s it asserts **directly**. It answers
"everything about object X" (projection, inspection) and drives cleanup on deletion
(METADATA-DESIGN "Object deletion"). Implied tags are never stored per object: they are the
closure of the direct tags under the ontology (INDEX-DESIGN§2.4).

### 2.1 Entry layout

A BTREE-DESIGN§1 tree of `BtreeKind::Forward`, keyed by `(oid: u64, snapshot: u32)` through the
packed-key codec (BTREE-DESIGN§1.6), with `VARINT` value sizes. Each value is a `ForwardEntry`:
the object's direct terms in ascending `TermId` order, split into tag terms (`< 2³²`) and other terms,
each run stored as a frame of reference — a base plus fixed-width offsets — so the arrays are
aligned, binary-searchable in place and compact.

```rust
#[repr(C)]
struct ForwardHead {                         // 24 bytes
    tag_count:   u16,                        //  [0..2]   tag terms
    other_count: u16,                        //  [2..4]   attribute and relation terms
    tag_width:   u8,                         //  [4..5]   bytes per tag offset: 1, 2 or 4
    other_width: u8,                         //  [5..6]   bytes per other offset: 1, 2, 4 or 8
    flags:       u8,                         //  [6..7]   FORWARD_FLAG_*
    _pad0:       u8,                         //  [7..8]
    tag_base:    u32,                        //  [8..12]  smallest tag TermId
    _pad1:       u32,                        // [12..16]
    other_base:  u64,                        // [16..24]  smallest other TermId
}
// followed by, each zero-padded to a multiple of 8 bytes:
//   tag_offsets:   [u{8·tag_width};   tag_count]     TermId = tag_base + offset
//   other_offsets: [u{8·other_width}; other_count]   TermId = other_base + offset

const FORWARD_FLAG_OVERFLOW:  u8 = 1 << 0;   // arrays live in a ForwardOverflow chain
const FORWARD_FLAG_WHITEOUT:  u8 = 1 << 7;   // snapshot whiteout (SNAPSHOTS-DESIGN§1.3)
```

Each width is the smallest of the allowed widths that holds `max − base` for its run. A typical
object with 12 tags (ids within a few thousand of each other) and 3 attributes costs
24 + 24 (12 × u16) + 8 (3 × u16, padded) = **56 bytes**, however many tags those 12 imply.

### 2.2 Overflow

An entry whose arrays exceed 4 KiB (around 2 000 terms) sets `FORWARD_FLAG_OVERFLOW`: the value
holds only the `ForwardHead` followed by a `BlockRef`, and the arrays — laid out exactly as
above — fill one or more `ForwardOverflow` regions:

```rust
#[repr(C)]
struct ForwardOverflowRegion {               // 262 144 B = 256 KiB
    header:    BtreeNodeHeader,              //      [0..64]       kind = ForwardOverflow;
                                             //                    payload_used = bytes of arrays held
    payload:   [u8; 262_064],                //     [64..262_128]  continuation of the arrays
    next_page: BlockRef,                     // [262_128..262_144] ZERO = tail of chain
}
```

The arrays are the concatenation of each region's `payload[..payload_used]` in chain order; the
tail's `next_page` is zero, and a chain whose total length disagrees with the counts in the head
is a format error.

### 2.3 Behaviour

- **Append on flush.** A flush writes only a new sorted run holding the changed entries
  (BTREE-DESIGN§1.2); full compaction (BTREE-DESIGN§1.4) folds runs.
- **Lookup** is one tree descent; listing an object's values goes through the term table
  (INDEX-DESIGN§4.1) for each non-tag term.
- **Diffing** for sync streams sorted runs newer than the peer's watermark (SNAPSHOTS-DESIGN§1.2).

### 2.4 Implied tags

Implications connect tags only (SPECIFICATION-DESIGN§3.3), so attribute and relation terms are
always direct; only tags have implied members.

**Invariant.** Under the ontology image of snapshot *S* (ONTOLOGY-DESIGN§1), an object is in
`postings(t)` at *S* exactly when `t` is one of its direct tags or `t ∈ closure(direct tags)`.
Postings hold direct and implied members alike, so `HasTag` and `IsA` remain single posting
reads.

**Closure tables.** At mount, and whenever an ontology image is installed, the engine builds from
the implication DAG, for every tag `t`:

- `ancestors[t]` — every tag `t` implies, transitively, as an ascending `u32` array;
- `descendants[t]` — every tag that implies `t`, likewise.

They are derived state, never persisted (about 2 MB each at 100 000 tags with an average depth of
5). `closure(D) = D ∪ ⋃_{d ∈ D} ancestors[d]` merges a handful of short sorted arrays. An
implementation may cache closures by the hash of the direct tag set, since many objects share one.

**Maintenance:**

| Change | Postings work |
|---|---|
| Add direct tag `a` | add the object to `postings(t)` for each `t ∈ {a} ∪ ancestors[a]` it is not yet in |
| Remove direct tag `a` | `before = closure(D)`, `after = closure(D − {a})`; remove the object from `postings(t)` for each `t ∈ before − after` |
| Delete the object | remove it from `postings(t)` for each `t ∈ closure(D)` |
| New implication `a → b` | for each `t ∈ {b} ∪ ancestors[b]`: `postings(t) |= postings(a)` |
| Removed implication `a → b` | see below |

**Removing `a → b`**, with the new ontology's tables:

```text
keep     = ⋃ postings(d)  for every d ∈ descendants_new[b]     // still reach b another way
suspects = postings(a) − keep
for each object o in suspects:
    if b ∉ direct(o):                                            // one forward-entry read
        for each t ∈ closure_old(direct(o)) − closure_new(direct(o)):
            remove o from postings(t)
```

`keep` is sound because any object holding some `d` with `d → … → b` reaches `b` from its direct
tags. Forward entries are read only for `suspects` — the objects that may lose tags, whose
postings change anyway — so the work is proportional to the change, not to `postings(a)`.

Ontology changes rework postings in the same commit that installs the new image (ONTOLOGY-DESIGN
"Install", "Upgrade"), so the invariant holds at every committed root. Reads of an older snapshot
use that snapshot's image and tables.

---

## 3. Postings

### 3.1 Postings tree

A term's **posting list** is the set of objects asserting it. Lists of up to
`INLINE_POSTINGS_MAX = 4` objects live inline in the term's dictionary entry (INDEX-DESIGN§4.1).
Longer lists live in the postings tree, a BTREE-DESIGN§1 tree of `BtreeKind::Postings` keyed by

```
(term_id: u64, high: u64, snapshot: u32)     // high = ObjectId >> 16  (48 significant bits)
```

through the packed-key codec, one entry per non-empty 65 536-id window of the list. The value is
the window's **container** (INDEX-DESIGN§3.2).

- A list moves from inline to the tree when it gains its fifth object, and back to inline when
  it falls to two; the dictionary entry's `INLINE` flag says which holds it.
- Adding or removing one object rewrites one container entry. An empty container is removed.
- All entries of one term are contiguous in key order, so a query reads a term's containers in
  id order with one range scan, and two terms intersect by walking both scans in step and
  skipping windows absent from either.
- Snapshot diffs and bitmap-delta sync compare containers per `(term_id, high)`; the
  dictionary entry's `generation` and `last_modify_lsn` say whether a term changed at all.

### 3.2 Containers

A container holds the low 16 bits of each object id in its window, in the smallest of three
forms (the roaring container rules):

```rust
#[repr(C)]
struct ContainerHead {                       // 8 bytes
    kind:          u8,                       // [0..1]  CONTAINER_ARRAY | _BITMAP | _RUN
    _pad:          u8,                       // [1..2]
    entries_m1:    u16,                      // [2..4]  entries − 1 (array: ids; run: runs; bitmap: 1023)
    cardinality:   u32,                      // [4..8]  ids in this container, 1..=65 536
}
// followed by the payload, starting 8-byte aligned:
//   CONTAINER_ARRAY  = 0:  [u16; entries]                  ascending ids, padded to 8 bytes
//   CONTAINER_BITMAP = 1:  [u64; 1024]                     bit i of word w = id 64·w + i
//   CONTAINER_RUN    = 2:  [(start: u16, len_m1: u16); entries]  ascending, non-overlapping
//   CONTAINER_WHITEOUT = 0xFF: no payload — snapshot whiteout (SNAPSHOTS-DESIGN§1.3)
```

| Form | Used when | Cost |
|---|---|---|
| array | cardinality ≤ 4 096 and smaller than the run form | 2 B per id |
| run | 4 × runs < the array or bitmap size | 4 B per run |
| bitmap | otherwise | 8 KiB |

The writer picks the smallest form after every change, so one set has one encoding. Every
payload is an aligned array of `u16` or `u64`: readers borrow it in place from the mapped node
(FORMAT-DESIGN goal 1) and evaluate set operations with integer loads only. This requires the
postings tree's values to start 8-byte aligned within their node.

### 3.3 Ordered and ranked collections

Tags with `OrderedCollection` semantics (SPECIFICATION-DESIGN§3.1) keep their membership in
postings like any term, and their order in the sequences tree, a BTREE-DESIGN§1 tree of
`BtreeKind::Sequences`:

- **Ordered:** key `(term_id: u64, position: u64, snapshot: u32)` → `oid: u64`. Positions are
  sparse: appending takes the last position plus 2²⁰; inserting between two members takes the
  midpoint; when neighbours are adjacent, the term's positions are renumbered at 2²⁰ spacing in
  one rewrite.
- **Ranked:** key `(term_id: u64, score: u32, oid: u64, snapshot: u32)` → the byte `0x00`, where `score` is
  the order-preserving encoding of an `f32` (sign bit flipped for positives, all bits flipped for
  negatives), so a scan yields members by score.

---

## 4. Dictionary, range scans and chunks

### 4.1 Term dictionary and term table

**Term dictionary.** A BTREE-DESIGN§1 tree of `BtreeKind::TermDictionary`, keyed by

```
(kind: u8, key: u32, context: u32, value: NormalisedValue, term_id: u64, snapshot: u32)
```

through the packed-key codec.

- Tag terms: `kind = 0`, `key` = the tag, `context = 0`, `value` all zero, `term_id` = the tag.
- Attribute terms: `kind = 1`, `key` = the attribute's key tag, `context` = the scope tag of a
  `Scoped` value or `0`, `value` = `NormalisedValue` of the (inner) value (INDEX-DESIGN§4.2).
- Relation terms: `kind = 2`, `key` = the predicate, `context = 0`, `value` = the target's
  `ObjectId` big-endian in bytes `[0..8]`, zero elsewhere.
- `term_id` ends the key so that distinct values sharing a normalised prefix stay distinct
  entries, adjacent in key order.

Each value is a `DictEntry`:

```rust
#[repr(C)]
struct DictEntry {                           // 56 bytes
    cardinality:     u64,                    //  [0..8]   objects asserting the term
    last_modify_lsn: u64,                    //  [8..16]  LSN of the last change to its postings
    generation:      u32,                    // [16..20]  bumped on every change to its postings
    flags:           u8,                     // [20..21]  DICT_FLAG_*
    inline_count:    u8,                     // [21..22]  0..=INLINE_POSTINGS_MAX when INLINE
    _pad:            u16,                    // [22..24]
    inline:          [u64; 4],               // [24..56]  ObjectIds when INLINE, ascending; else zero
}

const DICT_FLAG_EXACT:  u8 = 1 << 0;         // NormalisedValue holds the whole value
const DICT_FLAG_INLINE: u8 = 1 << 1;         // postings are `inline`, not in the postings tree
const DICT_FLAG_WHITEOUT: u8 = 1 << 7;       // snapshot whiteout (SNAPSHOTS-DESIGN§1.3)
```

`(cardinality, generation, last_modify_lsn)` let the query planner order intersections and let a
peer see which terms changed without reading postings.

**Term table.** A BTREE-DESIGN§1 tree of `BtreeKind::TermTable`, keyed by `term_id: u64`
(snapshot-agnostic: a term never changes once created), holding what the dictionary key cannot:

```rust
#[repr(C)]
struct TermRecord {                          // 8-byte head
    kind:  u8,                               // [0..1]  as in the dictionary key
    flags: u8,                               // [1..2]  TERM_FLAG_OUT_OF_LINE
    _pad:  u16,                              // [2..4]
    key:   u32,                              // [4..8]  attribute key or predicate
}
// followed by:
//   Attr, inline:       the value's INDEX-DESIGN§1.1 encoding (≤ 96 bytes), padded to 8
//   Attr, out of line:  BlobRef (16 B) to a blob-zone extent holding the encoding
//   Relation:           target: u64
//   TERM_KIND_HIGH_WATER = 0xFF: nothing — a reclaimed term's marker (INDEX-DESIGN§1.2);
//                        key = 0; no dictionary entry refers to it; lookups never return it
```

Tag terms have no term-table entry; the ontology defines tags.

**Equality lookup** of `(key, v)`: normalise `v`; seek to `(1, key, context, norm, 0)`; for every
entry with that prefix, an `EXACT` entry matches outright, otherwise the term table's full value
is compared. The matching entry's postings are its inline list or its postings-tree range. Most
values (integers, floats, timestamps, text up to 14 bytes) are exact and take one descent.

**Interning** (INDEX-DESIGN§1.2) is the same lookup on the write path; a miss inserts the
dictionary entry and the term record.

### 4.2 Normalised values, range and prefix scans

`NormalisedValue` is a 16-byte order-preserving encoding: comparing two as unsigned byte strings
orders the values they encode.

| ValueType | Encoding (16 bytes) | Exact when |
|---|---|---|
| Int / Timestamp | big-endian `i64` with the sign bit flipped, then zeros | always |
| Float | big-endian IEEE 754 bits — positives with the sign bit flipped, negatives with all bits flipped — then zeros | always |
| Text | first 14 bytes of UTF-8, then `min(length, 255)`, then `1` if longer than 14 bytes else `0` | length ≤ 14 |
| Blob | first 14 bytes, then `min(length, 255)`, then a continuation byte as for Text | length ≤ 14 |

A **range** query `lo ≤ key < hi` scans the dictionary from `(1, key, context, norm(lo))` to
`(1, key, context, norm(hi))` and unions the postings of the entries it passes; entries whose
normalised value equals a bound's and are not `EXACT` are compared against the term table's full
value. A **prefix** query on text is the range from the prefix to its successor. Blob values are
equality-only: a range over Blob values is rejected by the query layer.

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
#[repr(C)]
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

## Footprint at scale

For 10 M objects with 5–20 tags each (125–200 M tag postings) and one unique `name` attribute
per object:

| Structure | Estimate |
|---|---|
| Postings | 150–400 MB for direct tags (2 B per id in arrays, ~1.25 B in dense bitmaps, far less in runs); × (1 + k) when each tag implies k ancestors |
| Forward | direct terms only: ~40–70 B per object → ~0.4–0.7 GB, independent of implications |
| Dictionary + term table | ~10 M attribute terms × ~100 B (key, entry with inline posting, term record) → ~1 GB; tags negligible |

Roughly 100–250 B per object in all. These are estimates from the layouts above and assumed tag
distributions.
