# Reconcile — design

Scope: reconcile work-item trees, producers and the `reconcile_step` consumer in `crates/engine`, copy GC, autotier, replica repair, `mimir reconcile`.
Governing spec: RECONCILE-DESIGN§1 (RECONCILE-DESIGN§1.3 triggers, RECONCILE-DESIGN§1.5 move path, RECONCILE-DESIGN§1.7 self-healing, RECONCILE-DESIGN§1.9 operator interface), ALLOCATION-DESIGN§1.6 (copy GC), SPECIFICATION-DESIGN§17 references.

Plan: [reconcile-implementation-plan.md](reconcile-implementation-plan.md) · Open decisions: [reconcile-decisions.md](reconcile-decisions.md) · Completed: [reconcile-completed.md](reconcile-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

All background convergence (copy GC, autotiering, replica repair, snapshot cleanup) runs as one work-item system: producers enqueue `WorkItem`s into the reconcile trees, and `reconcile_step` drains them through a common move path.

## Selected design

- Work items live in `ReconcileWork`, `ReconcileHighPrio`, `ReconcileWorkPhys`, `ReconcileHighPrioPhys`, `ReconcilePending` and `ReconcileScan` trees.
- Producers: bucket-fill watermark (copy GC), replica drift (repair), tier residence versus policy (autotier), snapshot deletion scan.
- Moves go through backpointers so every reference to a moved extent is updated.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Reconcile

The reconcile subsystem is the unified, **state-driven** engine for background data maintenance.
A single mechanism handles operations that in other filesystems are separate threads with
separate state machines: replica repair, tier migration, option propagation, copy GC, disk
evacuation, auto-tiering, and erasure-coding promotion.

The engine compares **actual state** (where each extent is, how many replicas, which tier,
which compression / encryption) against **desired state** (placement rules, ontology-driven
options, replica counts) and queues work to close the gap. Multiple desired-state changes
compose naturally because each extent is evaluated independently.

### 1.1 What reconcile does

| Mismatch                                          | Action                                       |
| ------------------------------------------------- | -------------------------------------------- |
| Replica count below `Replicate { min_replicas }` rule | Re-replicate to additional disks         |
| Object on wrong tier vs. `Pin` / `Prefer` rule    | Migrate via the move path                    |
| Compression / encryption inconsistent with ontology | Rewrite with correct transform pipeline    |
| ChunkList head's `ChunkParamsRecord` ≠ resolved policy (INDEX-DESIGN§4.3, SPECIFICATION-DESIGN§3.5) | Re-chunk with new params via `WorkKind::OptionUpdate` |
| `AutoTier` access-time threshold crossed          | Migrate Hot → Warm → Cold → Glacier          |
| Bucket fragmentation > copygc threshold           | Run copy GC (ALLOCATION-DESIGN§1.6)                          |
| Disk in `Draining` state                          | Evacuate via backpointer scan (METADATA-DESIGN§2.2)         |
| Disk failure detected, replicas missing           | Resilver (CLUSTER-DESIGN§1) — re-replicate from peers   |
| Erasure-coding policy applies to cold data        | Encode into stripe (future)                  |
| Snapshot marked deleted (SNAPSHOTS-DESIGN§1.5)                   | Scan + drop / whiteout / re-tag keys at the deleted snapshot id |

Each is just a different `WorkKind` in the same queue. New mismatch types are additive.

### 1.2 Work-item btrees

Six BTREE-DESIGN§1 B+ trees, all `BtreeKind::Reconcile*` — five work queues plus `ReconcileScan` for
resumable scan cursors (described in RECONCILE-DESIGN§1.3):

```rust
enum WorkKind {
    ReplicaRepair    = 1,      // under-replicated; raise to target count
    TierMigrate      = 2,      // wrong tier per placement rule
    OptionUpdate     = 3,      // wrong compression/encryption
    Evacuate         = 4,      // on a Draining disk
    Copygc           = 5,      // fragmented bucket reclaim
    AutoTier         = 6,      // age-based migration
    EcEncode         = 7,      // promote to erasure-coding stripe
    SnapshotCleanup  = 8,      // SNAPSHOTS-DESIGN§1.5 — drop / whiteout / re-tag keys of a deleted snapshot
}

#[repr(C, packed)]
struct WorkItem {                            // 48 bytes
    target_kind: u8,                         // OwnerKind from METADATA-DESIGN§2.2
    work_kind: u8,                           // WorkKind
    attempt_count: u8,
    last_error_code: u8,
    flags: u32,                              // WORK_FLAG_*
    enqueued_lsn: u64,
    desired_state_ref: BlockRef,             // 16 B → CBOR(DesiredState) for variable detail
    owner_key: [u8; 16],                     // owning key in target btree (zero-padded per METADATA-DESIGN§2.2)
}

// WorkItem.flags bits
const WORK_FLAG_RATELIMITED: u32 = 1 << 0;  // throttle this item below pool default
const WORK_FLAG_PERSISTENT:  u32 = 1 << 1;  // do not drop on completion (audit trail)
```

| Btree                  | Ordering                              | Use                                          |
| ---------------------- | ------------------------------------- | -------------------------------------------- |
| `ReconcileWork`        | logical key (target_kind, owner_key)  | Default queue. Cheap on SSD where logical ≈ physical.|
| `ReconcileHighPrio`       | same                                  | High-priority items processed first.         |
| `ReconcileWorkPhys`    | physical LBA (disk_id, bucket, sector_offset) | HDD-backed pools — sequential processing avoids seeks. Maintained as a parallel index alongside `ReconcileWork`. |
| `ReconcileHighPrioPhys`   | same                                  | High-priority physical-order index.          |
| `ReconcilePending`     | logical key                           | Failed items. Retried only after device-config events; avoids spin loops on permanently-blocked work. |
| `ReconcileScan`        | scan_id                               | Resumable scan cursors (RECONCILE-DESIGN§1.3); not a work queue. One record per active scan. |

Whether to maintain `*_Phys` indexes is set per-disk via `DiskDescriptorOnDisk` (rotational
hint). Pure NVMe pools skip them.

### 1.3 Triggers

Work enters the queue via two paths:

**1. Per-key triggers.** Every snapshot-aware btree carries a trigger callback that fires on
insert / update / delete. The trigger compares the new state against the relevant rules and
emits a `WorkItem` to `ReconcileWork` (or `ReconcileHighPrio`) if a mismatch is observed.
Triggers are journalled like any other btree mutation (JOURNAL-DESIGN§1.4) — recovery replays them
deterministically.

**2. Scans.** A scan walks one or more btrees end-to-end, evaluating every key against current
desired state. Scans are launched by:

- **Device state change** — disk added, removed, evacuating, or transitioned to faulted.
- **Placement rule change** — admin updates `pool_state.placement_rules` (POOL-DESIGN§1).
- **Ontology change** — installation / upgrade alters compression, encryption, or tier
  selection for a tag.
- **Inode option change on a directory subtree** (e.g. a per-context override).
- **Snapshot marked deleted** — one `SnapshotCleanup` scan per snapshot-aware btree (SNAPSHOTS-DESIGN§1.5);
  the scan rewrites in-memory leaves only, never journalling per key.

Scan state lives in `ReconcileScan` records: `(scan_id, btree, cursor_key, originating_event)`.
A scan that crashes mid-walk resumes from `cursor_key` on the next mount. Scans that perform
in-place key mutations (e.g. `SnapshotCleanup`) are required to be idempotent under cursor
re-entry: a leaf that has already been rewritten must satisfy the scan's filter as a no-op,
so resuming from any earlier cursor produces the same final state.

### 1.4 Priority ordering

Work is processed strictly in this order:

1. `ReconcileHighPrio`, **metadata items** — under-replicated or evacuating btree-node /
   overflow / tag-bitmap extents.
2. `ReconcileHighPrio`, **data items** — under-replicated or evacuating blob / chunk extents.
3. `ReconcileWork` — normal metadata reconciliation.
4. `ReconcileWork` — normal data (tiering, option updates, optimisations,
   `SnapshotCleanup`).
5. `ReconcilePending` — retries (only when prerequisite device-config event has fired).

Tiers 1 and 2 share the `ReconcileHighPrio` btree; the worker discriminates by inspecting
`WorkItem.target_kind` (an `OwnerKind` from METADATA-DESIGN§2.2). `OwnerKind ∈ { BtreeNode, OverflowRecord }`
is metadata (tier 1); `OwnerKind ∈ { BlobExtent, Chunk }` is data
(tier 2). Workers consume the btree in `target_kind`-priority order rather than via two
separate scans, so the on-disk shape stays a single tree.

Within each tier, secondary ordering is logical (SSD) or physical (HDD), via the
`*_Phys` parallel index when present (RECONCILE-DESIGN§1.2). `SnapshotCleanup` runs at priority 4 — it
reclaims space but never blocks correctness or safety; under sustained pressure it yields to
copygc and tiering work and resumes from its cursor.

### 1.5 Move path

Reconcile shares one **move path** with copygc:

1. Read the extent from `from_loc` and validate (CRC32C + BLAKE3 at the object level for
   `OwnerKind::BlobExtent`).
2. Write the bytes to a fresh `to_loc`.
3. Emit one `ReconcileMove { from_loc, to_loc, owner_key }` WAL op (JOURNAL-DESIGN§1.3) — this is the
   atomicity boundary: a single LSN, one `payload_crc`, one fsync.
4. Remove the old work item from `ReconcileWork` / `ReconcileHighPrio`.

**Schema vs. replay.** `ReconcileMove` is what writers emit; the journal replay engine
projects it into the equivalent primitive btree mutations:

```
ReconcileMove { from_loc, to_loc, owner_key }
  ⇒ BackpointerRemove(from_loc → key)
   ∘ BackpointerInsert(to_loc → key, value)
   ∘ owner-update on owner_key:
        OwnerKind::BlobExtent      → ObjectLocation.replicas[i] (METADATA-DESIGN§2.1)
        OwnerKind::Chunk           → ChunkIndex entry's BlobRef (INDEX-DESIGN§4.3)
        OwnerKind::BtreeNode       → parent inner node's BlockRef (BTREE-DESIGN§1)
        OwnerKind::OverflowRecord  → ObjectRecord.overflow_offset (METADATA-DESIGN§1.2)
```

All three projected mutations apply under the move's LSN, so a crash either replays all of
them or none. `BackpointerInsert` / `BackpointerRemove` remain first-class WAL ops in JOURNAL-DESIGN§1.3
because they are also emitted directly by non-move callers (object deletion, fresh writes,
lazy btree-node free) — moves are the only flow that needs the bundled `ReconcileMove`.

**Type adaptation.** `BlockRef` is the uniform 16-byte handle used in the WAL ring
(`{disk_id, block_no, generation}`); the on-disk owner fields use narrower or differently-shaped
types for compactness. Replay demotes `to_loc: BlockRef` into the target's shape:

- `ReplicaRef` (METADATA-DESIGN§2.1, `BlobExtent`): split `block_no` into `(bucket_no, sector_offset)` per
  the METADATA-DESIGN§2.1 conversion; drop `generation` (recoverable on demand from `BucketAllocEntry`).
- `BlobRef` (INDEX-DESIGN§4.3, `Chunk`): drop `generation`; copy `length` from the *previous* owner
  record's `BlobRef.length` — moves preserve extent length, so the field is unchanged.
- `BlockRef` (`BtreeNode`): copy verbatim.
- raw `u64` (`OverflowRecord`): take `block_no`.

Writers do the inverse promotion when emitting `ReconcileMove`: they read the destination
bucket's current `BucketAllocEntry.generation` to populate `BlockRef.generation`, and any
`length` carried by the source field is dropped on the WAL ring (the WAL op carries no
`length` because none of its consumers — `BackpointerInsert`, the owner-update — needs one
beyond what the existing owner record already holds). The move-preserves-length invariant is
load-bearing here: a hypothetical future op that *changes* the plaintext length of a chunk
extent could not reuse `ReconcileMove` without an explicit `new_length` field.

Throttling: two pool-wide tunables in `pool_state`:

- `move_bytes_in_flight` (default 64 MiB) — total outstanding move I/O
- `move_ios_in_flight` (default 64) — concurrent move requests

Per-work-kind disable flags (`copygc_enabled`, `tiering_enabled`, …) allow administrative
control without unmounting.

### 1.6 Composition properties

Because reconcile is state-driven, multiple in-flight operations compose without coordination:

- **Evacuating two disks simultaneously** — each extent's desired state is evaluated against
  the current device topology; whichever evacuation reaches it first moves it correctly.
- **Tier policy change during evacuation** — the next evaluation of each extent considers both
  the new tier and the avoid-evacuating-disk constraint together.
- **Replica repair during copygc** — under-replicated data discovered mid-copygc gets a
  `ReplicaRepair` work item enqueued at high priority; copygc continues in parallel.

This is what bcachefs gains by replacing event-driven rebalance/resilver/tiering threads with
one state-driven engine.

### 1.7 Self-healing

If reconcile detects an inconsistency with **no obvious cause** (no recent option change, no
device event), it records an error in `pool_state.errors`: something unexpected happened that
needs operator attention. Degraded data is repaired regardless — the lack of a known cause
doesn't block the repair, only flags it for investigation.

Failed work items in `ReconcilePending` are retried only on device-configuration events (disk
added, state changed, capacity expanded). Without such an event, the prerequisites haven't
changed and a retry would just re-fail.

### 1.8 In-memory state

| Structure | Purpose |
| --- | --- |
| `ReconcileEngine` | Owns the four work btrees' write paths, the move-path semaphore, and the throttle counters. |
| `ScanRegistry` (`HashMap<ScanId, ScanState>`) | In-progress scans; cursor + estimated remaining work for status reporting. |
| `MoveInflight` (`Vec<InflightMove>`) | Bounded by the throttle; each entry carries the read buffer, target write point, and rollback handle. |

The engine runs as a small pool of worker threads (default: one per HDD, two per SSD) draining
the priority queue. Workers acquire the engine write lock only at trigger-firing and at
move-completion atomic-update — not during the read/write of moved data.

### 1.9 Operator interface (DESIGN-level)

`mimir reconcile status` reports per-priority queue depth, in-flight bytes, current scan
progress, and any pending-error items.

`mimir reconcile wait --type evacuate --disk N` blocks until all work of the given type for
the given target completes.

Per-workload pause / resume via `pool_state` flags (e.g. `reconcile_on_ac_only` to pause on
battery power for laptops).

### 1.10 Footprint

Work-item btrees are transient — fill during migration events, drain to near-empty in steady
state. Typical bounds:

- Idle pool: < 1 MiB across all reconcile btrees.
- Disk evacuation in progress (~10 M items, all of an evacuated disk's blob extents): ~458 MiB
  while in flight; drains to zero on completion.
- Cluster resilver after a disk failure: similar.

These are listed under "transient" in FORMAT-DESIGN§16, not in the steady-state footprint.

---

