# Rewrite Follow-up — Deferrals & Improvement Plan

This document is the post-Phase-7 backlog, derived from `TODO(rewrite-phase-N)`
markers in source plus phase-completion reports. It complements
`REWRITE_CONTRACT.md` (which governs *how* sub-agents work) by listing *what*
remains to be done and a recommended sequencing.

The post-Phase-7 codebase compiles, tests, and lints clean across all 15
library crates and 4 binaries (549 tests total). Every deferral below is
deliberate: the owning sub-agent flagged it with a `TODO` comment and an
explicit rationale. Nothing here is unknown unknowns — it's all in-source.

---

## 1. Inventory by theme

### Theme A — B+ tree & sorted-run machinery (the keystone)

The single largest deferral. IMPL §1.5 specifies 256 KiB B+ tree / radix nodes
with append-only sorted runs, lazy compaction, format-descriptor key packing,
and journal-reclaim integration. Phase 1 shipped the **structs** (`BtreeNodeHeader`,
`SortedRunHeader`, `SortedRunKeyFormat`, `FieldFormat`), but the **machinery** is
still entirely placeholder. Affected:

- `mimisbrunnr-storage`: no live `LoadedNode`, no merge-search, no full
  compaction, no `FormatPromote` flow.
- `mimisbrunnr-meta`: `ObjectTable` and `LocationTable` are
  `BTreeMap<u64, _>` placeholders, not radix trees over §5/§6.1 leaves.
  `BackpointerTable` is `BTreeMap<BackpointerKey, BackpointerValue>`.
- `mimisbrunnr-index`: every index (`TagIndex`, `KvIndex`, `RangeIndex`,
  `ForwardIndex`, `ChunkIndex`) is a `HashMap`/`BTreeMap` mirror; persistence
  is a single CBOR blob per index in the index zone.
- `mimisbrunnr-pool`: `PoolStateRoot.disks_overflow_root`,
  `placement_rules_root`, `cluster_peers_root` all `BlockRef::ZERO`.
  13+ disk overflow tree returns `PoolError::DiskOverflowUnsupported`.
- `mimisbrunnr-ontology`: `OntologyState` is a single CBOR blob — no
  `BtreeKind::Ontology` B+ tree.
- `mimisbrunnr-watch`: subscriptions persisted as a CBOR blob — no
  `BtreeKind::Subscriptions`.
- `mimisbrunnr-unix`: `PathContextManager` likewise CBOR-only.
- `mimisbrunnr-engine`: index state framed as `[MIXI | u32 len | CBOR]`
  blob in the index zone (`disk_engine::save_index_state`). All
  `RootPointer` btree-root fields stay `BlockRef::ZERO` at commit.
- `mimisbrunnr-storage`: `BucketAllocTable` is in-memory `BTreeMap`,
  `freespace_root` unused.

### Theme B — Encryption integration

The on-disk layout reserves the bits but no cipher is wired:

- Workspace deps missing: `aes`, `aes-gcm`, `xts-mode`, `hctr2`,
  `chacha20poly1305`. (Adding them is itself a contract change.)
- `mimisbrunnr-transform::Encryptor`: every non-`None` `EncryptionMode`
  returns `TransformError::EncryptionDisabled`. Sector padding for XTS/HCTR2
  is in place; the cipher call is the missing piece.
- `mimisbrunnr-wal`: `WAL_ENTRY_FLAG_ENCRYPTED` rejects on append; AAD
  layout (header[0..36]), nonce derivation (LSN || 0), and GCM tag framing
  (16 B between ciphertext and trailing CRC) are all specified, just not
  implemented.
- `mimisbrunnr-storage`: `BLOCK_FLAG_ENCRYPTED` is recognised; XTS-AES-256
  block decrypt path doesn't exist. Same for HCTR2 on blob extents.
- Key hierarchy: `MasterKEK → DiskKey` (DESIGN §9.4) entirely TODO.
  `TransformKey` is a 32-byte newtype placeholder; no key-id mapping in
  superblock-adjacent metadata.
