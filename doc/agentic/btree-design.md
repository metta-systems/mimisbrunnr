# B+ tree regions — design

Scope: `crates/storage/src/btree.rs`, `crates/storage/src/btree/`, `crates/storage/src/btree_node.rs`.
Governing spec: BTREE-DESIGN§1 (BTREE-DESIGN§1.1 region envelope, BTREE-DESIGN§1.2 append-only growth, BTREE-DESIGN§1.3 in-memory representation, BTREE-DESIGN§1.4 compaction, BTREE-DESIGN§1.5 bucket packing, BTREE-DESIGN§1.6 packed-key codec).

Plan: [btree-implementation-plan.md](btree-implementation-plan.md) · Open decisions: [btree-decisions.md](btree-decisions.md) · Completed: [btree-completed.md](btree-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

One generic region machinery that every persistent tree (forward, tag directory, range, KV directory, chunk, object/location, backpointer, bucket alloc, freespace, ontology, subscriptions, placement rules, disk descriptors) plugs into, with the spec's operational model: append a sorted run per flush, rewrite only the header sector, compact lazily.

## Selected design

- **Region envelope.** 256 KiB region: `BtreeNodeHeader` (64 B) followed by sorted runs, each a `SortedRunHeader` + `SortedRunKeyFormat` + payload, CRC32C per run.
- **Packed-key codec is the wire form** for every tree whose spec section mandates it (BTREE-DESIGN§1.6). CBOR sorted runs are used only where the spec says the value is CBOR; the codec is never swapped for convenience.
- **Elided value-prefix bytes are persisted inline** in `SortedRunKeyFormat`, directly after the field array; the encoder caps the prefix at the shortest value length and at `u8::MAX`. Readers need no caller-supplied template.
- **Value size kinds.** `value_size_kind` is `FIXED` or `VARINT` (LEB128 length prefix). The encoder auto-selects from value-length variance; callers with variable-shape values force `VARINT` so single-entry runs do not degrade to `FIXED`.
- **In-memory node.** `LoadedNode` holds the header, sorted runs, a lazily built merged view, the pending journal and a `JournalPin` covering the pending entries ([journal-design.md](journal-design.md)).
- **Covered key range.** `BtreeNodeHeader.min_key` / `max_key` carry the node's key range; flush, compaction and append keep them current. Inner-node descent depends on them.
- **Deletes are whiteouts** in the pending journal and in appended sorted runs; they survive flush and are dropped only by compaction ([btree-decisions.md](btree-decisions.md) BTREE-D1 for the encoding).
- `BlockKind` and `BtreeKind` discriminants are pinned by the spec.

## Invariants

- A region read returns exactly what was written; an unreadable or unexpected region is an error, never an empty node.
- Reading a region does not require the caller to know which codec wrote it.
- A flush never loses a pending insert or delete.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Large node format

All B+ tree nodes and radix-tree leaves are **256 KiB** (configurable 128–512 KiB at format time;
`Superblock.btree_node_size_log2`, default 18 = 256 KiB). This is the bcachefs model: shallow
trees, large sequential reads/writes, and node-internal log structure that lets a flush append
new keys without rewriting the whole node.

A 256 KiB node in a 1 MiB bucket means **4 nodes per bucket**, which is the typical packing.
Nodes do not span buckets; if a node would fill its region, it triggers a split or full
compaction (BTREE-DESIGN§1.4) that writes the new node(s) into a fresh bucket region.

Two large-node variants share the same outer envelope but differ in their internal layout:

- **B+ tree node** (forward index, term dictionary, term table, postings, sequences, alloc
  table, freespace LRU, ontology, subscriptions, chunk index, snapshots, disk descriptors,
  placement rules, cluster peers, reconcile queues): a sequence of **sorted runs** of keyed
  records. New updates append a new sorted run; periodic full compaction merges all of a node's
  sorted runs back into one.
- **Positional region** (object table, location table — radix leaves; *and* `ForwardOverflow`
  INDEX-DESIGN§2.2, `ChunkList` INDEX-DESIGN§4.3): a positional array of fixed-size
  records or `BlockRef` slots. No internal sorted runs — for radix leaves the WAL journal (JOURNAL-DESIGN§1.4)
  serves as the per-leaf update log and the leaf is rewritten from the merged in-memory state on
  flush; for the per-object overflow/list regions, growth is a single rewrite and chains via a
  trailing `BlockRef` slot.

> **Terminology.** A *sorted run* is the bcachefs concept of a `bset` ("btree set") — a single
> append-only sorted-by-key commit unit within a btree node. The on-disk magic tag for one is
> still `"BSET"` (BTREE-DESIGN§1.1); when cross-referencing bcachefs source, `bset` and `sorted run` name
> the same thing.

### 1.1 Region envelope

Every large node begins with a 64-byte `BtreeNodeHeader`:

```rust
#[repr(C, packed)]
struct BtreeNodeHeader {                     // 64 bytes
    pre: BlockPreamble,                      // [0..8]   magic = "MIMB", kind ∈ BtreeKind (FORMAT-DESIGN§1.3)
    seq: u64,                                // [8..16]  monotonic per-region; bumped on each rewrite
    last_persisted_lsn: u64,                 // [16..24] BlockHeader.lsn analogue
    region_size_log2: u8,                    // [24..25] 18 = 256 KiB
    level: u8,                               // [25..26] 0 = leaf, ≥1 = inner
    sorted_run_count: u8,                          // [26..27] number of sorted runs present
    flags: u8,                               // [27..28] BTREE_NODE_FLAG_*
    payload_used: u32,                       // [28..32] bytes consumed by all sorted runs so far (≤ region_size − 64)
    min_key: [u8; 16],                       // [32..48] covered key range (interpreted per-kind)
    max_key: [u8; 16],                       // [48..64] ditto
}

// BtreeNodeHeader.flags bits
const BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS: u8 = 1 << 0;  // recovery hint
const BTREE_NODE_FLAG_HEAD_OF_CHAIN:          u8 = 1 << 1;  // INDEX-DESIGN§4.3 — first ChunkList region
                                                            // of an object's chain (carries
                                                            // ChunkParamsRecord; chained regions
                                                            // do not)
```

`BlockPreamble` is the 8-byte common prefix shared with `BlockHeader` (FORMAT-DESIGN§1.3), so a generic
reader can identify any persistent header by its first 8 bytes. The two header shapes
**diverge after the preamble** because their storage models are genuinely different:

- `BlockHeader` describes a **write-once 4 KiB unit** with a single trailing CRC over the
  whole block. `payload_length` is bounded by the block size; `flags` carries per-block
  encryption / continuation bits.
- `BtreeNodeHeader` describes an **append-only 256 KiB region** that is rewritten only at
  its header sector (BTREE-DESIGN§1.2). There is no whole-region CRC — each sorted run carries its own CRC,
  so a torn append invalidates only the trailing sorted run rather than the whole region.
  `payload_used` is a high-water mark that grows monotonically across header rewrites within
  one region's lifetime; `flags` carries region-rewrite hints.

Forcing a single header shape onto both would require (a) inventing a "CRC that is not a
CRC" slot for regions, and (b) overloading `payload_length`/`flags` across two unrelated
semantic spaces. Keeping them distinct, with the shared preamble making the dispatch
explicit, is cleaner.

Subsequent bytes after the `BtreeNodeHeader` are a **stream of sorted runs**, each preceded by:

```rust
#[repr(C, packed)]
struct SortedRunHeader {                          // 32 bytes
    magic: u32,                              // "BSET" (bcachefs's name for a
                                             //   sorted run)
    seq: u32,                                // monotonic within the region
    journal_seq: u64,                        // newest WAL LSN merged into this sorted run (recovery)
    entry_count: u32,                        // entries in this sorted run
    payload_length: u32,                     // bytes of sorted run payload
    flags: u32,                              // SORTED_RUN_FLAG_*
    crc: u32,                                // CRC32C over (SortedRunHeader || payload), CRC slot zeroed
}

// SortedRunHeader.flags bits
const SORTED_RUN_FLAG_PACKED_KEYS: u32 = 1 << 0;  // entries use SortedRunKeyFormat encoding (BTREE-DESIGN§1.6)
const SORTED_RUN_FLAG_ENCRYPTED:   u32 = 1 << 1;  // sorted run payload encrypted
```

The CRC scope is **per-run**, not per-4 KiB-block. Each sorted run is therefore an independently
verifiable, append-only commit unit. A torn write of a partial sorted run fails its CRC and is
discarded — earlier sorted runs remain valid. There is no trailing CRC over the whole region.

### 1.2 Append-only growth

When the journal-reclaim thread (JOURNAL-DESIGN§1.4) decides to flush a node:

1. Materialise the pending journal entries that target this node into a new sorted run.
2. Append `SortedRunHeader` + sorted run payload at offset `payload_used` within the region.
3. Update `BtreeNodeHeader.sorted_run_count`, `payload_used`, `last_persisted_lsn`.
4. Rewrite **only** the modified bytes (the new sorted run, plus a re-checksummed header) — typically
   a few hundred KB, not the whole 256 KiB.

Because the bucket is write-once-then-recycle (ALLOCATION-DESIGN§1), the new sorted run lands at the next free sectors
of the bucket. The header rewrite at offset 0 is a single 4 KiB-sector overwrite — a permitted
operation because the region is reserved within the bucket and only the header sector is
re-touched.

> **Note.** Strictly write-once buckets cannot accept overwriting the header sector. Buckets
> hosting btree nodes are flagged `BucketDataType::BtreeNode` and tolerate **header-sector
> overwrites only** (the rest of the region remains append-only). On SMR / zoned drives this
> constraint moves header rewrites to a separate co-located header bucket; see ALLOCATION-DESIGN§1.5 for the
> zoned variant.

### 1.3 In-memory representation

Loaded nodes are decoded into:

```rust
struct LoadedNode {
    header: BtreeNodeHeader,
    sorted_runs: SmallVec<[SortedRun; 4]>,   // typically 1–3 active sorted runs
    merged_view: BTreeMap<Key, Value>,       // lazy: built on first lookup
    pending_journal: Vec<JournalEntry>,      // JOURNAL-DESIGN§1.4 entries past last_persisted_lsn
    dirty: bool,
    pin: JournalPin,                         // WAL pin for pending journal entries
}
```

**Key tracking for inner-node descent.** The `BtreeNodeHeader.min_key` and `BtreeNodeHeader.max_key`
fields (16 bytes each, [32..48] and [48..64]) carry the covered key range of the node. For leaf nodes
this is the smallest/largest key present; for inner nodes it is the routing key range used to
determine which child to descend into. Every write of the node keeps them exact:

- A flush sets them to the extremes of the pending journal entries merged with the existing
  sorted runs.
- Full compaction (BTREE-DESIGN§1.4) recomputes them from the single merged run.
- Appending a sorted run (BTREE-DESIGN§1.2) widens them to cover the new run's first and last
  key.

Lookups merge-search across sorted runs; each run is internally sorted at write time. With ≤ 3 active sorted runs
each binary-searched, lookup cost is `O(3 × log(n))` per node — equivalent to a single sorted
search at the constant-factor bcachefs measures at < 5% overhead.

### 1.4 Full compaction

When a node's `payload_used` exceeds 75 % of region size, or `sorted_run_count > 4`, full compaction
runs:

1. Allocate a fresh region in a new bucket (via the standard write-point mechanism, ALLOCATION-DESIGN§1.5).
2. Merge-sort all sorted runs into a single sorted run; write it as sorted run 0 in the new region.
3. Update the parent inner node's child pointer (which itself may need a flush — propagates
   up the tree).
