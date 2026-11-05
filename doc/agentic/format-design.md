# On-disk format — design

The on-disk and in-memory format of a Mímisbrunnr pool, in format order. Cross-cutting rules (conventions, versioning, footprint, references) are specified here; every other part is specified in the numbered "Format specification" sections of the scope design document that owns it, and this document links to them in the order they compose a pool. "FORMAT-DESIGN§n" means section n of this document; "SCOPE-DESIGN§n" means section n of `<scope>-design.md`.

System design: [specification-design.md](specification-design.md) · Workspace conventions: [operations-design.md](operations-design.md) · Order of work: [operations-plan.md](operations-plan.md)

Together these sections specify the **on-disk** and **in-memory** data structures that realise the
[system design](specification-design.md). They are the contract between the storage layer, the index layer, and the
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
- All fixed-size on-disk structs are `#[repr(C)]` (or `#[repr(C, packed)]` — see below), with
  `Pod + Zeroable` from `bytemuck`. This permits `bytemuck::cast_slice` over an mmapped region
  for zero-copy traversal.
- `#[repr(C, packed)]` is the **common form** in this document. Every field is placed at an
  offset chosen by hand and documented inline (e.g. `// [12..20]`); explicit `_pad` arrays
  fill any gaps. Because the layout is already aligned by construction, `packed` strips
  Rust's implicit padding without changing the byte image and without introducing unaligned
  loads in practice — readers either `bytemuck::cast` whole records (which copies the bytes
  into an aligned local) or read individual fields via `read_unaligned` on platforms that
  need it. `#[repr(C)]` (no `packed`) is used when the struct is large and accessed
  field-at-a-time (e.g. `ObjectRecord`, `TagDirectory` leaf entries) so the compiler can
  emit aligned loads on hot paths.
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
(BTREE-DESIGN§1.1) — begins with the same 8-byte `BlockPreamble`. A generic reader can read those 8 bytes
to identify the structure (atomic 4 KiB block vs. append-only 256 KiB region) and dispatch to
the right parser:

```rust
#[repr(C, packed)]
struct BlockPreamble {            // 8 bytes — first field of every persistent header
    magic: [u8; 4],               // "MIMR" = 4 KiB block; "MIMB" = 256 KiB region (BTREE-DESIGN§1.1)
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
    flags: u32,                   // [28..32] BLOCK_FLAG_*
}

// BlockHeader.flags bits
const BLOCK_FLAG_ENCRYPTED:    u32 = 1 << 0;  // payload encrypted (XTS-AES-256)
const BLOCK_FLAG_CONTINUATION: u32 = 1 << 1;  // continuation of a chained record
```

A block is `BlockHeader | payload | u32 CRC32C(BlockHeader || payload)` — total = 4096 bytes by
construction. CRC is computed with the CRC slot itself zeroed.

`BlockKind` enumerates:

```rust
// Discriminants are pinned: any insertion goes at the end with the next free
// value; existing variants never renumber. On-disk records carry these
// numbers literally, so reordering is a format break. Variants are grouped
// here by functional role for readability — the numeric values, not the
// declaration order, are what's stable.
#[repr(u16)]
enum BlockKind {
    // Filesystem identity, on-disk geometry, and pool composition
    Superblock        = 0,   // per-disk identity + format pointers (ENGINE-DESIGN§1.1)
    ZoneMap           = 1,   // per-disk extra-extent zone descriptors (ENGINE-DESIGN§1.1)
    PoolStateRoot     = 2,   // pool-wide disk geometry + tunables (POOL-DESIGN§1)
    // WAL framing
    WalSegment        = 3,   // WAL header / segment marker
    Checkpoint        = 4,   // checkpoint block within the WAL
    // Tag-store pages
    TagBitmapPage     = 5,   // roaring bitmap framing (4 KiB; INDEX-DESIGN§3.2)
    SequencePage      = 6,   // INDEX-DESIGN§3.3 OrderedStore page (singly-linked chain)
    RankedPage        = 7,   // INDEX-DESIGN§3.3 RankedStore page (singly-linked chain)
    // KV equality pages
    KvHashDirectory   = 8,   // extendible-hash directory (4 KiB; INDEX-DESIGN§4.1)
    KvHashBucket      = 9,   // extendible-hash bucket (4 KiB; INDEX-DESIGN§4.1)
    // Per-object metadata overflow
    OverflowRecord    = 10,  // per-object tag/attr overflow (4 KiB; METADATA-DESIGN§1.2)
}
```

Large-node regions (256 KiB B+ tree nodes and radix leaves; BTREE-DESIGN§1) carry **`BtreeNodeHeader.kind`**
of type `BtreeKind` instead of `BlockHeader.kind`:

```rust
// Discriminants are pinned per BlockKind's rule: insertions append, existing
// variants never renumber. Grouped by functional role for readability.
#[repr(u16)]
enum BtreeKind {
    // Logical radix (current view + per-snapshot history sidecars)
    ObjectTable           = 0,   // METADATA-DESIGN§1 radix leaves & inners (current view)
    ObjectHistory         = 1,   // SNAPSHOTS-DESIGN§1.2 sidecar: (oid, snapshot) → ObjectRecord overrides
    LocationTable         = 2,   // METADATA-DESIGN§2.1 radix leaves & inners (current view)
    LocationHistory       = 3,   // SNAPSHOTS-DESIGN§1.2 sidecar: (oid, snapshot) → ObjectLocation overrides
    // Forward index family (oid → assertions)
    Forward               = 4,   // INDEX-DESIGN§2 forward index B+ tree (snapshot-aware key)
    ForwardOverflow       = 5,   // INDEX-DESIGN§2.2 per-object assertion spill (positional, no sorted runs)
    // Inverted / range indexes
    TagDirectory          = 6,   // INDEX-DESIGN§3.1 tag directory B+ tree (snapshot-aware)
    Range                 = 7,   // INDEX-DESIGN§4.2 range index B+ tree (snapshot-aware)
    // KV equality + value storage
    KvDirectory           = 8,   // INDEX-DESIGN§4.1 extendible-hash directory spillover (positional, no sorted runs)
    ValueSpill            = 9,   // value-hash → CBOR(Value); content-addressed
    // Chunk content-addressing
    ChunkIndex            = 10,  // INDEX-DESIGN§4.3 chunk index B+ tree (content-addressed; snapshot-agnostic)
    ChunkList             = 11,  // INDEX-DESIGN§4.3 per-object FastCDC chunk-list region (positional, no sorted runs)
    // Physical reverse mapping
    Backpointer           = 12,  // METADATA-DESIGN§2.2 reverse-mapping B+ tree (snapshot-agnostic)
    // Catalogs (snapshot-aware)
    Ontology              = 13,  // ONTOLOGY-DESIGN§1 ontology image tree (snapshot → CBOR(OntologyImage), snapshot-aware)
    Subscriptions         = 14,  // SUBSCRIPTIONS-DESIGN§1 subscription B+ tree (snapshot-aware)
    // Snapshot tree itself
    Snapshots             = 15,  // SNAPSHOTS-DESIGN§1.1 snapshot tree (SnapshotId → SnapshotNode)
    // Per-disk physical allocation
    BucketAlloc           = 16,  // ALLOCATION-DESIGN§1.2 per-disk bucket alloc B+ tree (physical)
    FreespaceLru          = 17,  // ALLOCATION-DESIGN§1.4 per-disk freespace LRU B+ tree (physical)
    // Pool / cluster state
    DiskDescriptors       = 18,  // POOL-DESIGN§1 disk descriptors overflow tree (>12 disks)
    PlacementRules        = 19,  // POOL-DESIGN§1 placement rules (heterogeneous, CBOR values)
    ClusterPeers          = 20,  // POOL-DESIGN§1 cluster peers (NodeId → PeerRecord)
    // Reconcile queues (transient)
    ReconcileWork         = 21,  // RECONCILE-DESIGN§1.2 normal-priority reconcile queue (logical order)
    ReconcileHighPrio     = 22,  // RECONCILE-DESIGN§1.2 high-priority reconcile queue
    ReconcileWorkPhys     = 23,  // RECONCILE-DESIGN§1.2 physical-LBA-ordered work index (HDD pools)
    ReconcileHighPrioPhys = 24,  // RECONCILE-DESIGN§1.2 physical-LBA-ordered high-prio index (HDD pools)
    ReconcilePending      = 25,  // RECONCILE-DESIGN§1.2 failed items awaiting device-config retry
    ReconcileScan         = 26,  // RECONCILE-DESIGN§1.3 in-progress scan cursors
}
```

The **format_version** is *per-kind*, not global. Old kinds are evolved independently. A reader
that encounters an unknown `(kind, format_version)` aborts with a clear "format too new" error.

### 1.4 CRC

CRC32C (Castagnoli, hardware-accelerated on x86 SSE 4.2 and ARMv8 CRC). Sufficient for 4 KiB blocks
and faster than CRC32. BLAKE3 is used for *content* integrity (per-object), not for block
checksums.

### 1.5 Large node format