- Per-pool SipHash secret for `value_hash` is hard-coded zero. Should
  derive from `DiskKey` once that exists.

### Theme C — WAL completeness

- Compression flag `WAL_ENTRY_FLAG_COMPRESSED` rejects on append. Need
  `zstd`-compress-then-encrypt pipeline.
- Single-disk WAL only; cross-disk mirroring (DESIGN §3 paragraph
  "Mirroring across hot disks") is TODO.
- Journal-reclaim driver thread: `DirtyNode` struct exists but the
  fill-level rules (idle/background/aggressive/stall thresholds, IMPL §3.4)
  aren't driven by anything. Engine never schedules flushes; index state
  just sits in memory until `commit()`.

### Theme D — Snapshots (DESIGN §11, IMPL §11)

- `DiskEngine::snapshot_create` returns `EngineError::NotImplemented`.
- Snapshot tree (`SnapshotNode`, ancestor_bitmap, skiplist) — not modelled.
- Snapshot-aware bkey position (IMPL §11.2) — every "snapshot-aware"
  btree currently keys by `(_, snapshot=0)` or omits the field.
- Sidecar history btrees (`ObjectHistory`, `LocationHistory`) — not
  populated; current view only.
- Visibility rules (IMPL §11.3), retention policy (IMPL §11.8), rollback
  (§11.9), bucket retention under key-level snapshots (§11.6) — all TODO.
- Snapshot-related WAL ops (`SnapshotCreate`, `SnapshotDelete`,
  `SnapshotUnlink`, `SnapshotDepthUpdate`) are defined and replay-skipped.

### Theme E — Reconcile (DESIGN §17, IMPL §17)

- `DiskEngine::reconcile_step` returns `Ok(0)`.
- Work-item btrees (`ReconcileWork`, `ReconcileHighPrio`, `ReconcileWorkPhys`,
  `ReconcileHighPrioPhys`, `ReconcilePending`, `ReconcileScan`) — `BtreeKind`
  variants exist but no producers/consumers.
- Triggers (§17.3): bucket-fill watermark for copygc, replica drift for
  resilver, tier residence vs. policy for autotier, snapshot deletion scan,
  EC encode threshold — all unimplemented.
- Move path (§17.5) and self-healing (§17.7) — TODO.
- Operator interface (§17.9) — `mimir reconcile status / pause / resume` not in
  the CLI.

### Theme F — Blob zone & content paths

- `DiskEngine.blobs: HashMap<u64, Vec<u8>>` is the entire blob store right
  now. No write to actual `BlobZone` extents on the primary disk.
- `WriteBlob` WAL op carries an `extent: BlockRef`; engine sets
  `BlockRef::ZERO`.
- `ObjectLocation.replicas` not populated.
- Content reads: no `DiskEngine::read_blob(oid)` API, so `mimir project
  export` is stubbed.
- Chunked-object flow (IMPL §3.3.1, §9.3) — `WalOp` variants exist
  (`ChunkInsertBatch`, `ChunkListAppend`, `ChunkListReplace`, `ChunkListShrink`,
  `ChunkObjectFinalize`); engine doesn't drive them.
- Selective chunking (DESIGN §9.6) — chunk-decision logic at write time
  not wired; transform pipeline doesn't consult ontology storage policy.

### Theme G — Cluster sync (DESIGN §10)

- `ClusterPeers` btree variant exists; no peer registry, no peer-state
  machine.
- Hybrid clock observe-and-bump (`HybridClock::observe`) is implemented but
  not exercised — no remote ops arrive.
- Sync ops (`SyncOp` per DESIGN §10.4) — not modelled; engine doesn't
  serialise mutations into a peer-bound stream.
- Bitmap delta sync (§10.5), sync modes (§10.6), conflict resolution
  (§10.7), bandwidth budget (§10.8) — TODO.
