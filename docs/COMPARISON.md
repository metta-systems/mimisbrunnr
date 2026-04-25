# Mímisbrunnr vs Existing Filesystems

A comparison of Mímisbrunnr against APFS, NTFS, ReFS, bcachefs, btrfs, XFS, and ext4 across four
operational dimensions: **crash recovery**, **data resilience**, **many-small-file workloads**,
and **few-large-file / streaming workloads**.

The Mímisbrunnr column is an **engineering estimate** based on the design and implementation
specifications ([DESIGN.md](DESIGN.md), [IMPLEMENTATION.md](IMPLEMENTATION.md)) — not measured
benchmarks. Where the estimate is uncertain, the text says so.

---

## Summary Table

| Dimension                  | ext4         | XFS          | btrfs         | bcachefs      | APFS          | NTFS          | ReFS          | **Mímisbrunnr** |
| -------------------------- | ------------ | ------------ | ------------- | ------------- | ------------- | ------------- | ------------- | --------------- |
| Crash model                | journal      | metadata journal | COW       | COW           | COW           | journal       | COW           | WAL + COW       |
| Atomic root commit         | no           | no           | yes           | yes           | yes           | no            | yes           | yes (A/B)       |
| Data checksums             | no           | no (metadata only) | yes (CRC32C) | yes (configurable) | no    | no            | yes (opt.)    | yes (BLAKE3 + CRC32C) |
| Self-heal from replica     | no           | no           | yes           | yes           | no            | no            | yes           | yes (per-object) |
| Snapshots                  | LVM only     | LVM only     | yes           | yes           | yes           | VSS (external) | yes (Storage Spaces) | yes (COW root chain) |
| Online fsck                | limited      | limited      | scrub         | yes           | mostly auto   | no            | yes           | yes (scrub)     |
| Small-file metadata cost   | low          | medium       | high          | medium        | low           | low (MFT)     | high (64K block) | medium-high  |
| Large-file streaming       | very good    | **best in class** | poor (COW frag) | good     | good          | good          | very good     | good (with care) |
| Tiering / multi-tier       | no           | no (RT subvol) | no          | yes (native)  | no            | no            | yes (Storage Spaces) | yes (tag-driven) |
| Cluster-friendly           | no           | no           | no            | no            | no            | no            | partial (S2D) | yes (native)    |
| Maturity                   | mature       | mature       | maturing      | new           | mature        | mature        | mature        | **design**      |

---

## 1. Crash Recovery

### Existing systems

- **ext4** — Metadata journal (`data=ordered` is the default). After a crash, journal replay
  rebuilds metadata consistency; recently-written file *contents* may be lost or partially
  written. Recovery time is proportional to journal size (typically <1 second). Full `e2fsck`
  scan is offline and can take hours on petabyte-class volumes.
- **XFS** — Pure metadata journal with delayed logging. Recovery is sub-second even on large
  volumes. `xfs_repair` is offline-only but extremely fast. No data-loss protection beyond what
  the journal guarantees.
- **btrfs** — Copy-on-write with atomic transaction commits. A crash leaves the filesystem at the
  last committed transaction (typically 30 s old). `btrfs check --repair` has a long history of
  making things worse and is not recommended; recovery typically means rolling back to a snapshot
  or restoring from backup.
- **bcachefs** — COW with per-extent checksums; designed for online repair (`bcachefs fsck` works
  on a mounted filesystem). Crash leaves the FS at the last committed journal entry.
- **APFS** — Atomic transactions via COW. Recovery is automatic at mount — the container's
  superblock pair (checkpoint mechanism) selects the latest valid checkpoint. `fsck_apfs` exists
  but is rarely needed.
- **NTFS** — `$LogFile` (transactional log) + `$UsnJrnl` (change journal). Fast recovery for
  metadata. `chkdsk` is needed for corruption beyond what the log can fix; it is offline and
  slow on large volumes.
- **ReFS** — COW with **automatic online repair** from a mirror copy if one exists. ReFS does not
  have a `chkdsk` equivalent — corruption detected during read is repaired in place.

### Mímisbrunnr (estimated)

