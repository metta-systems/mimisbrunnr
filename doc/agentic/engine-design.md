# Engine — design

Scope: `crates/engine` (`Engine`, `DiskEngine`, commit, boot), excluding journal replay specifics ([journal-design.md](journal-design.md)).
Governing spec: SPECIFICATION-DESIGN§15, ENGINE-DESIGN§1.2 (root list and atomic commit), ENGINE-DESIGN§2 (in-memory state), FORMAT-DESIGN§16 (footprint).

Plan: [engine-implementation-plan.md](engine-implementation-plan.md) · Open decisions: [engine-decisions.md](engine-decisions.md) · Completed: [engine-completed.md](engine-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

The engine orchestrates mutations journal-first, keeps one long-lived `LoadedNode` per tree, flushes dirty trees by appending to write-once regions or copying them into fresh ones, and commits by a `Checkpoint` journal entry carrying the root list. Everything else on disk is derived or rebuilt at boot.

## Selected design

- **No zones.** A device is three superblock copies and buckets; every structure, metadata or data, lives in allocated buckets classified by bucket kind (ALLOCATION-DESIGN§1.2). There is no fixed index, metadata or blob area and nothing to grow.
- **Root list.** Every tree the engine owns is one entry of the root list, keyed by tree kind (ENGINE-DESIGN§1.2); an absent entry loads as an empty tree. The list travels in `Checkpoint` entries; the superblock holds only a hint to the newest one.
- **Commit.** Flush dirty trees (append a run or page run, or compact into a fresh region), then commit a `Checkpoint` entry with the new root list; regions the new root no longer reaches are freed after it is durable. A crash before the checkpoint leaves the previous root's regions intact.
- **No legacy blob.** The `MIXI` CBOR blob is retired: counters come from the checkpoint (`next_oid_local` from the pool's own `ObjectRoots` entry, every other id counter and `last_applied_lsn` from the latest checkpoint's root list head), path projections are answered from the index with nothing rebuilt (UNIX-DESIGN "Selected design"), and blob bytes live in data buckets.
- **Pools are disposable** until the maintainer declares the format stable: a format change means recreating the pool. Version fields stay in every structure so that pools after that point migrate (FORMAT-DESIGN§15).
- **One engine on host and device.** The engine and every crate it depends on build as `no_std` + `alloc` (operations-design "Deployment targets"); file-backed devices and other host parts sit behind the `std` feature. A pool opens from a `BlockDevice` alone, and opening replays the journal on every target, the device included.
- **Driven by its embedder.** The engine starts no thread. Mutations and reads are calls; background work — commits, checkpoints, flushes, reconcile — runs in `step` calls that do a bounded amount of work, with a non-blocking query of what is due. A `std`-gated helper drives them from threads on the host; the Vesper filesystem server drives them from its own loop.
- **Durability per call.** Every mutating call takes `Sync` (returns once durable) or `Buffered { max_delay }` (durable within the bound), and an explicit `commit` waits for everything before it (JOURNAL-DESIGN§1.5). The `BlockDevice` trait offers a write-through (FUA) write, a cache flush and a discard, each reporting whether the device supports it.

## Invariants

- A reopened pool reproduces exactly the committed state.
- The engine never hides another crate's invariants; it calls the owning crate's operations.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Superblock and Atomic Root

The superblock is the only structure with a fixed location and is written 3× (offsets 0, 4096, and
last-4096-of-device) for redundancy. The buckets holding those offsets are never allocated
(ALLOCATION-DESIGN§1.1).

### 1.1 Superblock layout (4 KiB, version 1)

```rust
#[repr(C)]
struct Superblock {                          // 4092 bytes; the checksum at [4092..4096] completes the 4 KiB block
    header: BlockHeader,                     //   [0..32]   kind = Superblock
    magic_full: [u8; 16],                    //  [32..48]   "MIMISBRUNNR\0\0\0\0\0"
    fs_uuid: [u8; 16],                       //  [48..64]   pool-wide UUID
    disk_uuid: [u8; 16],                     //  [64..80]   this device's UUID
    node_id: u16,                            //  [80..82]
    disk_id: u16,                            //  [82..84]
    media_type: u8,                          //  [84..85]   MediaType discriminator
    tier: u8,                                //  [85..86]   StorageTier discriminator
    block_size_log2: u8,                     //  [86..87]   12 == 4 KiB
    bucket_size_log2: u8,                    //  [87..88]   e.g. 20 = 1 MiB bucket
    device_capacity: u64,                    //  [88..96]
    creation_timestamp_ns: i64,              //  [96..104]
    last_mount_timestamp_ns: i64,            // [104..112]
    mount_count: u64,                        // [112..120]

    btree_node_size_log2: u8,                // [120..121]  default 18 = 256 KiB (BTREE-DESIGN§1)
    metadata_replicas: u8,                   // [121..122]  R: copies of every tree node (ENGINE-DESIGN§1.2)
    copygc_reserve_pct: u8,                  // [122..123]  default 8 (range 5..=21)
    _pad0: u8,                               // [123..124]
    first_usable_bucket: u32,                // [124..128]  first bucket after the one holding the superblocks

    // Checkpoint hint (ENGINE-DESIGN§1.2): where the newest Checkpoint entry was when this
    // superblock was written. Rewritten only on clean unmount and when the journal bucket
    // list changes.
    checkpoint_hint: CheckpointHint,         // [128..152]

    // Format versioning (FORMAT-DESIGN§15).
    encryption_keyid: [u8; 16],              // [152..168]  key identifier (not the key)
    fs_format_version: u32,                  // [168..172]  current writing version
    fs_min_on_disk: u32,                     // [172..176]  minimum version of any record on disk
    compat_features: u64,                    // [176..184]  old readers tolerate
    ro_compat_features: u64,                 // [184..192]  old readers mount RO
    incompat_features: u64,                  // [192..200]  old readers refuse
    downgrade_log_ref: BlockRef,             // [200..216]  chain of historical features

    // Journal location (JOURNAL-DESIGN§1.1).
    journal_bucket_count: u16,               // [216..218]  0 = this disk carries no journal
    _pad1: [u8; 6],                          // [218..224]
    journal_buckets: [u32; 256],             // [224..1248] bucket numbers in journal order;
                                             //             the first journal_bucket_count are valid
    _reserved: [u8; 2844],                   // [1248..4092] zeroed, available for future fields
    // trailing CRC32C at [4092..4096] lives inside BlockHeader's frame
}

#[repr(C)]
struct CheckpointHint {                      // 24 bytes
    jset_seq: u64,                           //  [0..8]    seq of the jset holding the Checkpoint
    lsn: u64,                                //  [8..16]   LSN of the Checkpoint entry
    journal_index: u16,                      // [16..18]   index into journal_buckets
    sector: u16,                             // [18..20]   sector of the jset within that bucket
    _reserved: u32,                          // [20..24]
}
```

`format_version` in the BlockHeader gates field interpretation. Reserved space is zero-initialised
so flipping bytes from 0 → 1 in a future version is a strict additive change. Pool-wide policy
(default chunking, throttles) lives in pool state (POOL-DESIGN§1), not in the superblock.

### 1.2 Root list and atomic commit

The **root list** names every tree's root. A root list is committed by a `Checkpoint` journal
entry carrying it (JOURNAL-DESIGN§1.5):

```rust
// Checkpoint entry payload (JOURNAL-DESIGN§1.3)
#[repr(C)]
struct RootListHead {                        // 88 bytes
    seq: u64,                                //  [0..8]    monotonic; larger seq wins
    lsn: u64,                                //  [8..16]   journal LSN this root corresponds to
    entry_count: u16,                        // [16..18]
    metadata_replicas: u8,                   // [18..19]   R, as in the superblock
    _pad: [u8; 5],                           // [19..24]
    id_counters: [u64; 8],                   // [24..88]   next id of each IdKind, indexed by it
}
// followed by entry_count × RootEntry, ascending by (tree, scope)

#[repr(u8)]
enum IdKind {                                // index into id_counters; first id at format
    Term          = 0,                       // TermId (INDEX-DESIGN§1.2), from 2³²
    Tag           = 1,                       // TagId (ONTOLOGY-DESIGN§1), from 1
    Rule          = 2,                       // rule id (ONTOLOGY-DESIGN§1), from 1
    Subscription  = 3,                       // SubscriptionId (SUBSCRIPTIONS-DESIGN§1), from 1
    Snapshot      = 4,                       // SnapshotId (SNAPSHOTS-DESIGN§1.1), after the root's
    PlacementRule = 5,                       // placement-rule id (POOL-DESIGN§1), from 1
    Disk          = 6,                       // disk_id (POOL-DESIGN§1.1), from 0; u16, so at most
                                             //   65 536 disks are ever added to a pool
    Scan          = 7,                       // reconcile scan_id (RECONCILE-DESIGN§1.3), from 1
}
```

**Id counters.** Every local id the pool allocates is drawn from its `id_counters` entry, which
then advances, and is never reused. The counters live in the root list head, outside every
snapshot-aware tree, so no deletion, snapshot cleanup or branch deletion can lower them.
Replay starts from the checkpoint's counters and advances each past every id the replayed
entries allocate (JOURNAL-DESIGN§1.3), so it assigns the same ids. An id kind whose width is
exhausted refuses further allocation with a defined error. `ObjectId`s are the exception: they
are allocated per origin node, so each node's counter is `ObjectRoots.next_local`
(METADATA-DESIGN§1.4), which sync raises for other nodes.

```rust
#[repr(C)]
struct RootEntry {                           // 8 + 16 × R bytes
    tree: u16,                               //  [0..2]   BtreeKind (FORMAT-DESIGN§1.3)
    _pad: u16,                               //  [2..4]
    scope: u32,                              //  [4..8]   0; reserved for trees that exist per scope
    root: NodePtr,                           //  [8..)    the tree's root node
}
```

Throughout the documents, `RootPointer.<name>_root` names the root-list entry of one tree:

| Name | `BtreeKind` | Section |
|---|---|---|
| `object_table_root` | `ObjectRoots` | METADATA-DESIGN§1.4 |
| `object_history_root` | `ObjectHistory` | SNAPSHOTS-DESIGN§1.2 |
| `forward_index_root` | `Forward` | INDEX-DESIGN§2 |
| `term_dictionary_root`, `term_table_root`, `postings_root`, `sequences_root` | `TermDictionary`, `TermTable`, `Postings`, `Sequences` | INDEX-DESIGN§3–§4 |
| `chunk_index_root` | `ChunkIndex` | INDEX-DESIGN§4.3 |
| `backpointer_root` | `Backpointer` | METADATA-DESIGN§2.2 |
| `ontology_root` | `Ontology` | ONTOLOGY-DESIGN§1 |
| `subscriptions_root`, `subscription_members_root`, `subscription_events_root` | `Subscriptions`, `SubscriptionMembers`, `SubscriptionEvents` | SUBSCRIPTIONS-DESIGN§1 |
| `snapshot_chain_root` | `Snapshots` | SNAPSHOTS-DESIGN§1.1 |
| `pool_state_root`, `disk_descriptors_root`, `placement_rules_root`, `cluster_peers_root` | `PoolState`, `DiskDescriptors`, `PlacementRules`, `ClusterPeers` | POOL-DESIGN§1 |
| `reconcile_work_root`, `reconcile_pending_root`, `reconcile_scan_root` | `ReconcileWork`, `ReconcilePending`, `ReconcileScan` | RECONCILE-DESIGN§1.2 |

Per-disk allocation trees are rooted in their disk descriptors (POOL-DESIGN§1), not in the list.

```rust
#[repr(C)]
struct BlockRef {                            // 16 bytes
    disk_id: u16,                            //  [0..2]
    sector: u16,                             //  [2..4]   4 KiB sector within the bucket
    bucket_no: u32,                          //  [4..8]
    generation: u32,                         //  [8..12]  bucket generation at write time (ALLOCATION-DESIGN§1.3)
    _reserved: u32,                          // [12..16]  zero
}

#[repr(C)]
struct BlobRef {                             // 16 bytes
    disk_id: u16,                            //  [0..2]
    sector: u16,                             //  [2..4]
    bucket_no: u32,                          //  [4..8]
    length: u64,                             //  [8..16]  bytes
}
```

Both are **bucket-relative**: their first 8 bytes have the `ReplicaRef` shape
(METADATA-DESIGN§2.1) and the backpointer key's fields, so a disk addresses `2³²` buckets —
4 PiB at the default 1 MiB bucket, 1 EiB at the 256 MiB maximum. A `BlockRef` names a tree node or region and
is checked against the bucket's generation on dereference. A `BlobRef` names a payload extent
(INDEX-DESIGN§4.1's out-of-line term values, INDEX-DESIGN§4.3's chunk index leaf); it carries a
length instead of a generation, and its freshness follows from the owning record's check.

**Node pointers.** A pointer to a tree node — a child slot in an inner node, a root-list entry,
a list head — is a `NodePtr`: `Superblock.metadata_replicas` (R, 1..=4, fixed at format time)
`BlockRef`s naming the R copies of the node, each on a different disk, the fastest tier first.
A node is written to all R places before any pointer to it is committed; a read takes the
first copy that verifies and repairs the others through reconcile. On a single-disk pool R = 1.
The default R and whether it may change after format are [POOL-D1](pool-decisions.md).

**Commit protocol** (invoked per the cadence in JOURNAL-DESIGN§1.5, *not* per mutation):

1. Flush every dirty tree up to the checkpoint LSN: append runs or page runs, compacting where
   BTREE-DESIGN§1.4 says, to all R copies (bucket opens journalled, ALLOCATION-DESIGN§1.5).
2. Commit a jset containing a `Checkpoint` entry with the new root list (JOURNAL-DESIGN§1.5),
   under the usual write barrier.
3. Later jsets carry `last_seq` at or after this checkpoint (unless pins or retention hold it
   back); regions the new root no longer reaches are freed (ALLOCATION-DESIGN§1.2).
4. On clean unmount, and whenever the journal bucket list changes, write the checkpoint hint
   into every superblock copy.

At mount, read the jset the hint names; the committed root is its `Checkpoint` or a newer one
found by the journal scan (JOURNAL-DESIGN§1.5), and the journal is replayed after its `lsn`.
After a clean unmount the hint names the last checkpoint and the journal tail is empty.

### 1.3 Block addressing

Within a device, a block is addressed bucket-relatively as `(bucket_no, sector)`; across the
pool, `(disk_id, bucket_no, sector)` identifies it. Devices may grow by adding buckets; bucket
numbers are never re-mapped.

**Positional access** for `ObjectRecord`s is preserved at the *per-leaf* level: the object's
origin node selects a radix root in `ObjectRoots`, the radix descent (METADATA-DESIGN§1) resolves
`oid_local` to a leaf, and the record lives at slot `oid_local % 1536` within that leaf. The
leaves are positional page regions (BTREE-DESIGN§1.7) reached through `NodePtr`s; there is no
device-wide flat array.

---

## 2. In-Memory State

The on-disk format is the source of truth. The in-memory layer is a **cache** plus a few
resident tables:

| In-memory type                               | Mirrors on-disk                           | Lifetime                  |
| -------------------------------------------- | ----------------------------------------- | ------------------------- |
| `ObjectTable` (positional accessor over `BTreeNodeCache`) | METADATA-DESIGN§1 per-node radix trees of `BtreeKind::ObjectTable` leaves; per-record access = (`ObjectRoots` entry → radix descent → leaf) → cache lookup of the 4 KiB page → 128 B slot at `oid_local % 1536` | pages cached per the block cache policy below; no separate resident array (~1.19 GiB of records at 10 M objects) |
| `ObjectRoots` | METADATA-DESIGN§1.4 — one entry per origin node | fully resident (a few entries) |
| `ObjectHistory` (snapshot-override sidecar) | SNAPSHOTS-DESIGN§1.2 — `BtreeKind::ObjectHistory` keyed by `(oid, snapshot)` | shares `BTreeNodeCache`; read for non-head snapshots |
| Term dictionary, term table, postings, sequences, forward index (accessors over `BTreeNodeCache`) | INDEX-DESIGN§2–§4 trees | no resident mirror; posting containers and forward entries read in place from cached blocks |
| `TermId`, `TagId`, rule id counters          | root list head (ENGINE-DESIGN§1.2), advanced by replay | resident scalars |
| `OntologyState` (name and uid maps, closure tables) | ONTOLOGY-DESIGN§1 ontology tree      | fully resident; closure tables rebuilt when `rules_epoch` advances |
| `SubscriptionEngine` (records, compiled queries) and the `MatchIndex` of subscriptions and placement and hydration rules (SUBSCRIPTIONS-DESIGN "Evaluation") | SUBSCRIPTIONS-DESIGN§1 subscription trees; rules from their pool trees | resident; membership containers and pending events read on demand |
| `PoolManager`                                | POOL-DESIGN§1 pool state and disk descriptors | fully resident     |
| Bucket generations (`Vec<u32>` per disk) | ALLOCATION-DESIGN§1.3, per-disk alloc table | fully resident (4 B per bucket) |
| `WritePoints` (`(disk, bucket kind, stream tag) → open bucket`) | derived; opens journalled as `BucketAlloc` | resident; ~hundreds of entries |
| `BTreeNodeCache` (`LoadedNode`s + LRU of 4 KiB blocks) | BTREE-DESIGN§1 nodes: headers and run index blocks per loaded node, data blocks and pages on demand | journal-pinned nodes never evicted; clean blocks LRU. The cache budget is set by the embedder — a few MiB on a device, hundreds on a host. |
| `SnapshotTree` (`SnapshotId → SnapshotNode` + ancestor cache) | SNAPSHOTS-DESIGN§1.1 snapshots btree | fully resident — typically thousands of entries; ancestor checks must be in-cache |
| `ReconcileState` (queue heads, active scans, throttle counters) | RECONCILE-DESIGN§1 reconcile trees | resident; queues paged from their trees |
| `JournalReclaim` (pin pressure per dirty node) | derived from `BTreeNodeCache` | resident; rebuilt on demand |

`Engine` (SPECIFICATION-DESIGN§15) owns these and is wrapped by `DiskEngine`, which adds the
block devices, superblocks, allocator and snapshot manager. All mutations follow the pipeline
below; see JOURNAL-DESIGN§1.4's *Locking model* for the lock domains referenced here:

```
1. Acquire the per-tree write mutex (JOURNAL-DESIGN§1.4) for the target btree.
2. Tag the mutation with the **current snapshot id** (SNAPSHOTS-DESIGN§1.2) — for snapshot-aware btrees, the
   key includes `snapshot = current_snapshot_id`.
3. Append the entry to the open journal set; it becomes durable with that set's commit, per the
   call's durability (JOURNAL-DESIGN§1.5).
4. Apply to the in-memory state (idempotent on lsn).
5. Release the per-tree write mutex.
6. Take each affected `LoadedNode`'s lock briefly in exclusive mode to add the entry to its
   journal pin. Node writes are deferred to the journal-reclaim step (JOURNAL-DESIGN§1.4).
```

The per-tree mutex covers steps 1–4 (LSN allocation, journal append, in-memory update); step 6's
per-node locks are independent and uncontended in steady state because a flush holds the same
lock in *shared* mode while building new runs. **Reads** see the in-memory state
as of the last completed mutation and are never blocked on either lock domain. On a
single-threaded embedder (the device) the locks are uncontended by construction.

### 2.1 Inode identity for FUSE

The FUSE bridge (`TagVfs`) does **not** use object IDs as inodes — it allocates 64-bit inodes
lazily as paths are traversed (a `BTreeMap<TagSet, u64>` and `BTreeMap<u64, TagVfsEntry>`). Inodes
are session-local; they are never persisted, since the TagVfs view is a derived projection.

---