Specified in [BTREE-DESIGN§1](btree-design.md#1-large-node-format).

## 2. Superblock and Atomic Root

Specified in [ENGINE-DESIGN§1](engine-design.md#1-superblock-and-atomic-root).

## 3. Write-Ahead Log

Specified in [JOURNAL-DESIGN§1](journal-design.md#1-write-ahead-log).

## 4. Heterogeneous Value Encoding

Specified in [INDEX-DESIGN§1](index-design.md#1-heterogeneous-value-encoding).

## 5. Object Record Table

Specified in [METADATA-DESIGN§1](metadata-design.md#1-object-record-table).

## 6. Location and Backpointer Tables

Specified in [METADATA-DESIGN§2](metadata-design.md#2-location-and-backpointer-tables).

## 7. Forward Index

Specified in [INDEX-DESIGN§2](index-design.md#2-forward-index).

## 8. Tag Inverted Index

Specified in [INDEX-DESIGN§3](index-design.md#3-tag-inverted-index).

## 9. KV Equality Index, Range Index, Chunk Index

Specified in [INDEX-DESIGN§4](index-design.md#4-kv-equality-index-range-index-chunk-index).

## 10. Subscriptions, Path Contexts, Ontology

These structures change less frequently and are loaded fully into memory at mount, but persisted
on every checkpoint.

### 10.1 Ontology persistence

Specified in [ONTOLOGY-DESIGN§1](ontology-design.md#1-ontology-persistence).

### 10.2 Subscriptions

Specified in [SUBSCRIPTIONS-DESIGN§1](subscriptions-design.md#1-subscriptions).

### 10.3 Path projections

Specified in [UNIX-DESIGN§1](unix-design.md#1-path-projections).

### 10.4 Pool state

Specified in [POOL-DESIGN§1](pool-design.md#1-pool-state).

## 11. Snapshots and Cluster Sync

Specified in [SNAPSHOTS-DESIGN§1](snapshots-design.md#1-snapshots).

### 11.7 Cluster diff between snapshots

Specified in [CLUSTER-DESIGN§1](cluster-design.md#1-cluster-diff-between-snapshots).

## 12. Allocation Layer

Specified in [ALLOCATION-DESIGN§1](allocation-design.md#1-allocation-layer).

## 13. In-Memory Mirror Structures

Specified in [ENGINE-DESIGN§2](engine-design.md#2-in-memory-mirror-structures).

## 14. Encryption Integration

Specified in [ENCRYPTION-DESIGN§1](encryption-design.md#1-encryption-integration).

## 15. Format Versioning and Migration

The on-disk format evolves through a **two-version superblock**: the pool records both the
version it currently *writes* with and the minimum version of any data still *on disk*. The
two values move independently — new writes use the current version immediately, while older
data retains its original format until rewritten by background migration (FORMAT-DESIGN§15.4).
`BlockHeader.format_version` (per-block, per-kind) remains the fine-grained per-structure
version.

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

1. Picks the active superblock copy by `(seq, lsn)` (ENGINE-DESIGN§1.2).
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
   one defined. The reconcile-driven migration (FORMAT-DESIGN§15.4) rewrites instances into the new kind.
   Mid-migration the pool may contain both shapes; new writes always use the new one. Typically
   `ro_compat` (old readers can still read the old shape) or `incompat` (depends on whether
   the new kind appears in critical paths).
3. **Semantic** — the meaning of an existing field changes. Always `incompat`; always requires
   `fs_format_version` advance. Old readers must not interpret the field under the old meaning.

---

## 16. Summary of On-Disk Footprint (10 M objects, 5 000 tags, ~100 live snapshots)

Steady-state footprint. The WAL ring holds the unmaterialised journal tail (≤ 64 MiB); per-key
snapshot overhead scales with logical changes since each snapshot's parent. The forward-index
sizing assumes 8 assertions per object (the INDEX-DESIGN§2.2 inline-spill threshold and the INDEX-DESIGN§2.1 calculation
basis), which is a worst-case upper bound — small-object workloads (~4 assertions) cut the
forward index roughly in half (~660 MiB at this scale).

| Structure              | Size      | Notes                                              |
| ---------------------- | --------- | -------------------------------------------------- |
| Superblock × 3         | 12 KiB    | Fixed                                              |
| WAL                    | 64 MiB    | Btree-update journal (JOURNAL-DESIGN§1); mirrored across devices |
| Bucket alloc table     | ~288 MiB  | 16 M buckets × ~18 B/entry (16 B value + ~2 B packed `bucket_no`) + sorted-run overhead (BTREE-DESIGN§1.1 headers, format descriptors, tail padding) |
| Freespace LRU          | ~12 MiB   | Sparse; key packing on `(band, bucket_no)`         |
| Object table (records) | ~1.19 GiB | Positional — no key packing applies; 4 893 leaves × 256 KiB |
| Object table (radix)   | < 1 MiB   | Single inner node (depth 1 total)                  |
| Location table         | ~460 MiB  | Positional — no key packing; 1 839 leaves × 256 KiB |
| Backpointers           | ~267 MiB  | METADATA-DESIGN§2.2 — 10 M extents × ~28 B packed                 |
| Forward index          | ~1.24 GiB | INDEX-DESIGN§2 / BTREE-DESIGN§1 B+ tree, 1 968 entries/leaf at 8 assertions/object → 5 082 leaves + 1 inner. Includes any `unix-path` and other path-projection attributes (UNIX-DESIGN§1) — those are ordinary `Attr` assertions. |
| Tag inverted index     | 200–400 MiB | Roaring bitmaps (4 KiB framed), 5 000 tags       |
| KV index               | ~100 MiB  | Extendible hash + roaring bitmaps                  |
| Range index            | ~20 MiB   | BTREE-DESIGN§1 B+ tree, packed (`attr_id` constant per leaf) |
| Ontology               | <10 MiB   | One CBOR(OntologyImage) per snapshot — modules + tag definitions + relations + implications (ONTOLOGY-DESIGN§1) |
| Subscriptions          | ~700 KiB  | Per 1 000 subs with packed `sub_id`                |
| Snapshots btree        | ~10 KiB   | 100 snapshot nodes × 64 B + skiplist overhead      |
| Snapshot key overhead  | ~50 MiB   | Per-snapshot divergent keys across the 7 snapshot-aware btrees (see breakdown below) |
| **Total metadata**     | **~4.0 GiB** | Replicated to every node; dominated by object records (1.19 GiB) and forward index (1.24 GiB) |

**Snapshot key overhead breakdown.** "Snapshot key overhead" is the sum of *divergent* keys —
keys whose value at one snapshot differs from its value at an ancestor. Shared keys cost nothing
extra (one entry, visible in every snapshot whose ancestry reaches it via SNAPSHOTS-DESIGN§1.3). The ~50 MiB
figure assumes the canonical workload of 100 snapshots taken roughly hourly over an actively
mutated pool with **~5 000 mutations/snapshot** (≈ 0.05 % of the 10 M objects diverging per
snapshot, typical for sync-driven snapshotting):

| Source of divergence                 | Per snapshot      | × 100 snapshots |
| ------------------------------------ | ----------------- | --------------- |
| `ObjectHistory` sidecar (oid, snap → ObjectRecord override) | ~5 000 × 128 B = ~640 KiB | ~64 MiB |
| `LocationHistory` sidecar (oid, snap → ObjectLocation override) | ~5 000 × 48 B = ~240 KiB | ~24 MiB |
| `Forward` divergent leaf entries     | ~5 000 × ~20 B packed = ~100 KiB | ~10 MiB |
| `Range` divergent leaf entries       | ~1 000 × ~25 B packed = ~25 KiB  | ~2.5 MiB |
| `TagDirectory` divergent entries     | ~50 × 48 B = ~2.5 KiB (tags rarely diverge per snapshot)  | ~250 KiB |
| `Ontology`, `Subscriptions`         | usually 0 (catalogs change rarely)         | <1 MiB combined |
| **Sum** (≈ 1 MiB / snap × 100)       |                   | **~100 MiB raw** |

The raw arithmetic gives ~100 MiB; the table's ~50 MiB figure accounts for BTREE-DESIGN§1.6 packing
benefits (the trailing `snapshot` field packs to ~0 bits when one snapshot dominates a sorted
run) and the fact that not every divergent oid touches every snapshot-aware tree. Workloads
with **lower mutation rates** (cold archive snapshots taken daily) drop into the single-digit
MiB range; **higher rates** (~50 K mutations/snapshot, e.g. an actively edited dataset) push
this row into the GiB range — a deliberate tradeoff of snapshot frequency × pool churn versus
disk space, surfaced via SNAPSHOTS-DESIGN§1.7's retention policy.

**Transient overhead** (not in steady state — fills during specific events, drains afterwards):

| Structure              | Size       | Trigger                                           |
| ---------------------- | ---------- | ------------------------------------------------- |
| Reconcile work btrees  | < 1 MiB idle, ~458 MiB during disk evacuation / cluster resilver | RECONCILE-DESIGN§1.10 |
| `ReconcileScan` cursors| < 1 MiB    | Per active scan; ~hundreds of bytes each          |
| `SnapshotCleanup` work | < 1 MiB    | Bounded by the count of snapshot-aware btrees, not key count (SNAPSHOTS-DESIGN§1.5) |

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
