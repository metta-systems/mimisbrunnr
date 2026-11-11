# Pool management — design

Scope: `crates/pool` (`PoolManager`, placement rules, disk descriptors, pool state), `bin/brunnr`.
Governing spec: SPECIFICATION-DESIGN§8, POOL-DESIGN§1 (pool state and disk descriptors).

Plan: [pool-implementation-plan.md](pool-implementation-plan.md) · Open decisions: [pool-decisions.md](pool-decisions.md) · Completed: [pool-completed.md](pool-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

A pool spans any number of disks across tiers; placement and tiering follow placement rules over each object's assertion set, so retagging an object re-places it without a separate relocation call: rules are watchers in the match index shared with subscriptions (SUBSCRIPTIONS-DESIGN "Evaluation") and match through the one per-object evaluator (QUERY-DESIGN "Per-object evaluation").

## Selected design

- **Identity from the devices.** A pool is the set of devices whose superblocks carry its `fs_uuid`; each device names itself by `disk_id` and `disk_uuid` (ENGINE-DESIGN§1.1), and the disk-descriptor tree lists every member. Opening a pool takes a set of block devices — whole disks, partitions or byte ranges of image files — and checks them against the descriptors. Nothing in the pool names a host path; host tools take device paths on their command line.
- **One disk-descriptor tree** keyed by `disk_id`, whatever the disk count: a few descriptors fit one leaf, so there is no inline array and no overflow mode.
- **Pool state is one versioned record** — tunables, throttles, default chunking, the error log — in its own single-entry tree.
- **Tree nodes are replicated** `metadata_replicas` times across disks, fastest tier first (ENGINE-DESIGN§1.2); the replica count is [POOL-D1](pool-decisions.md).
- Placement rules persist one entry per rule (`placement_rules_root`); `cluster_peers_root` belongs to [cluster-design.md](cluster-design.md).
- Placement changes are carried out by reconcile ([RECONCILE5](reconcile-implementation-plan.md)).

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Pool state

### 1.1 Disk descriptors

`RootPointer.disk_descriptors_root` (ENGINE-DESIGN§1.2) roots a BTREE-DESIGN§1 B+ tree of
`BtreeKind::DiskDescriptors`, snapshot-agnostic, keyed by `disk_id: u16`, with fixed-size values.
A disk's `disk_id` is drawn from the `Disk` id counter (ENGINE-DESIGN§1.2) when it is added and
never reused after it is removed, since `BlockRef`s, replica references and backpointers carry it;
placement-rule ids come from the `PlacementRule` counter the same way. The descriptor:

```rust
#[repr(C)]
struct DiskDescriptor {                      // 24 + 32 × R bytes (R = metadata_replicas)
    disk_uuid: [u8; 16],                     //  [0..16]   matches the device's Superblock.disk_uuid
    media_type: u8,                          // [16..17]   MediaType
    tier: u8,                                // [17..18]   StorageTier
    state: u8,                               // [18..19]   DiskState
    flags: u8,                               // [19..20]   DISK_FLAG_*
    bucket_count: u32,                       // [20..24]   copy of the device's capacity in buckets
    buckets_root: NodePtr,                   // [24..)     ALLOCATION-DESIGN§1.2 bucket alloc table
    freespace_root: NodePtr,                 //            ALLOCATION-DESIGN§1.4 freespace LRU
}

const DISK_FLAG_ROTATIONAL: u8 = 1 << 0;     // seek-bound: reconcile sweeps in physical order
```

`MediaType`, `StorageTier` and `DiskState` are SPECIFICATION-DESIGN§8.1's enums, stored as the
`u8` values pinned there. The descriptor holds what the pool decides about the disk —
its media class, tier and state — and the superblock what is fixed for the device: its geometry
and `disk_id` (ENGINE-DESIGN§1.1). The one copy is `bucket_count`, the device's capacity in
buckets, kept only so that a pool can display a missing disk's capacity. The superblock's geometry
is the source of truth and nothing reads `bucket_count` for allocation or validation; when an
opened device's capacity differs — a grown partition or virtual disk — the copy is refreshed at
the next checkpoint. Usage is the alloc table's. The per-disk allocation trees are COW under the standard checkpoint machinery, so every
checkpoint captures a self-consistent state of every disk's allocation.

**Opening a pool.** Read every supplied device's superblock; group by `fs_uuid`; load the root
list from the newest checkpoint among their journals (JOURNAL-DESIGN§1.5); then require that
every descriptor in state `Online` or `Draining` has a supplied device with its `disk_uuid`, and
that no supplied device of the pool lacks a descriptor. A missing or foreign device is a defined
error naming it.

### 1.2 Pool state record

`RootPointer.pool_state_root` roots a B+ tree of `BtreeKind::PoolState` holding one entry, key
`0`, whose value is positional, versioned CBOR (FORMAT-DESIGN§1.6), schema version 1:

```text
PoolStateRecord = [version,
                   cluster_node_count: uint,
                   default_chunking_threshold: uint,      // objects ≤ this size are one piece, never cut
                   default_chunking: bytes(16),           // ChunkParamsRecord (INDEX-DESIGN§4.3)
                   encoded_extent_max: uint,              // plaintext bytes per compressed extent,
                                                          //   default 131072 (METADATA-DESIGN§1.3)
                   rule_rework_inline_max: uint,          // estimated rework work units above which
                                                          //   a rule change runs as a resumable
                                                          //   rework (INDEX-DESIGN§2.4)
                   move_bytes_in_flight: uint,            // RECONCILE-DESIGN§1.5, default 64 MiB
                   move_ios_in_flight: uint,              // default 64
                   reconcile_flags: uint,                 // bit 0 copygc, 1 tiering, 2 repair,
                                                          //   3 ac-only (RECONCILE-DESIGN§1.9)
                   copygc_reserve_pct: uint,              // default 8, range 5..=21 (ALLOCATION-DESIGN§1.6)
                   region_size_log2: uint,                // new regions; default 18 = 256 KiB (BTREE-DESIGN§1)
                   errors: [ErrorEntry]]                  // newest 64 (RECONCILE-DESIGN§1.7)

ErrorEntry = [lsn: uint, time_ns: int, kind: uint, detail: bytes]
```

The default chunking policy applies when neither the ontology nor a placement rule resolves the
chunking axis (SPECIFICATION-DESIGN§3.5); the format-time default is no chunking.

**Other pool-state trees** (`placement_rules_root`, `cluster_peers_root`) are root-list entries
like every other tree (ENGINE-DESIGN§1.2), so one checkpoint advances them all atomically.

**Administrative changes commit by checkpoint.** Changes to the pool state record, the disk
descriptors, placement rules and cluster peers have no journal ops. An administrative command
applies its change to those trees and forces a checkpoint (JOURNAL-DESIGN§1.5) before it
returns; the change is durable exactly when that checkpoint is, and a crash before it leaves the
pool as it was. These changes are rare, so a checkpoint each costs nothing that matters.
Consequences that touch other state go through the ordinary paths: an `add-disk` or a new
placement rule enqueues reconcile work in the same checkpoint, and the moves it causes are
journalled as usual.

---