- Hydration model (§10.3 `ContentPresence`) — type exists in
  `mimisbrunnr-types`, no engine support.
- Remote storage backends (§10.9) — TODO.

### Theme H — Allocation completeness (IMPL §12)

- §12.4 Freespace LRU — `freespace_root` field exists; tree not built.
  Allocator has no fragmentation-band scan.
- §12.5 Per-disk write-points — engine treats every write as identical;
  no segregation by data type / stream tag.
- §12.6 Copy GC — `WorkKind::Copygc` defined; reconcile engine doesn't
  drive it; no fragmentation tracking.
- §12.7 TRIM / discard — `BUCKET_FLAG_NEEDS_DISCARD`, `BucketDataType::NeedDiscard`
  exist; no discard-issuing thread.
- Generation-checked pointers work (Phase 1b); end-to-end backpointer-driven
  copygc doesn't.

### Theme I — Query / SQL surface

- `CmpOp::Contains` returns `UnsupportedCmpOp` from `QueryExecutor`; no
  substring index. SQL planner emits the right shape but execution fails.
- `CmpOp::Prefix` does a linear forward-index scan. Wants a sorted
  range / trie index.
- `Ne` builds the universe via tag-index union; no per-attr-key bitmap.
- `Related` is linear over forward index. Wants a `(predicate, target) → oids`
  reverse index.
- SQL: `ORDER BY`, `GROUP BY` (beyond bare `COUNT(*)`), `HAVING`, `JOIN`,
  subqueries, multi-column projections, `SELECT DISTINCT`, internal-wildcard
  `LIKE` (`'mid%dle'`, `_` single-char).
- `INSERT / UPDATE / DELETE` SQL — read-only by design; if added, becomes a
  thin wrapper over `DiskEngine` mutations.

### Theme J — Subscription engine completeness

- Mutation hooks (`on_tag_added`, etc.) are conservative: "watched tag added
  → enter". Arbitrary boolean queries need re-evaluation against the live
  query executor; engine layer must call `set_membership`. Currently no
  caller does.
- Per-tick debounce flush (`SubscriptionEngine::tick`) is a no-op.
- WAL-replay offline catch-up (DESIGN §11.5) — `set_cursor` exists; the
  replay loop in engine is missing.
- Subscription persistence is CBOR; `BtreeKind::Subscriptions` (IMPL §10.2)
  unused.

### Theme K — Bitmap representation

`roaring::RoaringBitmap` is 32-bit. `ObjectId.local` is 48 bits. Every place
that builds a bitmap currently uses **the low 32 bits of `local`** with
`node = 0` assumed:

- `mimisbrunnr-query::QueryExecutor::evaluate_full` reconstructs `ObjectId`
  with hardcoded `node = 0`.
- `mimisbrunnr-fuse` faceted readdir uses the same assumption.
- `mimisbrunnr-watch` membership bitmaps share the limitation.

Options for fixing:
1. Switch to `croaring` (64-bit) — single dep change but loses Rust-native
   feel.
2. Per-node sharded `HashMap<NodeId, RoaringBitmap>` — preserves the 32-bit
   bitmap, splits by node prefix. Most natural fit for the design.
3. Custom 48-bit roaring wrapper over `roaring::RoaringTreemap`. Treemap is
   `BTreeMap<u32, RoaringBitmap>` keyed on the high 32 bits — would work for
   16+32 split.

### Theme L — FUSE writability

- All writes return `EROFS`. `write`, `create`, `mkdir`, `unlink`, `rmdir`,
  `rename` all stubbed.
- Real `mtime` / `mode` / `uid` / `gid` — currently 0/0/0; need libc or a
  configurable defaults file.
- Symlinks under `/ctx/` — `VfsEntryKind::CtxObject` doesn't model symlinks
  even though `unix::ImportKind::Symlink` exists.
- Subtree-only mount (`brunnr mount-unix --context ctx`) currently mounts
  the full `/tags + /ctx` tree.
