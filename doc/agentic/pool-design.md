# Pool management — design

Scope: `crates/pool` (`PoolManager`, `PoolConfig`, placement rules, disk descriptors), `bin/brunnr`.
Governing spec: SPECIFICATION-DESIGN§8, POOL-DESIGN§1 (pool state root).

Plan: [pool-implementation-plan.md](pool-implementation-plan.md) · Open decisions: [pool-decisions.md](pool-decisions.md) · Completed: [pool-completed.md](pool-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

A pool spans any number of disks across tiers; placement and tiering follow placement rules over each object's assertion set, so retagging an object re-places it without a separate relocation call.

## Selected design

- `pool.toml` links `brunnr` and `mimir` to the same pool; `PoolManager::add_disk` updates the configuration it owns.
- Placement rules and disk descriptors persist in B+ tree regions referenced from the pool state root (`placement_rules_root`, `disks_overflow_root`); `cluster_peers_root` belongs to [cluster-design.md](cluster-design.md).
- Placement changes are carried out by reconcile autotier ([RECONCILE5](reconcile-implementation-plan.md)).

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Pool state

`PoolStateRoot` is a single 4 KiB block holding pool-wide scalars and an inline disk
descriptor array sized for the typical small-pool case:

```rust
#[repr(C, packed)]
struct PoolStateRoot {                        // 4096 bytes
    header: BlockHeader,                      //    [0..32]   kind = PoolStateRoot
    disk_count: u32,                          //   [32..36]
    cluster_node_count: u32,                  //   [36..40]
    inline_disks: [DiskDescriptorOnDisk; 12], //   [40..3112]  12 × 256 B
    _reserved: [u8; 980],                     // [3112..4092] room for pool-wide tunables
    // trailing CRC32C at [4092..4096] inside BlockHeader's frame
}
```

**Hybrid disk storage.** Per the typical use case (1–5 disks per pool), the inline array
covers normal operation without indirection; the cap of 12 leaves comfortable headroom for
modest multi-disk hosts (4–10 disks) before any spillover. Larger pools spill to a B+ tree:

- `disk_count ≤ 12`: all disks live in `inline_disks[0..disk_count]`. The
  `RootPointer.disks_overflow_root` (ENGINE-DESIGN§1.2) is zero; no overflow tree exists.
- `disk_count > 12`: all disks live in the **disks-overflow B+ tree**
  (`BtreeKind::DiskDescriptors`, keyed by `disk_id: u16`, rooted at
  `RootPointer.disks_overflow_root`). The `inline_disks[]` array is unused (zeroed).

Crossing the threshold (the 13th disk added) migrates the inline contents into the overflow
tree as a single batched btree insert. The reverse transition (disk_count dropping to 12 or
below) is **lazy** — the overflow tree may be retained until the next checkpoint or until
explicitly compacted; readers always check `disk_count` to know which side to consult.

A 256 KiB overflow leaf packs ~1 000 entries per sorted run (256 B per descriptor), so even
multi-thousand-disk clusters stay at depth 0–1.

**Disk descriptor (256 bytes):**

```rust
#[repr(C, packed)]
struct DiskDescriptorOnDisk {                // 256 bytes
    disk_id: u16,                            //  [0..2]
    media_type: u8,                          //  [2..3]
    tier: u8,                                //  [3..4]
    state: u8,                               //  [4..5]    DiskState
    _pad0: u8,                               //  [5..6]
    path_len: u8,                            //  [6..7]    valid byte count of path[]
    _pad1: u8,                               //  [7..8]
    capacity_bytes: u64,                     //  [8..16]
    used_bytes: u64,                         // [16..24]
    bucket_count: u32,                       // [24..28]   capacity_bytes >> bucket_size_log2
    first_usable_bucket: u32,                // [28..32]   = Superblock.first_usable_bucket
    buckets_root: BlockRef,                  // [32..48]   ALLOCATION-DESIGN§1.2 bucket alloc table root
    freespace_root: BlockRef,                // [48..64]   ALLOCATION-DESIGN§1.4 freespace LRU root
    path: [u8; 192],                         // [64..256]  UTF-8, NUL-padded; path_len bytes valid
}
```

The 192-byte inline `path` covers the full range of plausible disk identifiers: Linux device
names (`/dev/sda`, `/dev/nvme0n1p1`), full by-id / by-uuid symlinks
(`/dev/disk/by-id/nvme-Samsung_SSD_990_PRO_2TB_S6Z2NJ0X123456A`), NVMe-oF and iSCSI
discovery URLs, S3/HTTP endpoints with full bucket+key paths, etc. A `path_len` exceeding
192 is a format error; this would represent a genuinely pathological identifier and is
better surfaced explicitly than silently truncated.

The per-disk `buckets_root` and `freespace_root` are COW under the standard checkpoint
machinery — every RootPointer commit captures a self-consistent snapshot of every disk's
allocation state.

**Other pool-state btrees** (`placement_rules_root`, `cluster_peers_root`) live directly in
`RootPointer` (ENGINE-DESIGN§1.2). All btree roots are uniformly anchored at the top of the COW commit
machinery, so a checkpoint flips a single `RootPointer` and every tree advances atomically.

---

