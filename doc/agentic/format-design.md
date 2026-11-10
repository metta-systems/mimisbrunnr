# On-disk format — design

The on-disk and in-memory format of a Mímisbrunnr pool, in format order. Cross-cutting rules (conventions, versioning, footprint, references) are specified here; every other part is specified in the numbered "Format specification" sections of the scope design document that owns it, and this document links to them in the order they compose a pool. "FORMAT-DESIGN§n" means section n of this document; "SCOPE-DESIGN§n" means section n of `<scope>-design.md`.

System design: [specification-design.md](specification-design.md) · Workspace conventions: [operations-design.md](operations-design.md) · Order of work: [operations-plan.md](operations-plan.md)

Together these sections specify the **on-disk** and **in-memory** data structures that realise the
[system design](specification-design.md). They are the contract between the storage layer, the index layer, and the
cluster-sync layer.

Goals:

1. **Zero-copy, compact access.** The pool is a filesystem read as disk sectors, often mapped as
   memory pages. A mapping of any bucket yields naturally aligned structures a reader borrows in
   place (`&T`, `&[T]`) — no copy and no deserialisation on the hot path (object lookup, tag and
   attribute lookup, query evaluation, blob read). Layouts are compact: no framing, padding or
   indirection a fixed layout can avoid.
2. **Format evolution.** Every persistent structure carries a version tag and a length so newer
   readers can skip unknown trailing fields and older readers can refuse cleanly.
3. **Atomic root updates.** Mutations land in the journal; a root is committed by a journal
   entry in a write-once journal set. A torn write never produces a corrupt root.
4. **Key-level snapshots.** Snapshots are 32-bit IDs embedded in the position of every key in
   snapshot-aware btrees. Creation is O(1); diffs are set differences over the snapshot tree's
   ancestor relation, driving cluster sync.
5. **No JSON on disk.** Fixed binary layouts everywhere they fit. CBOR (RFC 8949) only where the
   shape is genuinely heterogeneous (attribute `Value`s, query ASTs, ontology records), and
   then positional and versioned (FORMAT-DESIGN§1.6).

---


## 1. Format Conventions

### 1.1 Byte order, alignment, packing

- All multi-byte integers are **little-endian**.
- All fixed-size on-disk structs are `#[repr(C)]` with `Pod + Zeroable` from `bytemuck`, never
  `packed`. Every field sits at an offset that is a multiple of its own size, chosen by hand and
  documented inline (e.g. `// [12..20]`); explicit `_pad` arrays fill any gaps, so the compiler
  inserts no padding of its own. A struct's alignment never exceeds the alignment of its offset
  within the 4 KiB block, so a reader borrows it, or a slice of them, in place from a mapped or
  read sector (`bytemuck::try_from_bytes`, `try_cast_slice`). Targets that fault on unaligned
  access (`+strict-align`) read every field with aligned loads.
- No bit-packed fields cross a byte boundary (use a `u32` flags word, not C bitfields).
- All structs are sized as a multiple of 8 bytes; explicit `_pad` arrays document tail padding.
- Strings on disk are UTF-8, length-prefixed (`u16 len`), never NUL-terminated: they are short
  names and labels, each field with its own cap below 64 KiB. Bulk data is never a string but a
  byte array with its own length field (`u32` or `u64`).
- Every on-disk enum states its discriminants explicitly; none takes its value from declaration
  order.

### 1.2 Block size

- Logical block: **4 KiB**. Every persistent structure is laid out so that:
  - Records that must be updated atomically fit in one logical block, or
  - Records spanning multiple blocks use a shadow-paging / log-structured update.
- Block addresses are bucket-relative `(bucket_no, sector)` in 4 KiB sectors (ENGINE-DESIGN§1.3).

### 1.3 Preamble

Every persistent header begins with the same 8-byte `Preamble`, so a reader identifies any
structure from its first 8 bytes and dispatches to its parser:

```rust
#[repr(C)]
struct Preamble {                 // 8 bytes — first field of every persistent header
    magic: u32,                   // [0..4]  which structure (table below), little-endian ASCII
    version: u16,                 // [4..6]  that structure's layout version (FORMAT-DESIGN§15)
    kind: u16,                    // [6..8]  BtreeKind for a region; 0 for every other structure
}
```

| Magic | Structure | Defined in |
|---|---|---|
| `MIMR` | superblock | ENGINE-DESIGN§1.1 |
| `BREG` | B+ tree or positional region, block 0 | BTREE-DESIGN§1.1 |
| `BIDX` | run index block | BTREE-DESIGN§1.2 |
| `BPGS` | page-run index block | BTREE-DESIGN§1.7 |
| `JRNL` | journal set | JOURNAL-DESIGN§1.2 |

Each header ends with its `checksum: u64` (FORMAT-DESIGN§1.4), computed with that field zeroed.
Blocks without a header of their own — run data blocks, pages — have their checksum in the
index block's entry for them.

Regions (256 KiB B+ tree nodes and radix leaves; BTREE-DESIGN§1) carry their tree's
`BtreeKind` in `Preamble.kind`:

```rust
// Numbered contiguously, grouped by subsystem.
#[repr(u16)]
enum BtreeKind {
    // Object metadata
    ObjectRoots           = 0,   // METADATA-DESIGN§1.4 NodeId → object radix root (snapshot-agnostic)
    ObjectTable           = 1,   // METADATA-DESIGN§1 per-node radix leaves & inners (current view)
    ObjectHistory         = 2,   // SNAPSHOTS-DESIGN§1.2 sidecar: (oid, snapshot) → ObjectRecord overrides
    ExtentList            = 3,   // METADATA-DESIGN§1.3 shared extent B+ tree, (oid, end_offset, snapshot) → extent (snapshot-aware)
    // Term index
    Forward               = 4,   // INDEX-DESIGN§2 forward index B+ tree (snapshot-aware key)
    ForwardOverflow       = 5,   // INDEX-DESIGN§2.2 forward arrays of very large entries (positional, no sorted runs)
    TermDictionary        = 6,   // INDEX-DESIGN§4.1 term dictionary B+ tree (snapshot-aware)
    TermTable             = 7,   // INDEX-DESIGN§4.1 term table: term_id → kind, key, full value (snapshot-agnostic)
    Postings              = 8,   // INDEX-DESIGN§3.1 postings B+ tree, one container per entry (snapshot-aware)
    Sequences             = 9,   // INDEX-DESIGN§3.3 ordered / ranked collection order (snapshot-aware)
    // Chunk content-addressing
    ChunkIndex            = 10,  // INDEX-DESIGN§4.3 chunk index B+ tree (content-addressed; snapshot-agnostic)
    ChunkList             = 11,  // INDEX-DESIGN§4.3 shared chunk-list B+ tree, (oid, end_offset, snapshot) → chunk (snapshot-aware)
    // Physical space
    Backpointer           = 12,  // METADATA-DESIGN§2.2 reverse-mapping B+ tree (snapshot-agnostic)
    BucketAlloc           = 13,  // ALLOCATION-DESIGN§1.2 per-disk bucket alloc B+ tree (physical)
    FreespaceLru          = 14,  // ALLOCATION-DESIGN§1.4 per-disk freespace LRU B+ tree (physical)
    // Ontology and snapshots
    Ontology              = 15,  // ONTOLOGY-DESIGN§1 ontology records: tags, rules, modules (snapshot-aware)
    Snapshots             = 16,  // SNAPSHOTS-DESIGN§1.1 snapshot tree (SnapshotId → SnapshotNode)
    // Subscriptions
    Subscriptions         = 17,  // SUBSCRIPTIONS-DESIGN§1 subscription records (snapshot-agnostic)
    SubscriptionMembers   = 18,  // SUBSCRIPTIONS-DESIGN§1 (id, window) → membership container (snapshot-agnostic)
    SubscriptionEvents    = 19,  // SUBSCRIPTIONS-DESIGN§1 (id, lsn, index) → pending event (snapshot-agnostic)
    // Pool and cluster
    PoolState             = 20,  // POOL-DESIGN§1.2 single-entry pool state record
    DiskDescriptors       = 21,  // POOL-DESIGN§1.1 disk descriptors (disk_id → DiskDescriptor)
    PlacementRules        = 22,  // POOL-DESIGN§1 placement rules (heterogeneous, CBOR values)
    ClusterPeers          = 23,  // POOL-DESIGN§1 cluster peers (NodeId → PeerRecord)
    // Reconcile
    ReconcileWork         = 24,  // RECONCILE-DESIGN§1.2 queued work, by priority then physical position
    ReconcilePending      = 25,  // RECONCILE-DESIGN§1.2 failed items awaiting device-config retry
    ReconcileScan         = 26,  // RECONCILE-DESIGN§1.3 in-progress scan cursors
}
```