- **WAL + A/B alternating root pointer.** Mutation flow: WAL append → fsync → COW pages → atomic
  superblock root flip → fsync. A torn write at any stage leaves either the previous or the new
  root durable; a half-written WAL entry fails its trailing CRC and is discarded.
- **Three superblock copies** (start, +4 KiB, end of device) defeat single-block superblock
  corruption. Reader picks the highest-`(seq, lsn)` valid copy.
- **Recovery time** ≈ replay of WAL tail since the last checkpoint. With a 64 MiB WAL and
  checkpoints every ~30 s, expect <1 s recovery on NVMe — comparable to XFS / APFS / bcachefs.
- **Snapshot chain** retained for the deletion grace window (default 7 days) gives an explicit
  rollback path superior to ext4/XFS/NTFS. A node that detects post-mount corruption can revert
  to the most recent verified snapshot; cluster peers then re-sync ops generated after that
  snapshot's LSN.
- **Online repair**: scrub walks the COW tree, validates per-block CRC32C and per-object BLAKE3,
  and (where replicas exist) heals from a peer or another disk. Closer to ReFS / bcachefs than
  to ext4 / XFS in operational model.

**Caveat:** the cluster-sync layer is itself a complexity multiplier. A Byzantine peer pushing
malformed ops is a failure mode existing local FSes don't have. The design mitigates this via
HLC ordering + signed sync bundles, but until the implementation is hardened, recovery is only
*as good as the design intent* — currently unproven.

---

## 2. Resilience and Data Integrity

### Existing systems

- **ext4 / XFS** — Metadata CRCs only (XFS v5, ext4 with `metadata_csum`). No data-block
  checksums; bitrot in file content is invisible to the filesystem. Resilience comes from the
  layer below (RAID + scrub, dm-integrity). XFS pairs well with `dm-integrity` for full-stack
  protection but the integration is manual.
- **btrfs** — CRC32C on every data block by default; xxHash, SHA-256, BLAKE2b configurable.
  RAID-1/10 self-heals on read (returns the good copy, rewrites the bad one). RAID-5/6 is still
  flagged as unstable for some failure modes. Scrub is mature.
- **bcachefs** — CRC32C / xxHash / BLAKE2 per extent; replication and erasure coding are first-
  class but the EC implementation is young. Self-heal is built into the read path.
- **APFS** — **Metadata checksums only** (Fletcher-64). Data is not checksummed by the filesystem
  — Apple's stated reasoning is that NAND ECC + flash-translation-layer remapping handle bit
  errors. This is a deliberate but contested choice; it means silent data corruption from
  controller bugs, cosmic rays after writeback, or HDD failures is invisible to APFS.
- **NTFS** — No data checksums. Uses `$Bitmap` and `$LogFile` for metadata consistency. Bit-rot
  protection requires Storage Spaces below.
- **ReFS** — Optional **integrity streams** per file (Murmur3-based hash). When enabled and
  paired with mirrored Storage Spaces, ReFS auto-heals on read. Default is off for general data
  to avoid COW overhead; on by default for metadata.

### Mímisbrunnr (estimated)

- **Two-layer integrity**:
  - **Storage layer**: CRC32C in every 4 KiB `BlockHeader` — catches torn writes, controller
    corruption, bit-flips at rest. Hardware-accelerated on x86 (SSE 4.2) and ARMv8.
  - **Content layer**: BLAKE3 per object, computed on plaintext before any compression /
    encryption, stored in `ObjectRecord.content_hash`. Verifiable end-to-end independent of
    storage transformations. BLAKE3 is faster than CRC32 on modern CPUs (parallel tree mode).
- **Per-object replication policy** via tags is the differentiating capability: a placement rule
  like `Replicate { query: HasTag("critical"), min_replicas: 3 }` gives 3× redundancy only to
  files that need it, while routine files run at 1×. ZFS, btrfs, ReFS, bcachefs all configure
  redundancy at the *pool / volume / dataset* level.
- **Self-heal**: when a block's CRC fails, the location table's `replicas` field lists alternate
  copies; the engine reads from a replica, verifies BLAKE3 against the object record, and
  rewrites the bad block. Equivalent operational outcome to btrfs/bcachefs/ReFS, with finer
  policy granularity.