- Real FUSE mount integration tests — none (would require root or
  `user_allow_other`).

### Theme M — Cross-crate API polish

Small but high-leverage. Each one removes a documented workaround:

| API gap | Workaround currently in use | Fix |
|---|---|---|
| `ForwardIndex::iter()` not public | CBOR round-trip in query, engine | Add `pub fn iter()` |
| `FileBlockDevice::open_read_only` missing | analyze opens RW | Add a flag |
| `StorageTier::name() / as_str()` missing | brunnr, analyze, pool reinvent the match | Inherent impl |
| `Engine::replay_create_object` is `pub(crate)` | mimir added a `create` subcommand to mint oids | Make public, or add `Engine::ensure_oid` |
| `DiskEngine::read_blob(oid)` missing | mimir project export stubbed | Add `pub fn read_blob(&self, oid) -> Option<Vec<u8>>` |
| `mimisbrunnr-fuse::ContentProvider` not re-exported | brunnr writes the boxed-fn type inline | Re-export the trait |
| `MimisbrunnrFs` constructor for subtree-only | brunnr documents the workaround | Add `with_root(root: VfsEntryKind)` |
| `WatchEvent` has no stable JSON form | mimir hand-formats | Implement `Display` or a `to_json()` |
| `ChunkIndex` doesn't derive serde | engine uses crate-local helpers | Derive serde on the inner map |
| `ObjectLocation::serialize` (return-Vec) | engine uses `serialize_into` | Add convenience helper |
| `PoolManager::add_disk` doesn't propagate to `PoolConfig` | engine re-reads + saves | Make `PoolManager` own the save |
| `arbitrary-int` listed as dev-dep in some crates that don't need it | dead | Remove |

### Theme N — Format & migration polish

- `fs_format_version` advance on upgrade — wired but no migration scan
  driver. `mimir fsck --upgrade` would block until the reconcile-driven
  rewrite finishes (IMPL §15.4).
- `downgrade_log_ref` chain (IMPL §15.5) — field exists; no chain populated.
- Per-structure migration rules (additive / layout / semantic) — unused.

### Theme O — Smaller, isolated TODOs