4. Old region is abandoned; its bucket's `dirty_sectors` decreases. The bucket becomes a copygc
   candidate (ALLOCATION-DESIGN§1.6) when fragmentation is high enough.

Full compactions are O(node_size) per node but rare — bcachefs measures ~1 per 100–1000 flushes
on typical workloads.

### 1.5 Why this matters

For a 10 M-object pool with 5 000 tags and ~100 snapshots, every tree is shallow:

| Tree                | Depth  | Levels | Notes                                      |
| ------------------- | ------ | ------ | ------------------------------------------ |
| Object table (METADATA-DESIGN§1)   | 1      | 2      | 4 893 leaves under 1 inner                 |
| Location table (METADATA-DESIGN§2.1)| 1     | 2      | 1 839 leaves under 1 inner                 |
| Forward index (INDEX-DESIGN§2)  | 1      | 2      | ~2 500 leaves (~64 B per object) under 1 inner |
| Term dictionary (INDEX-DESIGN§4.1) | 0–1 | 1–2    | 5 000 tag entries fit one leaf; depth 1 once attribute values number in the millions |
| Postings (INDEX-DESIGN§3.1)     | 1      | 2      | one entry per (term, 65 536-id window)     |

(See the depth convention in METADATA-DESIGN§1: *depth* counts inner levels above the leaves; *levels* is the
total count — a depth-*N* tree has *N + 1* levels.) Each node access is a single sequential
I/O of 256 KiB — critical for HDD performance and friendly to SSD command queues. The cache
holds whole nodes, so intra-node lookups are memory-resident after the first hit.