- **Cluster amplifies resilience**: every node holds the full metadata. A disk failure on Node A
  means blob loss only for objects whose replicas were all on A; the metadata (and therefore the
  knowledge of *what* was lost) is intact on every peer. ZFS / btrfs / bcachefs offer no
  equivalent property.

**Caveat**: BLAKE3 verification on every read is more expensive than ReFS's optional Murmur3 or
btrfs's CRC32C. The design accepts this for the integrity guarantee. On AES-NI-equipped hardware
the overhead is in the noise (~50 µs per MiB) but on small embedded targets it may matter.

---

## 3. Many Small Files

This workload (mail spools, source trees, image thumbnails, container layers) stresses metadata
allocation, directory lookups, and per-file overhead.

### Existing systems

- **ext4** — Strong baseline. H-tree indexed directories give O(log N) lookup. Inodes are
  pre-allocated at mkfs time; small files <60 bytes are inlined into the inode directly. Caveat:
  inode count is fixed — exhausting it on a near-empty volume is a classic operational footgun.
- **XFS** — Historically weaker than ext4 on small-file metadata workloads (heavy `creat`/
  `unlink` patterns), substantially improved with delayed logging in recent kernels but still
  marginally behind ext4 on synthetic mail-server benchmarks.
- **btrfs** — Slower than ext4/XFS on small-file workloads. COW means each metadata mutation
  rewrites the b-tree path. Inline data extents help for files under ~2 KiB. Compression helps
  on-disk size, costs CPU.
- **bcachefs** — Competitive with ext4 / btrfs depending on workload. The b-tree-of-b-trees
  design optimises for concurrent writers but COW write-amplification is similar to btrfs.
- **APFS** — Very competitive. The B-tree directory structure plus instant clones (`cp -c`)
  make small-file copy patterns extremely fast. Atomic safe-save via `clonefile()` is a major
  win for many editors / build tools.
- **NTFS** — Small files (<~1 KiB) are stored *resident* in the MFT record itself, eliminating
  one I/O per file. Strong baseline for small-file metadata; weaker than ext4 once files grow
  past the MFT inline threshold.
- **ReFS** — **Notably weaker** for many-small-file workloads. The 64 KiB minimum allocation
  unit and large B+ tree page size are tuned for VHDX / large-file scenarios. ReFS is not
  recommended for general small-file workloads (e.g. dev environments, mail servers).

### Mímisbrunnr (estimated)

This is the most uncertain dimension. Honest assessment:

**Per-object cost** for create/tag/delete is *higher* than ext4 or XFS:

- A single `creat()` equivalent generates: WAL append (1 op), object record write (COW page),
  forward index update (B+ tree leaf), tag inverted index updates (one per tag), location table
  update. Estimated 4–8 logical writes vs. ext4's 2–3 (inode + directory + journal).
- Materialised implications add bitmap updates. With the typical "10 → 30 tags" expansion from
  ontology, that's 30 bitmap-OR operations per object create — sub-microsecond each, but they
  add up.
- Fixed `ObjectRecord` is 128 B vs. ext4's 256 B inode (smaller). No directory entries because
  there are no directories. The forward index entry adds ~60 B per object. Total metadata per
  object is roughly **comparable to ext4** in size.

**Per-object cost** for read / discover is *lower*:

- No path traversal. "All files matching X" is a bitmap intersection — sub-millisecond at any
  scale. ext4/XFS scanning a million-file directory takes seconds; Mímisbrunnr does not have
  directories at all.
- O(1) record lookup by ID via the radix table. APFS / btrfs / bcachefs all do O(log N) tree
  lookup.

**Realistic comparison**:

| Operation                                   | ext4    | XFS     | btrfs   | APFS    | **Mímisbrunnr (est.)** |
| ------------------------------------------- | ------- | ------- | ------- | ------- | ---------------------- |
| `creat()` 1 file                            | fast    | fast    | medium  | fast    | **medium** (more index updates) |
| `creat()` × 100 K                           | fast    | medium  | slow    | fast    | **medium-slow** (tag bitmaps)   |
| List 1 M files matching predicate           | minutes | minutes | minutes | minutes | **<1 ms**              |
| `stat()` by ID/path                         | µs      | µs      | µs      | µs      | µs                     |
| Directory lookup by name                    | O(log)  | O(log)  | O(log)  | O(log)  | **N/A** (path projection table) |

