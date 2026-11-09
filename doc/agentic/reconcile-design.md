# Reconcile — design

Scope: reconcile trees, triggers, scans and the `reconcile_step` consumer in `crates/engine`, copy GC, autotier, replica repair, `mimir reconcile`.
Governing spec: RECONCILE-DESIGN§1 (RECONCILE-DESIGN§1.3 triggers and scans, RECONCILE-DESIGN§1.5 move path, RECONCILE-DESIGN§1.7 self-healing, RECONCILE-DESIGN§1.9 operator interface), ALLOCATION-DESIGN§1.6 (copy GC), SPECIFICATION-DESIGN§17 references.

Plan: [reconcile-implementation-plan.md](reconcile-implementation-plan.md) · Open decisions: [reconcile-decisions.md](reconcile-decisions.md) · Completed: [reconcile-completed.md](reconcile-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

All background convergence (copy GC, autotiering, replica repair, option propagation, snapshot cleanup, rule rework) runs as one state-driven system: bulk events walk the affected structures with resumable cursors, individual mismatches are queued as small work entries, and both drain through a common move path.

## Selected design

- **Three trees.** `ReconcileWork` (queued mismatches, keyed by priority and physical position), `ReconcilePending` (items waiting for a device event) and `ReconcileScan` (cursors of running scans).
- **Bulk events are scans, not queues.** Evacuating a disk walks its backpointer range; a placement-rule or ontology change walks the postings of the affected terms; snapshot cleanup walks the snapshot-aware trees; rule rework and term reclamation walk their keys. A scan holds one cursor however much it covers.
- **Desired state is recomputed when work runs.** Work entries name what to look at, never what to do; the worker evaluates placement rules, ontology policy and device state at the moment it acts, so stale intentions cannot be applied.
- **Physical order everywhere.** Work keys sort by priority, then `(disk, bucket, sector)`, so workers sweep each disk sequentially — necessary on HDD, harmless on flash.
- **Driven by `step`.** Reconcile runs inside the engine's `step` calls with a work budget (ENGINE-DESIGN "Selected design"); on the host a helper may run several steps in parallel threads.
- **One move path** updates every owner of a moved location — all versions of a record, every object of a packed block — under one journal entry.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Reconcile

The reconcile subsystem is the unified, **state-driven** engine for background data maintenance.
A single mechanism handles operations that in other filesystems are separate threads with
separate state machines: replica repair, tier migration, option propagation, copy GC, disk
evacuation, auto-tiering, snapshot cleanup and erasure-coding promotion.

The engine compares **actual state** (where each extent is, how many replicas, which tier,
which compression / encryption) against **desired state** (placement rules, ontology-driven
options, replica counts) and closes the gap. Multiple desired-state changes compose naturally
because each extent is evaluated independently, at the time it is processed.

### 1.1 What reconcile does

| Mismatch                                          | Action                                       |
| ------------------------------------------------- | -------------------------------------------- |
| Replica count below `Replicate { min_replicas }` rule | Re-replicate to additional disks         |
| Object on wrong tier vs. `Pin` / `Prefer` rule    | Migrate via the move path                    |
| Compression / encryption inconsistent with ontology | Rewrite with correct transform pipeline    |
| `Inline` object whose resolved policy now sets an encryption mode (METADATA-DESIGN§1.2) | Move the content out of the record, encrypted as the policy says |
| ChunkList locator's `ChunkParamsRecord` ≠ resolved policy (INDEX-DESIGN§4.3, SPECIFICATION-DESIGN§3.5) | Re-chunk with new params (a content rewrite) |
| `AutoTier` access-time threshold crossed          | Migrate Hot → Warm → Cold → Glacier          |
| Bucket fragmentation > copygc threshold           | Run copy GC (ALLOCATION-DESIGN§1.6)          |
| Disk in `Draining` state                          | Evacuate via backpointer scan (METADATA-DESIGN§2.2) |
| Disk failure detected, replicas missing           | Resilver (CLUSTER-DESIGN§1) — re-replicate from peers |
| Tree-node copy fails verification (ENGINE-DESIGN§1.2) | Rewrite the copy from a good one          |
| Snapshot marked deleted (SNAPSHOTS-DESIGN§1.5)    | Cleanup pass: drop / whiteout / re-tag keys at deleted ids |
| Rollback (SNAPSHOTS-DESIGN§1.8)                   | Re-head the object table                     |
| Ontology rule change (INDEX-DESIGN§2.4)           | Rework derived postings                      |
| Terms with no references (INDEX-DESIGN§1.2)       | Reclaim terms                                |
| Erasure-coding policy applies to cold data        | Encode into stripe (future)                  |

New mismatch types are additive: a new work kind or a new scan kind.

### 1.2 Reconcile trees

Three BTREE-DESIGN§1 B+ trees:

```rust
#[repr(u8)]
enum WorkKind {
    ReplicaRepair    = 1,      // under-replicated; raise to target count
    TierMigrate      = 2,      // wrong tier per placement rule
    OptionUpdate     = 3,      // wrong compression / encryption / chunking / inline placement
    Copygc           = 5,      // fragmented bucket reclaim
    AutoTier         = 6,      // age-based migration
    EcEncode         = 7,      // promote to erasure-coding stripe
    NodeRepair       = 9,      // tree-node copy failed verification
    // 4, 8 unassigned (evacuation and snapshot cleanup are scans, RECONCILE-DESIGN§1.3)
}

// ReconcileWork and ReconcilePending key — packed-key codec (BTREE-DESIGN§1.6)
#[repr(C)]
struct WorkKey {                             // 12 bytes
    priority: u8,                            //  [0..1]   0 = first (RECONCILE-DESIGN§1.4)
    _pad: u8,                                //  [1..2]
    disk_id: u16,                            //  [2..4]   location of the work: the extent,
    bucket_no: u32,                          //  [4..8]     bucket or node it concerns
    sector: u16,                             //  [8..10]
    work_kind: u8,                           // [10..11]
    _pad1: u8,                               // [11..12]
}

#[repr(C)]
struct WorkValue {                           // 24 bytes
    owner_kind: u8,                          //  [0..1]   OwnerKind (METADATA-DESIGN§2.2)
    attempt_count: u8,                       //  [1..2]
    last_error: u16,                         //  [2..4]   0 = none
    _pad: u32,                               //  [4..8]
    enqueued_lsn: u64,                       //  [8..16]
    owner_key: [u8; 8],                      // [16..24]  ObjectId or chunk-hash prefix; the
                                             //           backpointer supplies the rest
}
```

| Btree              | Key | Use |
| ------------------ | --- | --- |
| `ReconcileWork`    | `WorkKey` | Queued mismatches found by triggers and scans, processed in key order |
| `ReconcilePending` | `WorkKey` | Items that failed for lack of a device; retried after a device-configuration event |
| `ReconcileScan`    | `scan_id: u64` | One `ScanRecord` per running scan (RECONCILE-DESIGN§1.3) |

A work entry records only where to look. The worker reads the current owner through the
backpointer at the key's location and evaluates desired state then; if nothing is wrong any
more, the entry is dropped. No desired state is stored.

### 1.3 Triggers and scans

Work enters through two paths:

**1. Per-key triggers.** A mutation whose new state is checked against the relevant rule as
part of the mutation — a write placed on the wrong tier because its preferred disk is full, a
replica write that failed, a node copy that failed verification — inserts one `ReconcileWork`
entry in the same journal entry batch. Triggers are deterministic functions of the mutation,
so replay re-creates them.

**2. Scans.** A scan walks one structure with a resumable cursor and acts on, or queues, each
mismatch it finds:

| Event | Scan |
| --- | --- |
| Disk set to `Draining` or removed | the disk's backpointer range `(disk_id, *)` (METADATA-DESIGN§2.2) |
| Disk failed | the failed disk's backpointer range, re-replicating from surviving copies or peers |
| Placement-rule change (POOL-DESIGN§1) | the postings of the terms the old and new rule queries touch |
| Ontology change altering compression, encryption, tier or chunking for a tag | that tag's postings |
| Snapshot marked deleted | the snapshot-aware trees, one cursor each (SNAPSHOTS-DESIGN§1.5) |
| Rollback | the object table's history entries on the abandoned branch (SNAPSHOTS-DESIGN§1.8) |
| Rule change in the ontology | the postings to rework (INDEX-DESIGN§2.4) |
| Term reclamation | the term dictionary (INDEX-DESIGN§1.2) |
| Copy-GC reserve low | the freespace LRU's most fragmented bands (ALLOCATION-DESIGN§1.6) |

```rust
// ReconcileScan value — positional CBOR (FORMAT-DESIGN§1.6)
ScanRecord = [version, scan_kind: uint, tree: uint /* BtreeKind */, cursor_key: bytes,
              params: bytes /* scan-kind-specific: disk id, term ids, snapshot set */]
```

A scan's cursor is journalled (`ReconcileScanStep`) only at checkpoints, and only as far as the
changes that checkpoint made durable, so a resumed scan redoes at most the work since the last
checkpoint. Every scan is idempotent under cursor re-entry: a key already handled satisfies the
scan's filter as a no-op. A scan that finds more mismatches than it can act on in one step
queues them in `ReconcileWork`.

### 1.4 Priority ordering

`WorkKey.priority` orders the work tree:

| Priority | Work |
| --- | --- |
| 0 | node copies and tree nodes under-replicated, failing verification or on a draining disk |
| 1 | data extents under-replicated or on a draining disk |
| 2 | metadata option updates |
| 3 | data tiering, option updates, copy GC |
| 4 | optimisations |

Scans run between queue items, with their own share of each step's budget: evacuation and
failure scans at priority 0–1, cleanup and rework scans at 4. Within a priority, entries are in
physical order, so a worker sweeps each disk from low to high buckets. Snapshot cleanup reclaims
space but never blocks correctness; under sustained pressure it yields to copy GC and tiering
and resumes from its cursor.

### 1.5 Move path

Reconcile shares one **move path** with copy GC:

1. Read the extent or node copy from `from_loc` and validate (block checksum; BLAKE3 at the
   object level for `OwnerKind::BlobExtent`).
2. Write the bytes to a fresh `to_loc`.
3. Emit one `ReconcileMove { from_loc, to_loc, owner_key }` journal entry (JOURNAL-DESIGN§1.3) —
   the atomicity boundary: one LSN, one commit.
4. Remove the work entry, if the move came from one.

**Schema vs. replay.** `ReconcileMove` is what writers emit; replay projects it into the
equivalent primitive mutations:

```
ReconcileMove { from_loc, to_loc, owner_key }
  ⇒ BackpointerRemove(from_loc → key)
   ∘ BackpointerInsert(to_loc → key, value)
   ∘ owner-update on owner_key:
        OwnerKind::BlobExtent      → start_offset 0 and an Extent locator: the locator's replica in
                                     the radix record and every ObjectHistory version of the oid
                                     naming from_loc (SNAPSHOTS-DESIGN§1.6); otherwise every
                                     extent-tree key of the oid, at any snapshot, in the range
                                     the owner key bounds, that names from_loc (METADATA-DESIGN§1.3)
        OwnerKind::Chunk           → ChunkIndex entry's BlobRef (INDEX-DESIGN§4.3)
        OwnerKind::BtreeNode       → the moved copy's slot in the parent's NodePtr (BTREE-DESIGN§1);
                                     for a forward-overflow region after the first, the next
                                     region's predecessor pointer (a new copy of that region)
        OwnerKind::PackedBlock     → the Packed locator of every object the block names, in
                                     every version naming the block
```

Changing the number of copies projects the same way, onto the same owners:

```
ReconcileReplicate { existing, new_loc, owner_key }
  ⇒ BackpointerInsert(new_loc → key, value)
   ∘ owner-update: add new_loc as a replica beside `existing`, raising replica_count
     (BlobExtent: in the locator or the extent-tree keys found as for a move; PackedBlock: in
     the Packed locator of every object the block names)

ReconcileDropReplica { loc, owner_key }
  ⇒ BackpointerRemove(loc → key)
   ∘ owner-update: remove loc from the replicas, lowering replica_count; refused when it is
     the last replica
```

A tree node always has exactly `metadata_replicas` copies, so its repair is a `ReconcileMove`
of the lost copy's slot to a new location, never a change of count. A chunk has one location in
`ChunkIndex`, so neither op applies to `OwnerKind::Chunk`.

All projected mutations apply under the move's LSN, so a crash either replays all of them or
none. `BackpointerInsert` / `BackpointerRemove` remain first-class journal ops because they are
also emitted directly by non-move callers (object deletion, fresh writes, node frees) — moves
are the only flow that needs the bundled `ReconcileMove`.

**Type adaptation.** `BlockRef` is the uniform 16-byte handle used in journal entries
(`{disk_id, sector, bucket_no, generation}`, ENGINE-DESIGN§1.2); the on-disk owner fields use
narrower shapes sharing its first 8 bytes. Replay demotes `to_loc: BlockRef` into the target's shape:

- `ReplicaRef` (METADATA-DESIGN§2.1, `BlobExtent`): take the first 8 bytes; write the low 16 bits
  of `generation` into the slot's generation tag.
- `BlobRef` (INDEX-DESIGN§4.3, `Chunk`): take the first 8 bytes; copy `length` from the *previous*
  owner record's `BlobRef.length` — moves preserve extent length, so the field is unchanged.
- `BlockRef` (`BtreeNode`): copy verbatim into the moved copy's slot of the parent's `NodePtr`.

Writers do the inverse promotion when emitting `ReconcileMove`: they read the destination
bucket's resident generation to populate `BlockRef.generation`; any `length` carried by the
source field is not journalled, because no consumer needs one beyond what the owner already
holds. A move never changes a length; an operation that does is not a move.

**Throttling.** Pool-state tunables (POOL-DESIGN§1):

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
  priority-1 `ReplicaRepair` entry; copy GC continues.

### 1.7 Self-healing

If reconcile detects an inconsistency with **no obvious cause** (no recent option change, no
device event), it records an error in the pool state's error log (POOL-DESIGN§1): something
unexpected happened that needs operator attention. Degraded data is repaired regardless — the
lack of a known cause doesn't block the repair, only flags it for investigation.

Items in `ReconcilePending` are retried only on device-configuration events (disk added, state
changed, capacity expanded). Without such an event, the prerequisites haven't changed and a
retry would just re-fail.

### 1.8 In-memory state

| Structure | Purpose |
| --- | --- |
| `ReconcileState` | Queue head of `ReconcileWork`, active scans and their cursors, throttle counters. |
| `MoveInflight` | Bounded by the throttle; each entry carries the read buffer and target write point. |

Reconcile work happens only inside `step` calls (ENGINE-DESIGN "Selected design"): each call
takes up to its budget of queue entries and scan progress. The device runs it on the filesystem
server's own loop; a host helper may run steps on worker threads (one per HDD, two per SSD by
default), taking the engine's write lock only at trigger time and at a move's atomic update,
never during the data copy.

### 1.9 Operator interface (DESIGN-level)

`mimir reconcile status` reports per-priority queue depth, active scans with progress,
in-flight bytes and any pending-error items.

`mimir reconcile wait --type evacuate --disk N` blocks until the evacuation scan for that disk
and the work it queued complete.

Per-workload pause / resume via pool-state flags (e.g. `reconcile_on_ac_only` to pause on
battery power for laptops).

### 1.10 Footprint

Reconcile trees are transient and small: a scan is one record however much it covers, and
queued entries are 36 bytes each before packing. Typical bounds:

- Idle pool: < 1 MiB across all reconcile trees.
- Disk evacuation of 10 M extents: one scan record, plus the entries for extents that could not
  be moved at once.
- Cluster resilver after a disk failure: similar.

These are listed under "transient" in FORMAT-DESIGN§16, not in the steady-state footprint.

---