### 1.6 Sorted-run format descriptors and packed keys

Each B+ tree sorted run (BTREE-DESIGN§1.1) carries a per-run **format descriptor** that exploits commonalities
across the sorted run's key range to compress keys end-to-end. Constant fields (e.g. `attr_id` within
a range-index leaf where every key shares the same attribute) consume **zero bits** and are
recorded once in the descriptor rather than per key.

```rust
#[repr(C, packed)]
struct SortedRunKeyFormat {                       // 8 + nr_fields × 16 + common_value_prefix bytes
    nr_fields: u8,                           // 1..=8
    key_header_bytes: u8,                    // 0..=4 (entry-type discriminator + flags)
    common_value_prefix: u8,                 // bytes shared at the start of every value (0..=255)
    value_size_kind: u8,                     // 0 = fixed; 1 = varint per-entry tail length
    sum_bit_width: u32,                      // total packed-key bits (incl. header), informational
    fields: [FieldFormat; nr_fields],
    value_prefix: [u8; common_value_prefix], // elided leading value bytes; readers
                                             // reconstruct each value as
                                             // `value_prefix || value_tail_i` without
                                             // any out-of-band state.
}

// SortedRunKeyFormat.value_size_kind values
const VALUE_SIZE_KIND_FIXED:  u8 = 0;        // every entry has the same value length;
                                             // no per-entry length prefix on the wire.
const VALUE_SIZE_KIND_VARINT: u8 = 1;        // each entry is preceded by an unsigned LEB128
                                             // varint giving the *elided-tail* length in bytes.

#[repr(C, packed)]
struct FieldFormat {                         // 16 bytes
    bit_width: u8,                           // 0..=64;  0 ⇒ constant (use `base` directly)
    flags: u8,                               // FIELD_FORMAT_FLAG_*
    _pad0: u16,
    base: u64,                               // value subtracted from each field at write time
    _pad1: u32,                              // tail pad to keep struct multiple-of-8 (FORMAT-DESIGN§1.1)
}

// FieldFormat.flags bits
const FIELD_FORMAT_FLAG_SIGNED:    u8 = 1 << 0;
const FIELD_FORMAT_FLAG_MSB_FIRST: u8 = 1 << 1;  // see byte-order note below
```

