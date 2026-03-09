# Mímisbrunnr — The Well of Knowledge

**An associative filesystem for the Mettā operating system**

_Mímisbrunnr is the well at the root of Yggdrasil that holds all knowledge and wisdom. Odin sacrificed an eye for a single drink from it. In our system, each node sacrifices a few hundred megabytes to replicate all metadata — and gains the ability to see everything._

**Mímir** — the guardian of the well — is the query engine. You ask Mímir a question (a tag query), Mímir looks into the well (bitmap indexes), and returns answers.

---

## 1. Design Philosophy

Mímisbrunnr abandons hierarchical directories entirely. There are no folders, no paths, no tree structure. Every object (file) exists in a flat space and is discoverable through **associative queries** over its metadata — tags, attributes, relations, and ontological inference.

The design separates three concerns completely:

- **Metadata** (tags, attributes, ontology) — small, replicated everywhere, always searchable
- **Content** (blob data) — large, lives where placed, fetched on demand
- **Policy** (placement, tiering, encryption, compression) — driven by the ontology, not by location

This separation means any node in a cluster can search the entire filesystem instantly (metadata is local), while blob content only moves over the network when actually opened.

### 1.1 Naming

The system follows a Norse mythological naming convention consistent with the broader Mettā ecosystem:

| Name            | Component                   | Mythological basis                |
| --------------- | --------------------------- | --------------------------------- |
| **Mímisbrunnr** | The filesystem itself       | The well containing all knowledge |
| **Mímir**       | Query engine / daemon / CLI | The wise guardian you consult     |

```
brunnr create /dev/nvme0n1 /dev/sda      # forge the well across disks
brunnr mount                             # open the well

mimir query "electronic AND year:2024"   # consult the wise one
mimir tag obj:42 ambient chill           # add knowledge to the well
```

---

## 2. Data Model

### 2.1 Objects

Every file is an **object** with a globally unique ID and a bag of assertions about it. Objects have no intrinsic name — "name" is just another attribute.

```rust
// Node-prefixed: top 16 bits = node ID, bottom 48 = local seq
type ObjectId = u64;
type TagId = u32;
```

The node-prefix scheme allows independent creation across nodes with no coordination. Each node can create 281 trillion objects (48-bit local sequence) — at 1000 objects per second, that's ~8,900 years before exhaustion:

```rust
impl ObjectId {
    fn new(node: u16, local_seq: u48) -> Self {
        Self((node as u64) << 48 | local_seq as u64)
    }
    fn node(&self) -> u16 { (self.0 >> 48) as u16 }
    fn local(&self) -> u64 { self.0 & 0x0000_FFFF_FFFF_FFFF }
}
```

Object IDs are **never recycled**. The 48-bit space is sufficient, and non-recycling eliminates an entire class of stale-reference bugs across cluster sync. A generation counter provides insurance for a hypothetical future where recycling becomes necessary.

### 2.2 Assertions

Each object carries a bag of assertions — statements about what it is:

```rust
enum Assertion {
    Tag(TagId),                                      // "this is electronic music"
    Attr { key: TagId, value: Value },               // "artist = Aphex Twin"
    Relation { predicate: TagId, target: ObjectId }, // "member_of playlist:900"
}

enum Value {
    Text(CompactString),
    Int(i64),
    Float(f64),
    Timestamp(i64),
    Blob(Vec<u8>),
}
```

Multi-valued attributes are natural — an object can have multiple assertions with the same key. A music track in two playlists simply has two `playlist` attributes.