**Verdict**: Mímisbrunnr will likely be **slower than ext4/APFS on raw small-file ingest** and
**dramatically faster on small-file discovery / categorisation**. For workloads where the
question is "create 100 K files, then never look at them," ext4 wins. For "create 100 K files,
then constantly query them by attribute," Mímisbrunnr is in a different performance class.

---

## 4. Few Large Files (Streaming Recording, VM Images, Datasets)

This workload (4K video recording, multi-TB VM images, scientific datasets) stresses extent
allocation, sequential bandwidth, and rewrite patterns.

### Existing systems

- **XFS** — **The gold standard** for large-file streaming. Allocation groups give parallel
  allocation; preallocation via `fallocate` gives contiguous extents; the optional real-time
  subvolume bypasses the journal for guaranteed-rate recording. Used in IRIX broadcast systems
  for decades, still the choice for high-bitrate capture.
- **ext4** — Extent-based, good for large files. `fallocate` reserves contiguous space. Can hit
  fragmentation under concurrent multi-stream write but mostly excellent.
- **btrfs** — **The weakest** at this workload because of COW. Every overwrite of a large file
  produces a new extent, fragmenting the file over time. `chattr +C` (nodatacow) disables COW
  per-file but loses checksums and snapshots. Defragmentation breaks reflinks. Heavy VM-image
  hosting on btrfs typically requires `nodatacow` or external mitigation.
- **bcachefs** — COW like btrfs, but explicit tiering (NVMe write cache → HDD bulk) handles
  streaming workloads better. Sequential write performance is competitive with XFS when sized
  appropriately.
- **APFS** — COW with sparse-file support. Adequate for video editing on Apple platforms.
  Pro-video tools (DaVinci, Final Cut) work fine on APFS but high-bitrate raw capture often
  uses ProRes RAW on dedicated capture cards bypassing the FS.
- **NTFS** — Extent-based, good for large files. Sparse files supported. Defragmentation needed
  over time for sustained random writes inside large files.
- **ReFS** — **Designed for this**. 64 KiB allocation unit, block-clone for VHDX
  (instant copies), integrity streams off by default for the bulk data, sparse VDL (valid data
  length) gives instant large-file allocation. Hyper-V VM hosts on ReFS are the canonical use
  case.

### Mímisbrunnr (estimated)

The design is **self-aware about this workload** and treats it differently from small files:

- **Ontology-driven compression skip**: tags like `video`, `audio`, `mp4`, `h264` map to "skip
  compression" — no Zstd/LZ4 attempted on already-compressed media. Removes a major COW-friend
  performance trap.
- **Selective chunking**: FastCDC content-defined chunking is **off by default** and only
  enabled for tags like `vm-image`, `database-file`, `mutable-large` where dedup-on-edit pays
  for the chunk-index overhead. A 4K video recording is *not* chunked — it's stored as a single
  contiguous extent in the blob zone.
- **Blob-zone allocator** prefers 64+ block (256 KiB+) extents to keep large-file fragmentation
  manageable. A 1-hour 4K recording at 100 Mbps = 45 GB in a few large extents on an empty zone,
  comparable to XFS allocation behavior.
- **HCTR2 encryption is length-preserving** so streaming throughput is bounded by AES throughput
  (~3 GB/s on AES-NI), not by the cipher mode's expansion overhead.
- **Tiering**: a placement rule `Pin { query: HasTag("recording"), tier: Hot }` keeps active
  recordings on NVMe; archived recordings auto-migrate to HDD. Equivalent capability to ReFS
  Storage Spaces or bcachefs's tiering, but driven by *tags* rather than path.

**Estimated standing**:

- For **append-only streaming write** (capture, log files, write-once datasets):
  comparable to XFS, slightly behind because of WAL traffic per blob extent and per-block
  CRC32C cost. Within ~10–20% of XFS sequential bandwidth on NVMe is plausible.