**Byte order.** The unflagged default is **little-endian** (matching FORMAT-DESIGN§1.1's invariant for all
multi-byte integers). `FIELD_FORMAT_FLAG_MSB_FIRST` selects big-endian / MSB-first packing for
fields whose ordering must match a byte-wise lexicographic compare on a multi-byte natural
encoding (e.g. UTF-8 string prefixes inside `NormalisedValue`). For ordinary integer fields the
flag is left clear; bit-packed values still compare correctly because the packed form is the
field's two's-complement value with `base` subtracted, monotonic by construction.

The descriptor is part of the `SortedRunHeader` payload (BTREE-DESIGN§1.1), prepended before the packed-key
stream. With the typical 3-field shape and a short `common_value_prefix`, it adds
8 + 48 + (0..255) = 56–311 bytes per sorted run; on real workloads `common_value_prefix` is
small (a few bytes for shared `store_kind` / leading-zero padding), so 60–80 bytes is typical
— amortised over hundreds to thousands of keys. The descriptor's on-disk length is
`8 + nr_fields × 16 + common_value_prefix` and is fully determined by the leading 8-byte
header; no alignment padding follows the `value_prefix` bytes (the per-key bit stream begins
immediately after, on a byte boundary).

**Encoding.** A packed key + value is:

```
[ key_header_bytes of type / flags ]
[ ∑ field[i].bit_width  bits of (field[i] − base[i]) for each field ]
[ pad to byte boundary ]
if value_size_kind == 0 (fixed):
    [ value tail bytes — exactly (value_size − common_value_prefix) bytes ]
if value_size_kind == 1 (varint):
    [ unsigned LEB128 varint giving tail_len in bytes ]
    [ value tail bytes — exactly tail_len bytes ]
```