The forward index distinguishes **direct** assertions (user/app explicitly set) from **materialized** assertions (added automatically by the ontology's implication engine):

```rust
enum TagOrigin {
    Direct,        // user/app explicitly tagged this object
    Materialized,  // added by implication engine
}
```

### 2.3 Query Algebra

Queries are set operations over object sets, evaluated bottom-up via bitmap algebra:

```rust
enum Query {
    HasTag(TagId),
    HasAttr { key: TagId, op: CmpOp, value: Value },
    Related { predicate: TagId, target: ObjectId },
    And(Vec<Query>),
    Or(Vec<Query>),
    Not(Box<Query>),
    IsA(TagId),   // ontology-aware: "vehicle" matches "car", "truck"
}

enum CmpOp { Eq, Ne, Lt, Le, Gt, Ge, Prefix, Contains }
```

Example — "all portable electronics from 2024, not discontinued":

```rust
And(vec![
    HasTag(tag("electronics")),
    HasTag(tag("portable")),
    HasAttr { key: tag("year"), op: Eq, value: Int(2024) },
    Not(Box::new(HasTag(tag("discontinued")))),
])
```

---

## 3. Ontology Layer

The ontology defines relationships between tags themselves. It is the schema of the filesystem and drives all storage behavior — placement, compression, encryption, chunking.

### 3.1 Tag Definitions

Tags carry semantics defined by the ontology:

```rust
struct TagDefinition {
    id: TagId,
    name: String,
    semantics: TagSemantics,
    implies: Vec<TagId>,
}

enum TagSemantics {
    Label,                    // simple label: "electronic", "favorite"
    Attribute { value_type: ValueType },  // key-value: "artist=Aphex Twin"
    Grouping,                 // unordered group: "genre:ambient"
    OrderedCollection {       // ordered: "playlist:workout", "album:SAW-II"
        element_constraint: Option<TagId>,
    },
    Hierarchical,             // "location:europe/france/paris"
}
```

`OrderedCollection` is a property of the tag, not of the objects. The storage layer sees it and maintains both a bitmap (for fast membership queries) and a sequence vector (for ordering). Adding a new collection type — recipe books, photo albums, project folders — is purely an ontology change, no code needed.

### 3.2 Tag Relations

```rust
enum TagRelation {
    ImpliedBy,          // "car" implies "vehicle"
    MutuallyExclusive,  // "active" vs "discontinued"
    Requires,           // "usb-c" requires "electronics"
    Alias,              // "laptop" = "notebook"
}
```

### 3.3 Implication DAG and Materialization

Tag implications form a directed acyclic graph. When an object is tagged "car", the materializer automatically inserts "vehicle" and "physical_object" into its tag set. Querying for "vehicle" is then a single bitmap lookup — no query expansion needed.

Materialized implications trade write-time cost (bitmap OR operation — microseconds even for millions of objects) for read-time speed. Since reads vastly outnumber writes and the ontology changes rarely, this is the clear winner.

### 3.4 Ontology-Driven Storage

The ontology informs all storage decisions:

```
Ontology says "playlist:*" is-a ordered-collection
  → Storage maintains bitmap + Vec<ObjectId> for sequence

Ontology says "video" is-a media
  → Skip compression (already compressed)
  → HCTR2 encryption (wide-block)
  → Prefer warm/cold tier placement

Ontology says "vm-image" is-a mutable-large-file
  → Enable CDC chunking
  → Enable per-block compression
```

---

## 4. Ontology Modules

Ontologies are not monolithic. They are composed from **modules** — self-contained, versioned packages that declare tags, relations, implications, and constraints.

### 4.1 Module Structure

```rust
struct OntologyModule {
    id: ModuleId,               // "systems.metta.music"
    version: SemVer,
    name: String,
    tags: Vec<TagDeclaration>,
    implications: Vec<Implication>,
    mutex_groups: Vec<MutexGroup>,
    constraints: Vec<Constraint>,
    requires: Vec<ModuleDependency>,
    installed_by: Vec<AppId>,   // refcount of installers
    core: bool,                 // built-in, cannot be removed
}
```

### 4.2 Distribution Format

Modules ship as declarative TOML files — no code, just declarations:

```toml
[module]
id = "systems.metta.music"
version = "2.1.0"
name = "Music Ontology"

[requires]
"systems.metta.core" = ">=1.0, <4.0"

[[tags]]
name = "artist"
semantics = "attribute"
value_type = "text"

[[tags]]
name = "playlist"
semantics = "ordered-collection"
element_constraint = "audio"

[[tags]]
name = "genre"
semantics = "grouping"
extensible = true

[[implications]]
from = "rock"
to = "genre"

[[implications]]
from = "flac"
to = "audio"

[[constraints]]
tag = "bpm"
requires = "audio"
```

Distribution channels: built-in (core ontology), app-bundled, community repository, or user-created.

### 4.3 Module Hierarchy

```
┌─────────────────────────────────────────────────┐
│  Core Ontology (built-in, cannot be removed)    │
│  file, text, image, audio, video, document ...  │
│  jpeg → image → file, mp3 → audio → file ...     │
├─────────────────────────────────────────────────┤
│  systems.metta.music (depends: core)            │
│  artist, album, genre, bpm, playlist:* ...      │
│  rock → genre, flac → audio ...                 │
├─────────────────────────────────────────────────┤
│  com.djapp.mixing (depends: core, music)        │
│  cue-point, beatgrid, key-signature ...         │
├─────────────────────────────────────────────────┤
│  com.photoapp.editor (depends: core)            │
│  raw-photo, lens, focal-length, face:* ...      │
└─────────────────────────────────────────────────┘
```

### 4.4 Installation / Merge

Module installation is transactional. The merge algorithm:

1. **Check dependencies** — all required modules present at compatible versions
2. **Check conflicts** — tag name collisions with incompatible semantics
3. **Check cycles** — new implications must not create DAG cycles
4. **Check mutex consistency** — warn if existing objects violate new mutex groups
5. **Apply** — register tags, add implications, rematerialize affected bitmaps
6. **Sync** — emit `OntologyDelta::InstallModule` to cluster

Rematerialization after adding an implication like "flac → audio" is a single bitmap OR: `bitmap_audio |= bitmap_flac` — microseconds even for 100K affected objects.

### 4.5 Module Upgrade

Upgrades compute a diff. Safe changes (adding tags, adding implications) apply directly. Dangerous changes (removing tags with data, changing semantics) require explicit migration or `--force`.

De-materialization when removing an implication computes which objects should lose the implied tag by checking if any _other_ implication still provides it — `to_remove = from_bitmap - other_sources_bitmap`.

### 4.6 Scrubbing (App Removal)

Module removal is reference-counted. Multiple apps can depend on the same module. When the last app referencing a module is removed, three scrub policies are available:

|Policy|Behavior|Use case|
|---|---|---|
|**Preserve** (default)|Tags become "orphaned" — still queryable, no module backing|Safe, data outlives apps|
|**RemoveUnique**|Remove tags unique to this module, keep shared ones|Moderate cleanup|
|**Purge**|Remove all tags + data this module introduced|"I want this gone"|

Orphaned tags remain fully functional and can be adopted by another module or user-created module:

```
$ mimir ontology orphans
  bpm          (was: com.djapp.mixing, removed 2024-03-15)
  cue-point    (was: com.djapp.mixing, removed 2024-03-15)
  These tags still have data on 12,450 objects.

$ mimir ontology adopt bpm --into systems.metta.music
```

Core principle: **data outlives apps.** Tags your music app created are still your data.

---

## 5. Index Layer

Five index structures, all RAM-resident with WAL persistence:

### 5.1 Tag Inverted Index

One roaring bitmap per tag. The core structure for all tag queries.

```
tag "electronics" → RoaringBitmap { 1, 5, 42, 99, 107, ... }
tag "portable"    → RoaringBitmap { 5, 42, 200, ... }

electronics AND portable → bitmap AND → { 5, 42 }
```

10M objects, 5000 tags, 20 tags/object: ~200–400 MB total. Intersections return in sub-millisecond time.

### 5.2 Tag Store Variants

Most tags are simple bitmaps. Ordered collections carry additional structure because the ontology says they should:

```rust
enum TagStore {
    Simple(RoaringBitmap),
    Ordered {
        members: RoaringBitmap,
        sequence: Vec<ObjectId>,
    },
    Ranked {
        members: RoaringBitmap,
        ranked: Vec<(ObjectId, f32)>,
    },
}
```

A playlist with 500 tracks: 4 KB sequence + 2 KB bitmap. 10,000 playlists: ~40 MB.

### 5.3 KV Equality Index

Treats `(key, value)` as a compound tag. Stored with composite keys `[tag_id: 4B][value_hash: 8B]`, enabling prefix scans over all values for a given key.

### 5.4 Range B+ Tree

For ordered queries (`year > 2020`, `size BETWEEN 1MB AND 10MB`). Keyed by `(attr_id, value, obj_id)`, producing roaring bitmaps via range scans.

### 5.5 Forward Index

Object → all its assertions. Used for tag listing, faceted exploration, sync, and distinguishing direct from materialized tags.

### 5.6 Faceted Exploration

"Given my current query, what tags exist on the matching set?" — intersect result bitmap with each candidate tag bitmap. At ~μs per AND, 5000 tags complete in <10ms. LRU cache avoids recomputation.

---

## 6. On-Disk Layout

### 6.1 Raw Disk Zones

The filesystem operates directly on raw block devices:

```
 0                    Superblock (4KB, duplicated at end of disk)
 4K                   Superblock copy
 8K                   Write-Ahead Log (64MB circular buffer)
 8K+64M               Allocation bitmap
 ...                  ┌───────────────────────────────────┐
                      │  Zone 1: INDEX ZONE               │
                      │  Tag bitmaps, B+ trees, ontology  │
                      │  ~1-5% of disk                    │
                      ├───────────────────────────────────┤
                      │  Zone 2: METADATA ZONE            │
                      │  Object records, location table   │
                      │  ~1-2% of disk                    │
                      ├───────────────────────────────────┤
                      │  Zone 3: BLOB ZONE                │
                      │  File content, large values       │
                      │  ~95% of disk                     │
                      └───────────────────────────────────┘
 end-4K               Superblock backup copy
```

### 6.2 Object Records

Fixed-size, array-indexed by ID for O(1) lookup:

```rust
#[repr(C)]
struct ObjectRecord {       // 128 bytes, cache-line aligned
    id: u64,
    generation: u32,            // for future ID reuse safety
    state: ObjectState,
    content_hash: [u8; 32],     // BLAKE3 of plaintext
    blob_offset: u64,
    blob_length: u64,
    created_ns: i64,
    modified_ns: i64,
    tag_count: u16,
    attr_count: u16,
    inline_tags: [u32; 8],      // covers 80%+ of objects
    overflow_offset: u64,
    compression: CompressionState,
    encryption: EncryptionState,
    stored_size: u64,
}
```

Lookup: single read at `zone2_base + id * 128`. 10M objects = 1.28 GB.

### 6.3 Location Table

Maps objects to physical extents, supporting multi-disk pools:

```rust
struct ObjectLocation {
    disk_id: u16,
    extent_offset: u64,
    extent_length: u64,
    replica_count: u8,
    replicas: [ReplicaRef; 3],
}
```

### 6.4 Write-Ahead Log

All mutations go through the WAL — 64 MB circular buffer on the fastest disk, mirrored to a second disk. Checkpointing flushes dirty bitmaps and metadata to their zones. Crash recovery replays from last checkpoint.

Extended oplog retention (compressed segments on disk) supports dormant subscriptions catching up after being offline.

---

## 7. Object Deletion

### 7.1 Deletion Protocol

Deletion proceeds through four phases:

```
Delete request
     │
     ▼
┌────────────────┐
│ 1. TOMBSTONE   │  Object marked deleted, invisible to queries.
│    (instant)   │  Sync op emitted to cluster.
└──────┬─────────┘
       │ background
       ▼
┌────────────────┐
│ 2. INDEX       │  Remove from all bitmaps, forward index,
│    CLEANUP     │  collection sequences. O(tags_per_object).
└──────┬─────────┘
       │ after grace period (all nodes caught up)
       ▼
┌────────────────┐
│ 3. BLOB        │  Free disk extents (or decrement chunk refs).
│    RECLAIM     │  Clear location table entry.
└──────┬─────────┘
       │ after tombstone expiry
       ▼
┌────────────────┐
│ 4. SLOT        │  Object record zeroed.
│    CLEARED     │  ID NOT reused.
└────────────────┘
```

Index cleanup iterates the forward index (which lists exactly which bitmaps contain this ID) rather than scanning all bitmaps — O(tags_on_object), not O(total_tags).

### 7.2 Tombstone Grace Period

Tombstones persist until all cluster nodes have acknowledged the deletion AND a minimum grace period (e.g., 7 days) has elapsed. This prevents a remote node from applying stale ops to a deleted object.

### 7.3 ID Non-Recycling

IDs are never recycled. With 48-bit local sequence per node, exhaustion is not a practical concern. Deleted slots create sparsity in the object record array (~10% waste at typical churn rates — 128 MB at 1M deletions, acceptable). A generation counter in each record provides insurance if recycling is ever needed.

### 7.4 Cluster Sync and Deletion

Stale ops targeting deleted objects are resolved by HLC ordering. The deletion's timestamp is compared with the op's timestamp — ops generated before the deletion are dropped as stale, ops generated after are logged as conflicts.

### 7.5 Bulk Deletion

Deleting a tag (and all its associations) is a single bitmap removal. The member objects are untouched — they just lose one tag. Deleting a playlist removes the grouping, not the music.

---

## 8. Multi-Disk Pool Management

### 8.1 Disk Topology

```rust
struct DiskDescriptor {
    id: DiskId,
    capacity: u64,
    used: u64,
    media_type: MediaType,    // NVMe, SSD, HDD, SMR_HDD
    tier: StorageTier,        // Hot, Warm, Cold, Glacier
    seq_read_mbps: u32,
    random_iops: u32,
    latency_us: u32,
}
```

### 8.2 Multi-Disk Layout

```
   NVMe (Disk 0)            SSD (Disk 1)             HDD (Disk 2)
 ┌──────────────┐         ┌──────────────┐         ┌──────────────┐
 │ Superblock   │         │ Superblock   │         │ Superblock   │
 │ WAL (primary)│         │ WAL (mirror) │         │              │
 │ Index Zone   │         │              │         │              │
 │ Metadata Zone│         │              │         │              │
 ├──────────────┤         ├──────────────┤         ├──────────────┤
 │ Hot blobs    │         │ Warm blobs   │         │ Cold blobs   │
 │ tag:active   │         │ default      │         │ tag:archive  │
 │ tag:scratch  │         │ placement    │         │ tag:backup   │
 └──────────────┘         └──────────────┘         └──────────────┘
```

### 8.3 Semantic Placement Rules

Placement rules bind semantic properties to physical topology:

```rust
enum PlacementRule {
    Pin { query: Query, tier: StorageTier },
    Prefer { query: Query, tier: StorageTier, priority: u8 },
    Replicate { query: Query, min_replicas: u8, across_disks: bool },
    Colocate { query: Query },
    AutoTier { hot_threshold_days: u32, warm_threshold_days: u32, cold_after: u32 },
}
```

### 8.4 Disk Operations

**Disk addition:** Semantic rebalance — archives move to HDD, active projects move to SSD. Unlike ZFS where only new writes go to the new disk.

**Disk removal:** Location table scan (10M objects in ~50ms). Each object migrated to a destination chosen by evaluating placement rules against its tags. Disk enters `Draining` state during removal — reads still served, no new writes.

**Resilver:** Proportional to data on the failed disk, not pool size. Prioritized by surviving replica count. ~20× faster than ZFS resilver for typical configurations.

**Automatic tiering:** Objects migrate between tiers based on tags and access patterns. Explicit placement rules take precedence, then access-time-based defaults.

---

## 9. Transform Pipeline

### 9.1 Pipeline Ordering

Every write: **hash → compress → pad → encrypt**. Reversed on read.

```
  Plaintext data
       │
       ├── content_hash = BLAKE3(plaintext)    ← integrity reference
       ▼
  ┌─────────────┐
  │ Compression │  Ontology-driven: skip for JPEG/MP3/video
  └──────┬──────┘
         ▼
  ┌─────────────────────┐
  │ Sector alignment    │  Pad to 4KB boundary
  └──────┬──────────────┘
         ▼
  ┌─────────────────────┐
  │ Disk encryption     │  Length-preserving (HCTR2 or XTS)
  │                     │  Tweak = object_id + sector_offset
  └──────┬──────────────┘
         ▼
  Raw ciphertext on disk
```

Content hash is always computed on original plaintext — dedup and integrity verification are independent of storage format.

### 9.2 Compression

Ontology-driven algorithm selection. Already-compressed formats (JPEG, MP3, H.264) are skipped entirely:

|File type|Ratio|Policy|
|---|---|---|
|JSON/XML|12:1|Zstd level 9|
|Source code|4:1|Zstd level 3|
|JPEG/MP3/video|1:1|**Skip**|

Granularity: per-object for files under ~16 MB, per-block (4 KB) for large files needing random access.

### 9.3 Encryption

Two fundamentally different regimes using the correct cryptographic mode for each:

|Component|Mode|Why|
|---|---|---|
|**Blob zone**|HCTR2-AES-128|Wide-block: entire sector changes on any byte edit. Hides internal structure. Length-preserving.|
|**Metadata zone**|XTS-AES-256|Fast random-access reads. Length-preserving.|
|**Index zone**|XTS-AES-256|Bulk decrypt at boot.|
|**WAL**|AES-256-GCM|Append-only with monotonic LSN as nonce. Authenticated.|
|**Sync traffic**|ChaCha20-Poly1305|Network messages. Constant-time on all platforms.|
|**Fallback**|Adiantum|Wide-block via ChaCha12, for ARM without crypto extensions.|

AES-GCM is **not** used for disk encryption — it expands data, catastrophically fails on nonce reuse (power loss), and requires per-sector stored nonces. XTS and HCTR2 derive all state from sector address and key.

**Tweak construction:** HCTR2 tweaks include the object ID so identical content in different objects encrypts differently.

⁉️This prevents deduplication though? #todo 

**Integrity:** BLAKE3 checksums on plaintext stored in the metadata zone (different key) detect tampering that XTS/HCTR2 cannot.

### 9.4 Key Hierarchy

```
User Passphrase / Hardware Key
         │
    Argon2id
         ▼
    Master KEK (256b)
         │
    ┌────┴────────────────────────────┐
    ▼                                 ▼
 Disk Keys                      Message Keys
    │                                │
    ├── XTS-AES-256 (512b)           ├── AES-GCM-256 (WAL)
    │   metadata + index             └── ChaCha20-Poly1305 (sync)
    ├── HCTR2-AES-128 (128b)
    │   blob zone pool-wide
    └── Per-tag HCTR2 keys (128b)
        HKDF(blob_pool_key, tag_id)
```

Per-tag blob keys enable sharing specific key scopes without revealing other data.

### 9.5 Trust Tiers

```rust
enum NodeTrust {
    Trusted,        // Has master key, full search
    SemiTrusted,    // Gets metadata key at boot, loses on reboot
    Untrusted,      // Stores/relays ciphertext only, cannot search
}
```

### 9.6 Selective Chunking

Content-defined chunking (FastCDC) is only applied where the ontology indicates benefit:

|Workload|Chunk?|Why|
|---|---|---|
|Music/photo/video|No|Immutable, pure overhead|
|VM images, databases|Yes (CDC)|Large, small edits, massive sync savings|
|Large immutable transfers|Maybe|Fixed-chunk for resumability|

Only ~1% of objects are typically chunked, keeping the chunk index small (~5 MB vs 4 GB if everything were chunked). When active: chunk plaintext, hash each chunk, compress per-chunk, encrypt.

---

## 10. Cluster Synchronization

### 10.1 Architecture

Every node replicates all metadata. Blob content lives only where placed or cached.

```
           Node A (laptop)        Node B (NAS)         Node C (S3)
         ┌────────────────┐    ┌────────────────┐    ┌─────────────┐
Metadata │ ██████████████ │    │ ██████████████ │    │ ████████████│
         │ ALL objects    │    │ ALL objects    │    │ ALL objects │
         │ ~500 MB        │    │ ~500 MB        │    │ ~500 MB     │
         ├────────────────┤    ├────────────────┤    ├─────────────┤
Content  │ ░░░░ 30%       │    │ ████████ 80%   │    │ █████████95%│
         │ hot/active     │    │ most things    │    │ everything  │
         └────────────────┘    └────────────────┘    └─────────────┘
```

### 10.2 Node Types

|Role|Description|
|---|---|
|**Full**|Metadata + local blobs + syncs with peers|
|**Thin**|Metadata + stubs only (phones, small SSDs)|
|**BlobStore**|Blobs only (S3, NAS without compute)|
|**Hub**|Always-on relay for intermittently connected nodes|

### 10.3 Content Presence (Hydration Model)

```rust
enum ContentPresence {
    Local { disk_id: u16, extent_offset: u64, extent_length: u64 },
    Remote { origin_node: NodeId, mirrors: Vec<NodeId> },
    Hydrating { source: NodeId, progress_bytes: u64, total_bytes: u64 },
    Cached { disk_id: u16, extent_offset: u64, fetched_at: Timestamp },
    Partial { stub_length: u64, full_length: u64, origin_node: NodeId },
}
```

Hydration policy is ontology-driven — `Pin`, `StubOnly`, `Prefetch`, `AutoEvict` rules using the same query language as placement rules.

### 10.4 Sync Protocol — Operation Log

Mutations generate sync operations replicated across nodes via hybrid logical clocks (HLC) for total ordering without coordination:

```rust
struct SyncOp {
    timestamp: HybridTimestamp,   // wall_ms + logical + node_id
    origin_node: NodeId,
    sequence: u64,
    op: SyncOpKind,
}
```

### 10.5 Bitmap Delta Sync

Roaring bitmaps support efficient delta encoding:

```rust
let added   = &new_bitmap - &old_bitmap;   // tiny bitmap
let removed = &old_bitmap - &new_bitmap;
// Send ~200 bytes instead of ~50KB full bitmap
```

### 10.6 Sync Modes

**Incremental** (normal): ops since last watermark. **Snapshot** (catch-up): compressed full state (~400 MB for 10M objects). Threshold: >1M ops behind triggers snapshot.

### 10.7 Conflict Resolution

Most tag operations are commutative. For conflicts: `AddWins` (tag add beats remove, safer) or `LastWriterWins` (timestamp-based).

### 10.8 Bandwidth Budget

|Scenario|Size|
|---|---|
|Daily incremental sync|~850 KB|
|Full snapshot (new node)|~400 MB|

### 10.9 Remote Storage Backends

S3, SFTP, IPFS slot in via a `BlobBackend` trait. The placement engine treats them as additional tiers. Placement rules work identically — `Prefer { query: HasTag("archive"), tier: Glacier }` sends archives to S3 Glacier.

⁉️can provide a namespace layer on top of ipfs, the missing link!

---

## 11. Query Subscriptions (Watch System)

Mímisbrunnr replaces inotify with **query-based subscriptions** — persistent, named watches that track changes to any objects matching a query.

### 11.1 Why Not inotify

|inotify|Mímisbrunnr subscriptions|
|---|---|
|Watches paths|Watches queries over tags|
|Must know paths in advance|Query matches future objects|
|Misses events while offline|Cursor-based catch-up from oplog|
|Queue overflow = lost events|Oplog is persistent|
|Recursive watch is expensive|Bitmap intersection is O(1)|
|No semantic filtering|Query IS the filter|

### 11.2 Subscription Structure

```rust
struct Subscription {
    id: SubscriptionId,
    name: String,
    query: Query,
    interest: ChangeInterest,       // TAG_ADDED, CONTENT_CHANGED, ENTERED, EXITED, ...
    cursor: HybridTimestamp,        // where we've read up to in the oplog
    owner: SubscriptionOwner,
    state: SubscriptionState,       // Active or Dormant
    retention: Duration,
}
```

The key events `ENTERED` and `EXITED` don't exist in inotify. When a file gains a tag that makes it match your query, that's `ENTERED`. When it loses a tag and falls out, that's `EXITED`.

### 11.3 Inverted Subscription Index

Subscriptions are indexed by which tags they reference. When tag X changes, only subscriptions mentioning tag X are evaluated — not all subscriptions:

```rust
struct SubscriptionEngine {
    tag_to_subs: HashMap<TagId, Vec<SubscriptionId>>,
    attr_to_subs: HashMap<TagId, Vec<SubscriptionId>>,
    match_cache: HashMap<SubscriptionId, RoaringBitmap>,
}
```

Cost per mutation: ~microseconds (hash lookup + bitmap contains + conditional query evaluation).

### 11.4 Atomic Subscribe + Snapshot

No race condition between setup and initial state (unlike inotify + readdir):

```rust
fn subscribe(query: Query, interest: ChangeInterest)
    -> (SubscriptionId, Vec<ObjectId>)
{
    // Atomic: snapshot cursor and result set at same moment
    let cursor = oplog.latest_timestamp();
    let initial = execute_query(&query);
    // Any mutation after cursor will be delivered as an event
    (register(query, interest, cursor), initial)
}
```

### 11.5 Offline Catch-Up

When an agent reconnects after being dormant:

**Small gap (minutes/hours):** Replay individual ops from cursor — preserves full event sequence, O(ops_missed).

**Large gap (days/weeks):** Diff current vs cached result set — two bitmap operations, O(result_set_size). Loses individual event ordering but correct summary in milliseconds instead of seconds.

**Oplog expired:** Falls back to result set diff, which doesn't need the oplog at all.

### 11.6 Batching and Debouncing

Configurable per subscription — debounce window, max delay, max events, coalescing (same object modified multiple times → deliver only latest state).

### 11.7 Subscription Lifecycle

```
SUBSCRIBE → ACTIVE (live events) → DORMANT (agent offline, cursor frozen)
                                       → CATCH-UP (agent reconnects)
                                           → ACTIVE (live events resume)
```

Subscriptions persist across reboots (stored in metadata zone). Garbage-collected after retention period expires without reconnection.

### 11.8 Ontology-Aware Subscriptions

A subscription watching `HasTag("audio")` automatically widens when a new ontology module adds the implication "m4a → audio" — existing m4a files generate `ENTERED` events. Impossible with inotify.

### 11.9 Example: Build System Watcher

```rust
let (sub, sources) = mimir.subscribe(
    And(vec![
        HasAttr { key: "project", op: Eq, value: Text("vesper") },
        HasTag(tag("source")),
    ]),
    ChangeInterest::CONTENT_CHANGED | ChangeInterest::CREATED | ChangeInterest::DELETED,
    BatchConfig { debounce: 200ms, max_delay: 2s, coalesce: true },
);

loop {
    let batch = mimir.recv_batch(sub).await;
    trigger_incremental_build(&batch);
}
```

---

## 12. Unix Interoperability

### 12.1 Path Projections

Unix paths are not how files live in Mímisbrunnr — they are metadata about how files appear when projected onto a Unix tree. The same object can have different paths in different contexts:

```
Object: vesper-kernel-binary

  path-context:project-tree
    unix-path="target/aarch64-unknown-none/release/vesper"

  path-context:rpi4-sdcard
    unix-path="/boot/kernel8.img"
    unix-mode=0o755  unix-uid=0  unix-gid=0

  path-context:debian-package
    unix-path="usr/lib/vesper/kernel"
```

### 12.2 Path Contexts

A **path context** is itself an object — a named Unix filesystem projection containing a manifest of path-to-object mappings:

```rust
struct PathProjection {
    context: String,
    entries: Vec<ProjectedEntry>,
}

struct ProjectedEntry {
    object: ObjectId,
    path: String,
    entry_type: ProjectedEntryType,
}

enum ProjectedEntryType {
    File { mode: u32, uid: u32, gid: u32 },
    Symlink { target: String },
    Directory { mode: u32 },    // virtual — synthesized from paths
}
```

Directories are virtual entries synthesized from the paths — no directory objects exist in Mímisbrunnr.

### 12.3 Storage

Dual storage for each projection entry: the context object holds the manifest (fast full-tree export), and each object carries its path assertion for that context (fast per-object lookup). Both updated atomically.

### 12.4 Export

Generating a tarball reads the manifest sequentially:

```
mimir project export rpi4-sdcard --format tar.gz -o sdcard.tar.gz
mimir project export debian-package --format deb -o vesper_0.1.0_arm64.deb
```

### 12.5 Import

Ingesting a Unix tree: walk directory, create objects (dedup by content hash), auto-tag based on path conventions and file extensions:

```
mimir project import ./vesper-checkout \
    --context project-vesper --project vesper --auto-tag
```

### 12.6 FUSE Bridge

For tools that require Unix paths, a FUSE shim mounts a specific path context:

```
brunnr mount-unix --context project-vesper /mnt/vesper
```

Read/write through the FUSE bridge goes through Mímisbrunnr — blobs updated, tags preserved.

### 12.7 Multiple Projections

The same objects serve all projections simultaneously — no file copying. A kernel binary has a project-tree path, an install path, a package path, and a TFTP debug path, all referring to the same (or different, for stripped vs debug) blob.

Core principle: **Unix paths are a projection, not the truth.** Like projecting a 3D model onto a 2D blueprint.

---

## 13. Example Ontology: Source Code Projects

The `systems.metta.dev` ontology demonstrates how rich domain structure maps to tags without introducing hierarchy:

### 13.1 Dimensions

|Dimension|Examples|
|---|---|
|**Project identity**|`project=vesper`, `component=kernel`, `workspace=metta-systems`|
|**File role**|`source`, `generated`, `implementation`, `test`, `object`, `executable`, `bundle`|
|**Language**|`lang=rust`, `lang=c`, `lang=asm`|
|**Build configuration**|`profile=release`, `target=aarch64-unknown-none`, `opt-level=2`, `lto`|
|**Lifecycle**|`current`, `stale`, `dirty`, `released`, `pinned`|
|**Derivation**|`derived-from→`, `links→`, `bundles→`|

### 13.2 Implication Hierarchy

```
                              file
                            ╱      ╲
                     source          generated
                   ╱  │  ╲         ╱   │    ╲
         implementation│ resource  object library executable bundle
                    test│  ╱ ╲     codegen  doc-output
                   bench  icon shader      test-result
                build-script  font         intermediate
                   config  texture
                documentation
                     spec
```

### 13.3 Queries That Become Trivial

```
"All source in the vesper kernel"
  project=vesper AND component=kernel AND source

"All aarch64 release artifacts"
  generated AND target=aarch64-unknown-none AND profile=release

"Full provenance of the distribution image"
  Transitive closure over derived-from and bundles relations

"Stale artifacts safe to clean"
  generated AND stale AND NOT pinned

"What have I been working on this week?"
  source AND modified > 7-days-ago → distinct(project)
```

### 13.4 Build Integration

Build systems emit tagging operations as part of compilation — tagging outputs with configuration, recording derived-from relations. Clean operations become queries, not path globs. Rebuild detection uses content hashes and derived-from relations instead of filesystem timestamps.

---

## 14. Performance

### 14.1 Tag Query

```
Bitmap intersection (3-4 tags):          ~1 μs
ObjectRecord lookup (cached):            ~4 μs
ObjectRecord lookup (disk):              ~50 μs
1000 results with metadata:              ~1-5 ms
```

### 14.2 File Open

```
Location table → extent:                 ~1 μs (cached)
HCTR2 decrypt (AES-NI):                 ~50 μs/MB
Zstd decompress:                         ~80 μs/MB
NVMe sequential read:                    ~200 μs/MB
```

### 14.3 Tag Mutation

```
WAL append:                              ~2 μs (NVMe)
Bitmap update:                           ~0.1 μs
Forward index update:                    ~0.1 μs
Total:                                   ~2.5 μs
```

### 14.4 Subscription Evaluation per Mutation

```
Inverted sub index lookup:               ~0.1 μs
Bitmap contains check:                   ~0.01 μs per sub
Full query eval (if needed):             ~1 μs per sub
Total (3 affected subs):                 ~3 μs
```

### 14.5 Transform Overhead (per MB)

```
                 No transforms   +Compression   +Encryption   +Both
Write (NVMe)     ~200 μs         +150 μs        +50 μs        +200 μs
Read (NVMe)      ~200 μs         +80 μs         +50 μs        +130 μs
Write (HDD)      ~10 ms          invisible      invisible     invisible
```

---

## 15. Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                        CLIENT LAYER                              │
│   CLI (mimir/brunnr) │ Library API │ Optional FUSE shim         │
└────────────────────────────┬─────────────────────────────────────┘
                             │
┌────────────────────────────▼─────────────────────────────────────┐
│                       QUERY ENGINE (Mímir)                       │
│   Parser → Planner → Set Executor (bitmap algebra) → Facets     │
└───────┬───────────────┬──────────────┬───────────────────────────┘
        │               │              │
┌───────▼───────┐ ┌─────▼──────┐ ┌────▼──────────┐
│  ONTOLOGY     │ │ INDEX LAYER│ │ METADATA      │
│               │ │ (RAM +WAL) │ │ LAYER         │
│ Module system │ │            │ │               │
│ Implication   │ │ Tag Bitmap │ │ Object Table  │
│ DAG           │ │ KV Index   │ │ Location Table│
│ Materializer  │ │ Range B+   │ │               │
│               │ │ Forward Idx│ │               │
└───────┬───────┘ └─────┬──────┘ └────┬──────────┘
        │               │             │
┌───────▼───────────────▼──────────────▼───────────────────────────┐
│                    SUBSCRIPTION ENGINE                          │
│   Inverted sub index │ Match cache │ Catch-up │ Batch/debounce  │
├──────────────────────────────────────────────────────────────────┤
│                    TRANSFORM PIPELINE                            │
│   BLAKE3 → Compress (Zstd/LZ4) → Pad → Encrypt                  │
│   (HCTR2 blobs / XTS metadata / GCM WAL / ChaCha sync)         │
├──────────────────────────────────────────────────────────────────┤
│                 PLACEMENT & TIERING ENGINE                        │
│   Pin/Prefer/Replicate/Colocate/AutoTier rules                   │
│   Tag queries → ideal tier → migration planner → allocator       │
├──────────────────────────────────────────────────────────────────┤
│                   TRANSACTION LAYER                               │
│   WAL (64MB circular, mirrored) → Checkpoint → Recovery          │
│   Extended oplog retention for dormant subscriptions             │
├──────────────────────────────────────────────────────────────────┤
│                     POOL MANAGER                                  │
│   Topology │ Rebalancer │ Resilverer │ Health Monitor            │
├──────────────────────────────────────────────────────────────────┤
│                   PHYSICAL STORAGE                                │
│   NVMe (Hot) │ SSD (Warm) │ HDD (Cold) │ S3/SFTP (Glacier)     │
├──────────────────────────────────────────────────────────────────┤
│                  CLUSTER SYNC LAYER                               │
│   SyncOps (HLC) ←→ Bitmap deltas ←→ Blob hydration             │
│   Node A ◄──► Node B ◄──► Hub ◄──► Node C                      │
├──────────────────────────────────────────────────────────────────┤
│                UNIX INTEROP LAYER                                 │
│   Path contexts │ Projections │ FUSE bridge │ Tarball export     │
└──────────────────────────────────────────────────────────────────┘
```

---

## 16. Comparison with Existing Systems

|Capability|ZFS|ext4/btrfs|Mímisbrunnr|
|---|---|---|---|
|File discovery|Path traversal|Path traversal|Tag query (~1 μs)|
|Metadata query|None|Limited xattr|Full query algebra + ontology|
|Change watching|Not built-in|inotify (path-based)|Query subscriptions with offline catch-up|
|Placement control|Per-dataset|None|Per-object, tag-driven|
|Tiering|Manual special vdev|None|Automatic, ontology-driven|
|Compression|Per-dataset|Per-filesystem|Per-object, ontology-driven|
|Encryption|Dataset-level|dm-crypt layer|Per-object, mixed modes (HCTR2/XTS/GCM)|
|Resilver time|∝ pool size|N/A|∝ affected data only|
|Disk add rebalance|None (new writes only)|None|Semantic migration|
|Cluster search|Not supported|Not supported|Full metadata on every node|
|Redundancy|Per-vdev, uniform|RAID layer|Per-object (tag:"critical" → 3×)|
|Schema extensibility|None|None|Ontology modules (install/upgrade/scrub)|
|Unix compatibility|Native|Native|Path projections + FUSE bridge|

---

## 17. Key Design Decisions

|Decision|Choice|Rationale|
|---|---|---|
|No POSIX paths|Tags + ontology queries|Associative access is fundamentally more expressive|
|Roaring bitmaps|Primary index structure|Sub-ms intersection, excellent delta encoding for sync|
|Ontology is the schema|Tag semantics drive storage|New collection types require zero code changes|
|Ontology modules|Versioned, ref-counted packages|Apps bring schemas; data outlives apps|
|Materialized implications|Insert implied tags at write time|Read path stays a single bitmap lookup|
|Array-indexed metadata|O(1) by object ID|Faster than any tree for sequential IDs|
|ID non-recycling|48-bit space, never reuse|Eliminates stale-reference bugs across cluster|
|Separate zones|Index / metadata / blob|Each optimized for its access pattern|
|HCTR2 for blobs|Wide-block disk encryption|Hides internal structure; correct for at-rest|
|XTS for metadata|Narrow-block disk encryption|Fast random access; structure leak acceptable|
|GCM/ChaCha for messages|AEAD for WAL + sync|Authentication + unique nonces in append-only contexts|
|Full metadata replication|Every node holds all tags|Search is always local, instant|
|Oplog sync with HLC|Operation-based replication|Commutative ops minimize conflicts; sub-MB daily bandwidth|
|Ontology-driven chunking|CDC only for mutable large files|Avoids 16× metadata overhead for immutable media|
|Query subscriptions|Replace inotify entirely|Semantic filtering, offline catch-up, no lost events|
|Path projections|Unix paths as export metadata|Same object serves multiple Unix tree layouts|

---

## Appendix A: CLI Reference

```
# Pool management
brunnr create /dev/nvme0n1 /dev/sda        # create pool across disks
brunnr mount                               # mount the well
brunnr status                              # pool health, disk usage, tiers
brunnr add-disk /dev/sdb --tier warm       # add disk with semantic rebalance
brunnr remove-disk /dev/sda                # drain and remove

# Object operations
mimir tag obj:42 ambient chill             # add tags
mimir untag obj:42 chill                   # remove tag
mimir set obj:42 artist "Aphex Twin"       # set attribute
mimir info obj:42                          # show all assertions

# Queries
mimir query "electronic AND year:2024"
mimir query "project=vesper AND source AND lang=rust"
mimir explore electronic                    # faceted exploration

# Ontology management
mimir ontology list                         # installed modules
mimir ontology install systems.metta.music  # install module
mimir ontology remove com.djapp.mixing      # scrub (preserve data)
mimir ontology orphans                      # show orphaned tags
mimir ontology adopt bpm --into my-module   # adopt orphan

# Subscriptions
mimir watch "project=vesper AND source" --name build-watcher
mimir watch list
mimir watch catch-up build-watcher
mimir watch stream "source" --format json

# Path projections
mimir project create-context rpi4-sdcard
mimir project set-path obj:42 rpi4-sdcard "/boot/kernel8.img"
mimir project export rpi4-sdcard --format tar.gz -o sdcard.tar.gz
mimir project import ./checkout --context src --auto-tag
mimir project tree rpi4-sdcard
brunnr mount-unix --context project-vesper /mnt/vesper
```