`Preamble.version` is per structure (and, for regions, per `BtreeKind`), not global: each evolves
independently. A reader that encounters an unknown `(magic, kind, version)` aborts with a clear
"format too new" error.

### 1.4 Checksums

Every header, run data block, page and journal-set checksum is a 64-bit slot (`u64`) holding
the pool-wide block checksum, whose algorithm is [OPERATIONS-D4](operations-decisions.md); an
algorithm with a 32-bit result is stored zero-extended, so the choice changes no layout. BLAKE3
is used for *content* integrity (per-object), not for block checksums.

### 1.5 Large node format

Specified in [BTREE-DESIGN§1](btree-design.md#1-large-node-format).

### 1.6 Records with variable parts, and CBOR

**Binary records.** A structure whose fields are known from its kind is a binary record, never
CBOR: a fixed `#[repr(C)]` head (FORMAT-DESIGN§1.1) followed by at most one variable part — an
array of fixed records whose count is in the head, or a byte string whose `u32` length is in the
head, zero-padded to a multiple of 8. WAL payloads (JOURNAL-DESIGN§1.3) are the main user.

**CBOR.** CBOR is reserved for genuinely heterogeneous data: attribute `Value`s
(INDEX-DESIGN§1), query ASTs (subscription records, placement-rule parameters) and the ontology
records (ONTOLOGY-DESIGN§1). It is **positional**:

- A struct is a CBOR array of its fields in the order its format section pins; no field names
  reach the disk.
- An enum is a CBOR array `[variant_index, fields…]` with the variant indices pinned by its
  format section; no variant names reach the disk.
- A CBOR map appears only where the data itself is a map (its keys are data, not field names).
- Integers use the shortest CBOR form (RFC 8949 §4.2.1 preferred serialisation), so equal
  values always encode to equal bytes.

**Versioning.** Every top-level CBOR record persisted outside the WAL — an ontology record, a
subscription record, a placement rule — is an array whose element 0 is a `u16` schema version.
Fields are only ever appended. A reader decodes every version up to its own, supplying each
field an older version lacks from the default its format section documents; a version newer
than the reader knows is a defined error, governed by the FORMAT-DESIGN§15 feature flags.
Rewriting a record always writes the current version, so records migrate in place as they are
rewritten (FORMAT-DESIGN§15.4). Values nested inside a record (`Value`, a query AST) are
versioned by that record; a `Value`'s own extension point is its type tag. Journal payloads are
versioned by the jset's `Preamble.version` instead (JOURNAL-DESIGN§1.2).

## 2. Superblock and Atomic Root

Specified in [ENGINE-DESIGN§1](engine-design.md#1-superblock-and-atomic-root).

## 3. Journal

Specified in [JOURNAL-DESIGN§1](journal-design.md#1-journal).

## 4. Values and Terms

Specified in [INDEX-DESIGN§1](index-design.md#1-values-and-terms).

## 5. Object Table

Specified in [METADATA-DESIGN§1](metadata-design.md#1-object-table).

## 6. Replicas and Backpointers

Specified in [METADATA-DESIGN§2](metadata-design.md#2-replicas-and-backpointers).

## 7. Forward Index

Specified in [INDEX-DESIGN§2](index-design.md#2-forward-index).

## 8. Postings

Specified in [INDEX-DESIGN§3](index-design.md#3-postings).

## 9. Term Dictionary, Range Scans, Chunk Index

Specified in [INDEX-DESIGN§4](index-design.md#4-dictionary-range-scans-and-chunks).

## 10. Subscriptions, Unix Paths, Ontology

These structures change less often than the index. Ontology records and subscription records are
loaded at mount; subscription membership and pending events are read on demand; Unix paths are
attribute terms with no structure of their own.

### 10.1 Ontology persistence

Specified in [ONTOLOGY-DESIGN§1](ontology-design.md#1-ontology-persistence).

### 10.2 Subscriptions

Specified in [SUBSCRIPTIONS-DESIGN§1](subscriptions-design.md#1-subscriptions).

### 10.3 Unix paths

Specified in [UNIX-DESIGN§1](unix-design.md#1-unix-paths).

### 10.4 Pool state

Specified in [POOL-DESIGN§1](pool-design.md#1-pool-state).

## 11. Snapshots and Cluster Sync

Specified in [SNAPSHOTS-DESIGN§1](snapshots-design.md#1-snapshots).

### 11.7 Sync bundles from snapshot diffs

Specified in [CLUSTER-DESIGN§1](cluster-design.md#1-sync-bundles-from-snapshot-diffs).

## 12. Allocation Layer

Specified in [ALLOCATION-DESIGN§1](allocation-design.md#1-allocation-layer).

## 13. In-Memory State

Specified in [ENGINE-DESIGN§2](engine-design.md#2-in-memory-state).

## 14. Encryption Integration

Specified in [ENCRYPTION-DESIGN§1](encryption-design.md#1-encryption-integration).

## 15. Format Versioning and Migration

The on-disk format evolves through a **two-version superblock**: the pool records both the
version it currently *writes* with and the minimum version of any data still *on disk*. The
two values move independently — new writes use the current version immediately, while older
data retains its original format until rewritten by background migration (FORMAT-DESIGN§15.4).
The fine-grained per-structure version is the `version` in each structure's `Preamble`
(per magic and, for regions, per kind), with `record_version` inside fixed records and the leading schema
version of positional CBOR records (FORMAT-DESIGN§1.6).

### 15.1 Superblock version fields

```rust
fs_format_version:   u32,        // version the pool currently writes with
fs_min_on_disk:      u32,        // smallest version any persistent record uses
compat_features:     u64,        // additive features old readers ignore safely
ro_compat_features:  u64,        // features old readers can read but not write
incompat_features:   u64,        // features old readers cannot read at all
downgrade_log_ref:   BlockRef,   // chain of historical features (FORMAT-DESIGN§15.5)
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

1. Picks the superblock copy with a valid checksum and the highest `checkpoint_hint.lsn`
   (ENGINE-DESIGN§1.1).
2. Refuses mount if `superblock.incompat_features &~ self.supported_incompat != 0`.
3. Mounts read-only if `superblock.ro_compat_features &~ self.supported_ro_compat != 0`.
4. Otherwise mounts read-write.
5. Optionally consults `downgrade_log_ref` (FORMAT-DESIGN§15.5) for historical incompat features that may
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

**Step 2 — migrate old data.** The reconcile engine (RECONCILE-DESIGN§1) launches a scan that walks all
snapshot-aware btrees, rewriting any node or record whose `Preamble.version` or `record_version`
is older than the current writing version. Each rewrite is COW; partial migrations are crash-safe.

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

For each structure (a `BtreeKind`, or a non-region magic), schema evolution follows one of three patterns:

1. **Additive** — new fields appended in reserved/padding regions; bump that structure's
   `Preamble.version`. Old readers ignore unknown bytes. Classified `compat`.
2. **Layout** — new on-disk shape; allocate a new `BtreeKind` (or bump a non-region structure's version) and keep the old
   one defined. The reconcile-driven migration (FORMAT-DESIGN§15.4) rewrites instances into the new kind.
   Mid-migration the pool may contain both shapes; new writes always use the new one. Typically
   `ro_compat` (old readers can still read the old shape) or `incompat` (depends on whether
   the new kind appears in critical paths).
3. **Semantic** — the meaning of an existing field changes. Always `incompat`; always requires
   `fs_format_version` advance. Old readers must not interpret the field under the old meaning.

---

## 16. Summary of On-Disk Footprint (10 M objects, 5 000 tags, ~100 live snapshots)

Steady-state footprint. The journal holds the unflushed tail, plus retained history only where a
pool sets a cap (JOURNAL-DESIGN§1.6); its bucket list is typically 64 MiB on a host. Per-key
snapshot overhead scales with logical changes since each snapshot's parent. The forward-index
sizing assumes 8 direct assertions per object (the INDEX-DESIGN§2.1 entry layout: a 32 B head
plus frame-of-reference arrays); small-object workloads (~4 assertions) cut the forward index by
about a third (~0.4 GiB at this scale). Tree sizes are for one copy of each node; with
`metadata_replicas` R every tree-node row is R times larger.

| Structure              | Size      | Notes                                              |
| ---------------------- | --------- | -------------------------------------------------- |
| Superblock × 3         | 12 KiB    | Fixed                                              |
| Journal                | ~64 MiB   | Journal buckets (JOURNAL-DESIGN§1); retained history only with a pool cap; per hot device holding the journal (mirroring, JOURNAL-D1) |
| Bucket alloc table     | ~160 MiB  | 16 M buckets × ~10 B/entry (8 B value + ~2 B packed `bucket_no`) + sorted-run overhead (BTREE-DESIGN§1.1 headers, format descriptors, tail padding) |
| Freespace LRU          | ~12 MiB   | Sparse; key packing on `(band, bucket_no)`         |
| Object table (records) | ~1.6 GiB  | Positional — 1.19 GiB of records in 6 511 leaves × 256 KiB (¾ of each region holds pages, the rest absorbs page runs); content locators included |
| Object table (radix, roots) | < 1 MiB | One inner node per origin node at R = 1 (depth 1), a few at R = 2 (depth 2, METADATA-DESIGN§1); `ObjectRoots` one leaf |
| Backpointers           | ~267 MiB  | METADATA-DESIGN§2.2 — 10 M extents × ~28 B packed                 |
| Forward index          | ~0.6 GiB  | INDEX-DESIGN§2 — ~58 B per object at 8 direct assertions/object (32 B head + frame-of-reference term arrays); implied tags are not stored. Includes any `unix-path` and other path-projection attributes (UNIX-DESIGN§1) — those are ordinary `Attr` terms. |
| Postings               | 200–400 MiB | INDEX-DESIGN§3 — roaring containers, one tree entry per (term, 65 536-id window); 5 000 tags |
| Term dictionary + term table | 0.1–1 GiB | INDEX-DESIGN§4.1 — ~100 B per distinct attribute value; the upper end is one unique value per object |
| Ontology               | <10 MiB   | One positional CBOR record per tag, rule, constraint and module, snapshot-tagged (ONTOLOGY-DESIGN§1) |
| Subscriptions          | ~700 KiB + membership + pending events | Per 1 000 subs with packed `sub_id`; membership as posting containers, ~2 B per member; ~32 B per pending event, bounded per subscription (SUBSCRIPTIONS-DESIGN§1) |
| Snapshots btree        | ~10 KiB   | 100 snapshot nodes × 64 B + skiplist overhead      |
| Snapshot key overhead  | ~80 MiB   | Per-snapshot divergent keys across the 6 snapshot-aware btrees (see breakdown below) |
| **Total metadata**     | **~2.9–3.9 GiB** | Replicated to every node; dominated by object records (1.19 GiB), the forward index and, with many distinct attribute values, the term dictionary |

**Snapshot key overhead breakdown.** "Snapshot key overhead" is the sum of *divergent* keys —
keys whose value at one snapshot differs from its value at an ancestor. Shared keys cost nothing
extra (one entry, visible in every snapshot whose ancestry reaches it via SNAPSHOTS-DESIGN§1.3). The ~80 MiB
figure assumes the canonical workload of 100 snapshots taken roughly hourly over an actively
mutated pool with **~5 000 mutations/snapshot** (≈ 0.05 % of the 10 M objects diverging per
snapshot, typical for sync-driven snapshotting):

| Source of divergence                 | Per snapshot      | × 100 snapshots |
| ------------------------------------ | ----------------- | --------------- |
| `ObjectHistory` sidecar (oid, snap → ObjectRecord override) | ~5 000 × 128 B = ~640 KiB | ~64 MiB |
| `Forward` divergent leaf entries     | ~5 000 × ~20 B packed = ~100 KiB | ~10 MiB |
| `Postings` divergent containers      | ~1 000 × ~0.5 KiB = ~500 KiB (one container per changed term window) | ~50 MiB |
| `TermDictionary` divergent entries   | ~1 000 × ~90 B = ~90 KiB (statistics of changed terms) | ~9 MiB |
| `Ontology`                          | usually 0 (the catalog changes rarely)     | <1 MiB          |
| **Sum** (≈ 1.35 MiB / snap × 100)    |                   | **~135 MiB raw** |

The raw arithmetic gives ~135 MiB; the table's ~80 MiB figure accounts for BTREE-DESIGN§1.6 packing
benefits (the trailing `snapshot` field packs to ~0 bits when one snapshot dominates a sorted
run) and the fact that not every divergent oid touches every snapshot-aware tree. Workloads
with **lower mutation rates** (cold archive snapshots taken daily) drop into the single-digit
MiB range; **higher rates** (~50 K mutations/snapshot, e.g. an actively edited dataset) push
this row into the GiB range — a deliberate tradeoff of snapshot frequency × pool churn versus
disk space, surfaced via SNAPSHOTS-DESIGN§1.7's retention policy.

**Transient overhead** (not in steady state — fills during specific events, drains afterwards):

| Structure              | Size       | Trigger                                           |
| ---------------------- | ---------- | ------------------------------------------------- |
| Reconcile trees        | < 1 MiB idle; a scan record per bulk event, plus the entries it could not act on at once | RECONCILE-DESIGN§1.10 |
| `ReconcileScan` cursors| < 1 MiB    | Per active scan; ~hundreds of bytes each          |
| Snapshot cleanup scans | < 1 KiB    | One cursor per snapshot-aware btree while a pass runs, not per key or snapshot (SNAPSHOTS-DESIGN§1.5) |

At moderate scale (10 M objects, the worked example here) metadata sits in the low single-digit
GiB; for typical workloads where average object size is ≥ 1 MiB, that's well under 1% of pool
storage. Small-object pools (logs, telemetry, tags-only) trade more of their footprint to
metadata but still amortise per record, since the per-object cost is fixed.

---

## 17. Reconcile

Specified in [RECONCILE-DESIGN§1](reconcile-design.md#1-reconcile).

## 18. References

- Roaring portable serialisation: <https://github.com/RoaringBitmap/RoaringFormatSpec>
- BLAKE3: <https://github.com/BLAKE3-team/BLAKE3-specs>
- HCTR2: <https://eprint.iacr.org/2021/1441>
- CBOR: RFC 8949
- CRC32C: RFC 3720 §12.1
- HKDF: RFC 5869
- Argon2id: RFC 9106