Keys are laid out back-to-back with no inter-key padding. Binary search within a sorted run compares
packed keys **directly** without decoding — base subtraction is strictly monotonic, so packed
ordering matches unpacked ordering. Full decoding happens only at the lookup boundary. The
variable-length value tail comes *after* the packed key bytes and so does not affect ordering;
keys still sort the same way regardless of `value_size_kind`.

**`common_value_prefix` and reconstruction.** The elision applies only to bytes at the
**leading offsets** of the value that are bit-for-bit identical across **every entry in the
sorted run**. The encoder records both the length of the prefix (`common_value_prefix`) and
its bytes (`value_prefix[..common_value_prefix]`) in the descriptor, so the reader
reconstructs entry *i*'s full value as `descriptor.value_prefix || value_tail_i` without any
out-of-band state. For fixed-shape values (e.g. INDEX-DESIGN§4.1's 56-byte `DictEntry`) the
common prefix is whatever leading bytes every entry in the run shares — often none, since
`cardinality` comes first. For **variable-shape values** (notably INDEX-DESIGN§2.1's
`ForwardEntry`, whose arrays vary in length with the object's term count), the common prefix
can only cover bytes identical in every entry, so `common_value_prefix = 0` is the typical
setting. Format selection (BTREE-DESIGN§1.4) computes the prefix during
full compaction by scanning the merged sorted run's values and counting leading bytes shared
by every entry; if the sorted run mixes shapes, the count is bounded by the shortest value.
Single-entry sorted runs may set the prefix to the full value length without harm because
the bytes are persisted in the descriptor.

**Variable-size values.** When a sorted run mixes entries with different value lengths
(notably INDEX-DESIGN§2.1's `ForwardEntry`, INDEX-DESIGN§3.2's posting containers, and any other
heterogeneous-shape value such as
SUBSCRIPTIONS-DESIGN§1 `SubscriptionRecord` blobs that opt into the codec), the encoder sets
`value_size_kind = 1` (varint mode) and prefixes each per-entry value tail with an unsigned
LEB128 length giving the *elided tail's* byte count. The full value reconstructs as
`value_prefix || tail_bytes` and has length `common_value_prefix + tail_len`. Per-entry
overhead is 1 byte for tails ≤ 127 B, 2 bytes through 16 383 B, growing to a worst case of
10 bytes for the full `u64` range — dominated by the savings the codec recovers over the
CBOR fallback. Variable mode does not affect key ordering: keys are still bit-packed in
declaration order and binary search compares packed-key byte slices directly; the per-entry
length prefix and value tail come *after* the key bytes and are read only once a candidate
entry has been located.

Format selection picks `value_size_kind = 0` (fixed) when every entry's value length is
equal and `value_size_kind = 1` (varint) otherwise. The constraint
`value.len() ≥ common_value_prefix` for every entry holds in both modes; in variable mode
the encoder additionally verifies each value before writing. Random-access by entry index
is O(1) in fixed mode (constant stride) but O(N) in variable mode (sequential parse) —
acceptable because the heterogeneous-shape callers (forward leaves, subscription records)
read entire runs at a time rather than indexing into them.

**Format selection.** Full compaction (BTREE-DESIGN§1.4) computes an optimal format for the merged sorted run
by scanning the key distribution: `max − min` for each field gives the minimum bit width.
Append-on-flush keeps the existing format. If a new key's field overflows the format's bit
width, the flush either:

- Promotes the sorted run's format (rewriting the sorted run under a wider format — rare), or
- Triggers a full compaction of the node, producing a fresh format.

Format upgrades are journalled as a `FormatPromote` WAL op so recovery can reconstruct the
in-memory sorted run state.

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

Random-looking content hashes (chunk index, path-string hashes) bypass packing by encoding
each 64-bit limb of the hash as one `FieldFormat` with `bit_width = 64` and `base = 0` — a
32 B BLAKE3 chunk hash uses four such fields, an 8 B path-string hash uses one. The packed
form is then bit-for-bit identical to the unpacked hash bytes.

**Footprint impact.** Combined across the metadata zone, packing reduces B+ tree footprint by
~30% and improves cache utilisation proportionally — more keys per cache line means more keys
inspected per memory fetch during binary search. For runs in `value_size_kind = 1` (varint)
mode, per-entry overhead is typically 1 B (LEB128 for tail lengths ≤ 127 B). The variable-size
overhead is dwarfed by the codec's savings over the CBOR fallback for heterogeneous-shape
values (INDEX-DESIGN§2.1 forward entries are the canonical example).

---