- For **random rewrites within a large file** (VM images, database files): without CDC enabled,
  this is a COW filesystem and will fragment like btrfs. With CDC enabled (via tag), small
  edits dedup against existing chunks and sync-traffic costs collapse — *better* than any
  existing FS. The user/ontology controls the trade-off.
- For **sequential read of large file**: gated by extent contiguity. Empty pool: comparable to
  XFS. Aged pool with many rewrites: depends on whether CDC is on. Worst case (heavy COW, no
  CDC) similar to btrfs.

**Caveat**: real-time guaranteed-rate I/O (live broadcast capture) is not part of the design.
XFS's real-time subvolume has no equivalent here. For deterministic-latency video capture, XFS
on dedicated storage remains the right answer.

---

## 5. Where Mímisbrunnr Stands

**Where it should win**:

- Any workload dominated by *finding* files rather than *creating* them. Source trees, photo
  libraries, music collections, document archives, build artifact stores.
- Multi-host scenarios where every node needs to know what exists. Cluster filesystems, edge
  caching, hybrid local/cloud setups.
- Per-file policy granularity: redundancy, encryption, tier — driven by content semantics
  rather than mount points.
- Schema-evolving data: the ontology module system gives versioned, ref-counted schema
  evolution that no traditional FS attempts.

**Where it will tie or lose**:

- Pure throughput on append-only large-file workloads: XFS and ReFS are mature and tuned for
  this; Mímisbrunnr is competitive but not leading.
- Small-file *write* throughput: ext4 and APFS have less per-object machinery and will be
  faster for raw ingest.
- Single-host workloads with no cluster requirement: the cluster sync layer is overhead that
  ZFS / btrfs / bcachefs avoid.
- Mature operational tooling: every other FS in this comparison has decades or years of
  production fsck/recovery/migration tooling. Mímisbrunnr has design intent.

**Where the comparison is unfair (in either direction)**:

- All other systems are **shipped, in production, with public benchmarks**. Mímisbrunnr is a
  design with a partial implementation. Numbers in this document are *projections* from the
  spec, not measurements. They will be wrong, possibly in either direction.
- Mímisbrunnr's "workload" is not the same as the others. It's not trying to be a faster ext4 —
  it's trying to make path-based access *unnecessary*. Comparisons that frame it as "ext4 with
  tags bolted on" miss the design intent; comparisons that frame ext4 as "Mímisbrunnr without
  the good parts" are equally unfair.

---

## 6. Honest Recommendations

| If you need…                                       | Use today              | Mímisbrunnr fit       |
| -------------------------------------------------- | ---------------------- | --------------------- |
| Boot drive, single host                            | ext4 / APFS / NTFS     | overkill              |
| Maximum streaming bandwidth, single host           | XFS                    | competitive, not leading |
| VM host with snapshots and self-heal               | ReFS / bcachefs / btrfs | competitive (CDC mode) |
| Multi-PB archive with tiering                      | bcachefs / ZFS + custom | well-suited           |
| Photo / music / source / document library          | any + Spotlight/Tracker | **target use case**   |
| Distributed metadata across edge devices           | (no good answer)       | **target use case**   |
| Schema-evolving structured data on a filesystem    | (no good answer)       | **target use case**   |
| Real-time guaranteed-rate capture                  | XFS RT subvolume       | not designed for this |

---

## 7. References

- ext4: <https://ext4.wiki.kernel.org/>
- XFS: <https://xfs.org/index.php/XFS_FAQ>; *Scalability in the XFS File System* (Sweeney, USENIX 1996)
- btrfs: <https://btrfs.readthedocs.io/>; status page (RAID-5/6 caveats)
- bcachefs: <https://bcachefs.org/>
- APFS: *Apple File System Reference* (Apple, 2020); Howard Oakley's blog series on APFS internals
- NTFS / ReFS: *Windows Internals* (Russinovich et al., 7th ed.); MS-FSCC protocol docs
- ReFS integrity streams: <https://learn.microsoft.com/en-us/windows-server/storage/refs/integrity-streams>
- BLAKE3: <https://github.com/BLAKE3-team/BLAKE3-specs>
- Roaring bitmaps: <https://roaringbitmap.org/>