- `mimir watch stream` — continuous output (needs threading or an async
  runtime; out of contract §2's "sync only" rule, so design decision required).
- `mimir ontology adopt <tag> --into <module>` — needs new
  `OntologyState::adopt_orphan` API.
- LZ4 compression — needs `lz4_flex` workspace dep.
- `populate --blob-distribution gaussian` — needs `rand_distr`.
- `analyze` non-primary disk superblock view — needs public reader.

---

## 2. Sequenced improvement plan

Ordering rules of thumb:
- **B+ tree** before everything that needs persistent state to actually be
  persistent. Without it, snapshots, reconcile, and proper engine commit are
  blocked.
- **Cross-crate API polish** is fast and removes friction; do it first as a
  background sweep.
- **Encryption** is parallelisable with everything else (it touches disjoint
  code paths) — schedule it whenever there's spare capacity.
- **Cluster sync** waits until snapshots exist (it consumes snapshot diffs).

### Phase R0 — API polish sweep (pre-work, ~1 small agent)

Theme M, plus the small TODOs in Theme O that don't block anything.

**Why first:** removes documented workarounds from every later phase, making
agents' lives easier and reducing subtle bugs at the seams.

**Scope:** add public APIs, re-export trait types, remove dead deps, derive
serde where missing, add `name()` / `read_only` accessors.

**Effort:** S. One agent, one to two hours of agent time.

**Verification gate:** `cargo build --workspace`, no behaviour changes,
existing tests still pass; new APIs covered by unit tests in their owning
crates.

### Phase R1 — B+ tree & sorted-run machinery (Theme A)

The keystone. Implement IMPL §1.5 fully:

- §1.5.1 Region envelope read/write paths (already have structs).
- §1.5.2 Append-only growth: write a new `SortedRunHeader` + payload at
  `payload_used`; rewrite the `BtreeNodeHeader` sector only.
- §1.5.3 In-memory `LoadedNode` with merged-view caching.
- §1.5.4 Full compaction trigger and execution.
- §1.5.6 Packed-key codec (`SortedRunKeyFormat` driven encode / decode /
  compare).
- Generic over key/value types so every consumer (forward, range, tag-dir,
  bucket-alloc, freespace, ontology, subscriptions, snapshots, reconcile,
  chunk index) can plug in.

**Suggested split** (could be one big agent or two):
- **R1a** (storage): `LoadedNode`, sorted-run merge-search, compaction
  scheduler, packed-key codec, journal-reclaim driver thread (Theme C
  partially).
- **R1b** (downstream): swap every Phase 3+ index `HashMap`/`BTreeMap`
  placeholder for the new B+ tree. One commit per index.

**Why second:** every "persistence" deferral below this line traces back here.

**Effort:** XL — likely the largest single code drop in the project. The
packed-key codec alone is its own 500-line module. Plan for at least two
agent runs and several rounds of cross-crate reconciliation.

**Verification gate:** existing CBOR-blob persistence path remains as a
fallback under a feature flag for one revision so the change is bisectable.
Round-trip tests at the storage layer plus per-index round-trip tests
(insert N keys, flush to a fresh region, reload, query).

### Phase R2 — Bitmap representation (Theme K)

Fix the 32-bit truncation across the workspace. Pick option 2 (per-node
sharded) or option 3 (Treemap-based) and apply uniformly.

**Why third:** trips up `evaluate_full`, FUSE, watch — the whole stack
silently misbehaves above 33M objects per node. Doing this before snapshots /
reconcile means those don't need to be re-fitted later.

**Effort:** M. One agent. Touches every crate that builds bitmaps but each
change is mechanical.

### Phase R3 — Engine-driven journal-reclaim & checkpoint cadence (rest of Theme C, parts of Theme A)

With B+ tree in place, wire the journal-reclaim driver: WAL fill thresholds
trigger flushes (IMPL §3.4 table); accumulated dirty bytes / 30 s elapsed
trigger checkpoints (IMPL §3.5). Populate `RootPointer.*_root` fields properly.

**Effort:** M. One agent.

### Phase R4 — Real blob zone & content paths (Theme F)

Replace `DiskEngine.blobs: HashMap` with actual blob-zone bucket allocation
+ writes. Implement `read_blob`, populate `ObjectLocation.replicas`, drive
the chunked-object flow.

**Suggested sub-phases:**
- **R4a:** non-chunked write path: allocate blob bucket, append, set
  `ObjectLocation`, emit `WriteBlob` WAL op, populate backpointer.
- **R4b:** read path: `DiskEngine::read_blob(oid)` walks `ObjectLocation`,
  reads bucket, runs transform.invert.
- **R4c:** chunked write/read flow per IMPL §3.3.1.

**Effort:** L (split across 2–3 agents).

### Phase R5 — Encryption integration (Theme B)

Independent of R1–R4 in code, but the user-visible behaviour benefits from
having real persistence first. Add cipher crates to workspace, implement the
four cipher modes, wire AAD/nonce per IMPL §14, populate `encryption_keyid` in
superblock, build the `MasterKEK → DiskKey` hierarchy.

**Effort:** L. One agent (can run in parallel with R3 or R4).

### Phase R6 — Snapshots (Theme D)

Now that B+ trees exist and key-level snapshot ids are real positions, build:
- Snapshot tree + ancestor bitmap (IMPL §11.1).
- Sidecar history btrees (`ObjectHistory`, `LocationHistory`).
- Snapshot-aware bkey position threading through every snapshot-aware btree
  (forward, tag-dir, range, kv, ontology, subscriptions).
- Visibility / retention / rollback flows.
- Snapshot-related WAL ops actively driven.

**Effort:** XL.

### Phase R7 — Reconcile (Theme E)

Work-item btrees become live; producers (allocator threshold, replica drift,
tier residence drift, snapshot delete) emit `WorkItem`s; consumers
(`reconcile_step`) drain. Implement copygc, autotier, replica repair, EC
encode (defer to a future), snapshot cleanup.

Depends on R1 (work-item btrees), R3 (checkpoint cadence), R4 (move-path
needs blob reads/writes).

**Effort:** L–XL.

### Phase R8 — Allocation completeness (Theme H)

Most of this comes for free with R7 (copygc) but fragmentation tracking,
freespace LRU bands, and write-point segregation are their own work.

**Effort:** M.

### Phase R9 — Query/SQL surface completion (Theme I)

Add per-attr-key bitmap (for `Ne`), substring index (for `Contains`), reverse
relation index (for `Related`), `ORDER BY` via `OrderedCollection` plumbing.

**Effort:** M. Mostly orthogonal to lower phases.

### Phase R10 — Subscription engine completeness (Theme J)

Wire `set_membership` calls in engine mutation hooks; debounce flushes on
tick; WAL-replay offline catch-up.

**Effort:** S–M. Can be done before R6.

### Phase R11 — FUSE writability (Theme L)

Implement `write`, `create`, `unlink`, `rename`, `mkdir`, `rmdir` in
`MimisbrunnrFs`. Add real `mtime/mode/uid/gid` (consider adding `nix` or
`libc` to workspace).

**Effort:** M.

### Phase R12 — Cluster sync (Theme G)

Cluster peer registry, hybrid-clock-driven sync ops, bitmap delta sync.
Depends on R6 (snapshot diffs are the unit of sync).

**Effort:** XL.

### Phase R13 — Format & migration polish (Theme N)

Migration scan driver, downgrade log chain, `mimir fsck --upgrade`.

**Effort:** M.

---

## 3. Recommended next concrete step

Start with **R0** (API polish) — it's an easy win that makes every later
phase smoother. A single agent can do R0 in one session.

R1 (B+ tree) is the most consequential but also the most intricate; budget
multiple sessions and plan to land R1a (storage-layer machinery) and R1b
(per-index swap) as separate landings with verification between.

Phases R5 (encryption), R9 (query completeness), R10 (subscription
completeness), and R11 (FUSE writability) can run **in parallel** once R0 is
done — they touch disjoint code paths.

Phases R6 (snapshots), R7 (reconcile), and R12 (cluster sync) form a strict
chain after R1: snapshots before reconcile (snapshot cleanup is a
reconcile job); reconcile before cluster sync (sync uses reconcile to
hydrate diffs).

A reasonable two- to three-month roadmap for one agent-driven worker:

```
Week 1:        R0
Weeks 2–4:     R1a + R1b
Week 5:        R2 + R3 (in parallel)
Weeks 6–7:     R4
Weeks 8–9:     R5  ┃  parallel: R9, R10, R11
Weeks 10–13:   R6
Weeks 14–15:   R7 + R8
Weeks 16+:     R12, R13
```

Adjust by priority — production deployments without encryption (R5) are
viable for trusted single-node use, so R5 can slip without blocking; without
snapshots (R6), retention and rollback features are missing, which may be
acceptable for short-term use.

---

## 4. Health metrics to track

As phases land, surface these in `analyze`'s Pool Overview tab:

| Metric | Phase that wires it |
|---|---|
| % of `RootPointer.*_root` fields populated | R1b |
| Live B+ tree depth per index | R1b |
| Journal pin pressure (WAL fill %) | R3 |
| Average dirty-node lag (LSN gap) | R3 |
| Blob zone live bytes vs bucket-alloc bytes | R4 |
| Bucket fragmentation distribution | R8 |
| Reconcile queue depth, by `WorkKind` | R7 |
| Subscription drain backlog | R10 |
| Snapshot tree depth, live count | R6 |
| Encryption coverage (% of zones encrypted) | R5 |

These give an at-a-glance "how complete is the implementation" view that
supplements the test count.
