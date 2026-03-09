This is a great question that sits at the intersection of information retrieval, database internals, and knowledge representation. Let me walk through the landscape from most practical/proven to more exotic.

## The Core Problem

You have a sparse matrix: millions of objects × thousands of possible labels, where each object has maybe 5–50 labels out of thousands. You want to answer queries like "give me all objects tagged `electronics` AND `portable` AND NOT `discontinued`" — fast, from disk.

## Tier 1: The Workhorses

**Roaring Bitmaps + Inverted Index**

This is the dominant approach in practice. For each tag/category value, you maintain a compressed bitmap of which object IDs have that label.

```
tag:"electronics"  → [1, 0, 1, 1, 0, 0, 1, ...] (compressed)
tag:"portable"     → [0, 0, 1, 1, 0, 0, 0, ...] (compressed)

query: electronics AND portable → bitwise AND → [0, 0, 1, 1, 0, 0, 0, ...]
```

Why roaring bitmaps specifically: classic bitmaps waste space on sparse data. Roaring bitmaps adaptively switch between array, bitmap, and run-length encoding per 65536-ID chunk. Boolean ops stay O(n) but on _compressed_ representation.

Concrete numbers: 10M objects, 5000 unique tags, average 20 tags per object → roughly 200–500 MB on disk with roaring compression. Intersection of two tags returns in microseconds to low milliseconds.

Rust crates: `roaring` (the standard), or build on top of `tantivy` which uses this internally.

**Tantivy (Rust-native Lucene)**

If you want a batteries-included solution, Tantivy _is_ an inverted index engine. You model each object as a "document" with faceted fields:

```rust
let mut schema_builder = Schema::builder();
schema_builder.add_facet_field("category", FacetOptions::default());
schema_builder.add_text_field("tags", STRING | STORED);
// ... index your objects, query with boolean combinations
```

Tantivy's `Facet` type was literally designed for hierarchical ontologies — it stores paths like `/electronics/portable/tablet` and lets you query at any prefix level. This gives you the "filesystem navigation" feel directly.

On disk it's a segment-based inverted index (like Lucene), very cache-friendly, mmap'd.

## Tier 2: Columnar / Analytical

**Parquet + DuckDB (or DataFusion)**

If your access pattern is more analytical ("how many objects match X grouped by Y"), a columnar approach shines. Store each metadata dimension as a column; dictionary encoding makes categorical columns tiny:

```
object_id | category      | tags (list)           | price_range
----------|---------------|-----------------------|------------
1         | "electronics" | ["usb-c", "portable"] | "mid"
2         | "furniture"   | ["wood", "outdoor"]   | "high"
```

Dictionary encoding turns `"electronics"` into a 2-byte integer repeated millions of times → extreme compression. Parquet files of 10M rows with 50 metadata columns typically land at 100–300 MB.

DuckDB or DataFusion (Rust-native, Arrow-based) can query this directly from disk with predicate pushdown — it only reads the columns and row groups it needs.

Good when: you want SQL-like flexibility, aggregations, and your queries are "scan + filter" rather than point lookups.

## Tier 3: Graph / Triple Stores

When relationships between tags matter (not just "object has tag" but "tag X is-a subclass of tag Y", "tag X implies tag Z"):

**Oxigraph** (Rust-native RDF/SPARQL store) — stores subject-predicate-object triples. Your data becomes:

```turtle
:obj42 :hasTag :electronics .
:obj42 :hasTag :portable .
:electronics :subClassOf :product .
```

Then SPARQL gives you transitive queries for free: "find all objects tagged with anything that's a subclass of `:product`." This is where actual _ontology_ reasoning lives.

Trade-off: slower for pure tag intersection (10–100× vs roaring bitmaps), but handles schema evolution and relationship queries that flat indexes can't.

**sled / RocksDB with composite keys** — the DIY version. Encode keys like `tag:electronics:obj42` and do range scans. Simple, fast for point queries, but boolean combinations require manual merge logic.

## Tier 4: Emerging / Specialized

**Qdrant / Meilisearch faceted filtering** — if you also need fuzzy text search or vector similarity alongside tag filtering, these combine ANN indexes with faceted filters.

**Bloom filter indexes** — if you're okay with probabilistic "definitely not / maybe yes" pre-filtering before hitting the real index. Tiny on disk. Useful as a first-pass filter when data is sharded.

**Bitmask-based ECS stores** (like `hecs` or `bevy_ecs` use) — if your access pattern is more "iterate all objects with components X, Y, Z", the archetype-based storage from game ECS systems is surprisingly applicable. Objects with the same tag combination are stored contiguously in memory.

## My Recommendation for Your Case

For millions of objects with filesystem-like navigation:

```
┌─────────────────────────────────────┐
│         Application Layer           │
│   (filesystem-like path queries)    │
├─────────────────────────────────────┤
│    Tantivy with Facet fields        │  ← hierarchical tag navigation
│    or                               │     boolean queries, ranked results
│    Roaring bitmap custom index      │  ← if you want full control
├─────────────────────────────────────┤
│    mmap'd segment files on disk     │  ← OS page cache handles hot data
└─────────────────────────────────────┘
```

If you want the filesystem metaphor literally — where users "cd" into `/electronics/portable/` and "ls" to see matching objects — Tantivy's facet system maps almost 1:1. You'd index hierarchical paths and use facet counts to show "subdirectories" with their object counts, exactly like how e-commerce faceted navigation works.

If you'd rather build from primitives for tighter control (which given your kernel work I suspect you might prefer), `roaring` + a simple segment file format + mmap gives you a very lean on-disk representation where you own every byte.

---

Good — that opens up a much more interesting design space. No path translation, no `readdir` theater, no pretending tags are directories. Pure associative storage.

## The Data Model

An object has an ID and a bag of **assertions** about it:

```rust
type ObjectId = u64;
type TagId = u32;

// Simple tags:        object 42 IS "portable"
// Key-value pairs:    object 42 HAS author = "Knuth"  
// Typed values:       object 42 HAS year = 2024u32
// Relations:          object 42 RELATED_TO object 99

enum Assertion {
    Tag(TagId),
    Attr { key: TagId, value: Value },
    Relation { predicate: TagId, target: ObjectId },
}

enum Value {
    Text(CompactString),
    Int(i64),
    Float(f64),
    Timestamp(i64),
    Blob(Vec<u8>),  // small inline values
}
```

This is deliberately close to RDF triples (subject-predicate-object) but without the URI ceremony. Every assertion is really `(ObjectId, PredicateId, Value?)`. The tag case is just a predicate with no value — "object 42 has property `portable`" is `(42, portable, ∅)`.

## The Query Language

Without POSIX, you get to design a proper query algebra. The natural model is **set operations over object sets**:

```rust
enum Query {
    // Primitives
    HasTag(TagId),
    HasAttr { key: TagId, op: CmpOp, value: Value },
    Related { predicate: TagId, target: ObjectId },
    
    // Combinators  
    And(Vec<Query>),
    Or(Vec<Query>),
    Not(Box<Query>),
    
    // Ontology-aware
    IsA(TagId),  // includes subtypes: "vehicle" matches "car", "truck"
}

enum CmpOp { Eq, Ne, Lt, Le, Gt, Ge, Prefix, Contains }
```

So a query like "all portable electronics from 2024 that aren't discontinued" is:

```rust
And(vec![
    HasTag(tag("electronics")),
    HasTag(tag("portable")),
    HasAttr { key: tag("year"), op: Eq, value: Int(2024) },
    Not(Box::new(HasTag(tag("discontinued")))),
])
```

Execution is bottom-up: each leaf returns a `RoaringBitmap`, combinators are bitwise ops. The only tricky one is `HasAttr` with range comparisons — that needs an ordered index, not just a bitmap.

## Index Structures

You need three different indexes for three different query shapes:

### 1. Tag presence → Roaring Bitmaps

The core. One bitmap per tag. Already discussed, scales beautifully.

```
tag "electronics" → RoaringBitmap { 1, 5, 42, 99, 107, ... }
tag "portable"    → RoaringBitmap { 5, 42, 200, ... }

electronics AND portable → bitmap AND → { 5, 42 }
```

### 2. Key-value equality → Hash-partitioned bitmaps

For `author = "Knuth"`, you need a bitmap per (key, value) pair:

```
(author, "Knuth")  → RoaringBitmap { 42, 300, 812 }
(author, "Lamport") → RoaringBitmap { 15, 77 }
```

This is just the tag index generalized — treat `(key, value)` as a compound tag. Works perfectly for equality and exact match. Store in redb with a composite key:

```rust
// key layout: [tag_id: 4 bytes][value_hash: 8 bytes]
// This gives you prefix scans over all values for a given key
```

### 3. Ordered attributes → B+ tree range index

For `year > 2020` or `size BETWEEN 1MB AND 10MB`, you need values sorted. This is where redb's B+ tree helps directly:

```rust
// Table keyed by (attr_id, value, object_id)
// Enables range scans: all objects where year ∈ [2020, 2024]
const RANGE_INDEX: TableDefinition<(u32, i64, u64), ()> = 
    TableDefinition::new("range_idx");

fn range_query(db: &Database, attr: u32, min: i64, max: i64) -> RoaringBitmap {
    let txn = db.begin_read().unwrap();
    let table = txn.open_table(RANGE_INDEX).unwrap();
    let mut result = RoaringBitmap::new();
    
    let range = (attr, min, u64::MIN)..=(attr, max, u64::MAX);
    for entry in table.range(range).unwrap() {
        let ((_attr, _val, obj_id), _) = entry.unwrap();
        result.insert(obj_id as u32);
    }
    result
}
```

Then the query planner intersects this result with the bitmap results from the tag/equality indexes. All three return `RoaringBitmap`, so they compose cleanly.

## The Ontology Layer

This is where it gets genuinely interesting and distinct from "just a database with tags." The ontology defines relationships _between tags themselves_:

```rust
enum TagRelation {
    ImpliedBy,    // "car" implies "vehicle"  
    MutuallyExclusive, // "active" vs "discontinued"
    Requires,     // "usb-c" requires "electronics" (soft constraint)
    Alias,        // "laptop" = "notebook"
}
```

**Tag implication** is the most useful one. If your ontology says `car → vehicle → physical_object`, then querying for `vehicle` should match objects tagged only with `car`. Two implementation strategies:

**Materialized** — when you tag object 42 as `car`, automatically also insert `vehicle` and `physical_object` into its tag set. Queries stay simple bitmap lookups. Cost: writes are heavier, and changing the ontology requires recomputing (but this is a batch bitmap OR operation, fast with roaring).

**Query-time expansion** — when someone queries `vehicle`, expand it to `OR(vehicle, car, truck, bus, ...)`. No extra write cost, but the query planner needs the ontology graph and OR over many bitmaps is slower than a single lookup.

For a filesystem-like system where reads vastly outnumber writes and the ontology changes rarely, **materialized** is the clear winner. You'd maintain a DAG of implications:

```rust
struct Ontology {
    // tag → all tags it implies (transitive closure, precomputed)
    implies: HashMap<TagId, Vec<TagId>>,
    // tag → all tags that imply it (for expansion queries)  
    implied_by: HashMap<TagId, Vec<TagId>>,
    // mutual exclusion groups
    mutex_groups: Vec<HashSet<TagId>>,
}

impl Ontology {
    fn tags_to_materialize(&self, tag: TagId) -> impl Iterator<Item = TagId> {
        // Returns tag + everything it implies
        std::iter::once(tag).chain(self.implies[&tag].iter().copied())
    }
}
```

## Object Store

Without POSIX, you have freedom here too. The interesting option is **content-addressable with refcounting**:

```rust
struct ObjectMeta {
    content_hash: [u8; 32],   // BLAKE3
    size: u64,
    created: i64,
    modified: i64,
    // No name! Names are just another tag/attr
}
```

Files have no intrinsic name. `name` is just `HasAttr { key: "name", value: "paper.pdf" }`. Multiple names? Multiple assertions. No name at all? Perfectly valid — the object exists and is findable by its other properties.

Content storage itself could be a simple content-addressed blob store:

```
blobs/
  ab/cd/abcd1234...  (BLAKE3 hash prefix sharding)
```

Or for better performance on large files, chunked (like bup/restic) with a chunk-to-object mapping table. But start simple.

## Putting It Together

```
┌─────────────────────────────────────────────┐
│  Query Engine                               │
│  parse → plan → execute → collect           │
│  set algebra over RoaringBitmaps            │
├──────────────┬──────────────┬───────────────┤
│ Tag Index    │ KV Index     │ Range Index   │
│ tag→bitmap   │ (k,v)→bitmap │ B+tree scan   │
│              │              │ → bitmap      │
├──────────────┴──────────────┴───────────────┤
│  redb (single file, ACID, mmap'd)           │
├─────────────────────────────────────────────┤
│  Ontology (materialized implications)       │
├─────────────────────────────────────────────┤
│  Blob Store (content-addressed flat files)  │
└─────────────────────────────────────────────┘
```

The entire metadata layer (tags, attributes, ontology) lives in a single redb file. At 10M objects × 20 tags average, you're looking at maybe 500 MB for all indexes. The blob store is separate and can live on a different device, filesystem, or even be networked.

For the on-disk engine, **redb** is worth a serious look — pure Rust, ACID, B+ tree, mmap'd, no C dependencies. Simpler than sled (which had durability issues historically), more embeddable than RocksDB.

## The Hard Open Question

**Faceted exploration** — how do users discover what to query? In a hierarchical filesystem, you `ls` and see what's there. In your system, the equivalent is "given my current query, what tags exist on the matching set and how many objects have each?" This is the co-occurrence counting problem. With roaring bitmaps it's tractable (intersect current result with each candidate tag's bitmap, count), but it's O(num_tags) per exploration step.

This is where you might want a secondary structure — a precomputed tag co-occurrence sketch (HyperLogLog per tag pair) or simply caching the top-N most common tags in the current result set.

## Prior Art Worth Studying

**TMSU** (Go) — the most practically successful tag filesystem. Virtual FUSE filesystem over a SQLite database. Its path semantics are well-thought-out. Study its UX decisions even if you don't want its implementation.

**BeOS/Haiku's BFS** — the only shipping OS filesystem with queryable attributes baked in. Every file has typed attributes, and you can do `query "type==audio && bitrate>128"` from the shell. Haiku is still actively developed and the source is readable.

**WinFS** (Microsoft, cancelled) — the most ambitious attempt. Failed partly because the ontology layer was too complex and too slow. `The lesson`: keep the tag schema simple and let complexity emerge from combinations, don't build elaborate OWL-style ontologies into the core.

# On-disk format

You're designing something that has more in common with a database storage engine than a traditional filesystem. The data has three very different access patterns that want different on-disk representations:

```
1. Tag bitmaps      — large, read-heavy, sequential scan, rarely mutated
2. Object metadata  — small records, point lookups by ID, frequent updates
3. Blob content     — large, streaming read/write, append-mostly
```

Putting all three through the same storage structure is a mistake. Traditional filesystems do this (everything is inodes + data blocks) and pay for it. You can do better.

### Disk Geometry

At the raw block device level, you're working with sectors (512B or 4KB). The fundamental layout:

```
Disk Layout (single device)
═══════════════════════════════════════════════════════════════

 0                    Superblock (4KB, duplicated at end of disk)
 4K                   Superblock copy
 8K                   Write-Ahead Log (fixed region, 64MB)
 8K+64M               Allocation bitmap (1 bit per 4KB block)
 ...                  ┌─────────────────────────────────────┐
                      │  Zone 1: INDEX ZONE                 │
                      │  Tag bitmaps, B+ trees, ontology    │
                      │  ~1-5% of disk                      │
                      ├─────────────────────────────────────┤
                      │  Zone 2: METADATA ZONE              │
                      │  Object records, forward index      │
                      │  ~1-2% of disk                      │
                      ├─────────────────────────────────────┤
                      │  Zone 3: BLOB ZONE                  │
                      │  File content, large values         │
                      │  ~95% of disk                       │
                      └─────────────────────────────────────┘
 end-4K               Superblock backup copy
```

### Zone 1: The Index Zone

This is where roaring bitmaps live. The access pattern is: read entire bitmap for a tag, AND/OR it with other bitmaps, occasionally update a single bit. This maps perfectly to a **log-structured** or **copy-on-write** design — you never update bitmaps in place, you write a new version and atomically swap the pointer.

```rust
// On-disk format for a single tag's bitmap
#[repr(C)]
struct BitmapBlock {
    tag_id: u32,
    generation: u64,       // for MVCC / crash recovery
    serialized_len: u32,   // roaring serialized size
    // followed by roaring bitmap bytes
    // (roaring's serialization is already very compact)
}

// The bitmap directory: maps tag_id → disk offset
// This is a small B+ tree (thousands of entries, not millions)
// Fits in RAM, persisted for crash recovery
```

A roaring bitmap for a tag that applies to 100K out of 10M objects is roughly **20–50 KB** serialized. The entire index zone for 5000 tags would be 100–250 MB. On a modern NVMe that's a few hundred milliseconds to read _entirely into RAM_ at boot. So the right strategy is:

**Keep the entire index zone in memory.** Persist to disk on mutation (batched, write-ahead-logged). At 10M objects × 5000 tags × 20 tags/object, the in-memory working set is 200–500 MB. This is completely reasonable — ZFS and btrfs keep more than this in their ARC/page cache.

The B+ tree for range-indexed attributes (year, size, etc.) also lives here. Same approach: small enough to cache entirely, persist via WAL + periodic checkpointing.

### Zone 2: The Metadata Zone

Each object has a small metadata record. This is the structure you look up when you've found an object ID in a bitmap and need its details:

```rust
#[repr(C)]
struct ObjectRecord {
    id: u64,
    content_hash: [u8; 32],    // BLAKE3 of blob
    blob_offset: u64,           // byte offset into blob zone
    blob_length: u64,
    created_ns: i64,
    modified_ns: i64,
    tag_count: u16,
    attr_count: u16,
    // Inline tag IDs for objects with few tags (avoid indirection)
    inline_tags: [u32; 8],      // covers 80%+ of objects
    // If tag_count > 8, overflow pointer:
    overflow_offset: u64,       // points to extended record
}
// 128 bytes, cache-line friendly
```

10M objects × 128 bytes = **1.28 GB**. This is too large to hold entirely in memory for most systems, so you want a structure that supports efficient point lookups with caching.

**Best structure: hash-indexed fixed-size slots.** Since object IDs are sequential u64s, you can use a simple array indexed by ID. This is faster than any tree:

```
Object 0:    offset 0 × 128
Object 1:    offset 1 × 128
Object N:    offset N × 128

Lookup: single disk read at (zone2_base + id * 128)
        Often a cache hit after first access
```

This is how ext4's inode table works, and it's the right choice here for the same reason — IDs are dense and sequential.

### Zone 3: The Blob Zone

This is 95%+ of the disk. Two viable strategies:

**Log-structured (like LFS / F2FS).** Appends are always sequential, great for write throughput. Needs garbage collection when objects are deleted or overwritten. Excellent for SSDs because it reduces write amplification.

```
┌──────────┬──────────┬──────────┬──────────┬────
│ Segment 0│ Segment 1│ Segment 2│ Segment 3│ ...
│  32MB    │  32MB    │  32MB    │  32MB    │
└──────────┴──────────┴──────────┴──────────┴────

Each segment:
┌────────────────────────────────────────────────┐
│ Header (generation, checksum, object count)     │
│ Blob 1 (object 42, 15KB)                       │
│ Blob 2 (object 99, 2.3MB, spans to next seg)   │
│ Blob 3 (object 107, 800B)                      │
│ Free space...                                   │
└────────────────────────────────────────────────┘
```

**Extent-based (like XFS / ext4).** Allocate contiguous runs of blocks for each blob. Better for large sequential reads (video files). Needs a free-space allocator (bitmap or B+ tree of extents).

For a general-purpose associative filesystem, **extent-based with a free-space B+ tree** is more practical. Log-structured is better if your workload is write-heavy and deletion-rare.

### Write-Ahead Log

The WAL is critical for crash consistency. All mutations go through it:

```
WAL Entry Format:
┌──────────────────────────────────────────┐
│ LSN: u64 (log sequence number)           │
│ Txn ID: u64                              │
│ Type: enum { TagAdd, TagRemove,          │
│              BlobWrite, MetaUpdate,      │
│              OntologyChange, Commit }    │
│ Payload length: u32                      │
│ Payload: [u8]                            │
│ CRC32: u32                               │
└──────────────────────────────────────────┘
```

The WAL is a fixed-size circular buffer (64 MB is plenty). On crash recovery: replay from last checkpoint, re-apply mutations. This is the same approach as PostgreSQL, SQLite, and every serious database.

**Checkpointing:** periodically flush dirty in-memory bitmaps and metadata to their zones, then advance the WAL tail. This bounds recovery time.

### Multi-Disk Layout

With multiple raw disks, you get to make interesting placement decisions that a traditional filesystem can't:

```
          Disk 0 (NVMe, fast)           Disk 1..N (HDD or SATA SSD, capacity)
    ┌────────────────────────┐     ┌─────────────────────────────┐
    │ Superblock             │     │ Superblock (mirror)         │
    │ WAL (all writes here   │     │                             │
    │      first — fast!)    │     │                             │
    │ Index Zone (bitmaps)   │     │ Blob Zone (striped across   │
    │ Metadata Zone          │     │   disks 1..N for            │
    │ Hot blob cache         │     │   throughput)               │
    └────────────────────────┘     └─────────────────────────────┘
```

The key insight: **separate the metadata path from the data path.** Index lookups and tag queries hit the NVMe. Blob reads/writes go to the capacity tier. This is exactly what ZFS's SLOG and L2ARC do, but you can do it more precisely because you know the access patterns.

For striping blobs across N disks, a simple approach: hash the object ID to pick a disk, then allocate extents on that disk. This gives you roughly N× read throughput for concurrent access to different objects.

```rust
fn disk_for_object(id: ObjectId, num_disks: u32) -> u32 {
    // Simple but effective — distributes evenly
    // Skip disk 0 (metadata disk)
    1 + (id % (num_disks as u64 - 1)) as u32
}
```

For redundancy, you'd mirror the superblock and WAL to all disks, and optionally store blob parity (RAID5-style) or mirrors (RAID1-style) across the capacity disks. But that's a separable concern — get single-disk working first.

### Putting It All Together: The I/O Path

A query like "find all objects tagged `electronics` AND `portable`, return their names and sizes":

```
1. In-memory bitmap lookup:              ~1 μs
   electronics_bitmap AND portable_bitmap
   → RoaringBitmap with matching IDs

2. For each match, read ObjectRecord:     ~4 μs per (cached), ~50 μs (disk)
   seek to zone2_base + id * 128, read 128 bytes

3. Total for 1000 matches:                ~1-5 ms (mostly cached)
```

Opening a file (reading blob content):

```
1. ObjectRecord gives blob_offset + blob_length
2. Single sequential read from blob zone
3. For 1MB file on NVMe: ~200 μs
   For 1MB file on HDD:  ~10 ms
```

Tagging an object:

```
1. WAL append: ~2 μs (NVMe)
2. Update in-memory bitmap: ~0.1 μs (set bit in roaring)
3. Update in-memory forward index: ~0.1 μs
4. Background: checkpoint dirty bitmaps to disk
```

The main thing to notice: the metadata path never touches the blob zone, and the blob path is a single contiguous read. No tree traversal, no directory walking, no indirection chains. This is fundamentally faster than any hierarchical filesystem can be for associative queries.

# replication and multidisks

This is where the associative filesystem gets a genuine structural advantage over ZFS. ZFS treats all blocks as opaque — it doesn't know that blocks 5000–5500 are a video file tagged `archive` that hasn't been accessed in two years. Your filesystem _does_ know this, and that knowledge transforms how you handle topology changes.

## What ZFS Does (and Where It's Blind)

ZFS's storage model has a clean abstraction stack:

```
pool → vdev → disk

Pool: logical aggregation, all space is fungible
Vdev: redundancy unit (mirror, raidz, single)
Disk: physical device
```

When you add a vdev, new writes go there but **existing data doesn't move**. The pool gradually rebalances as old data is overwritten. If you remove a vdev (only supported for mirrors and special vdevs), ZFS copies every block off it — sequentially scanning the entire metadata tree to find which blocks live on that vdev.

The problem: ZFS doesn't know _what_ it's moving. It can't say "move all the cold archive data to the slow disk first, keep the hot query indexes on NVMe." It just sees blocks.

## The Associative FS Advantage: Semantic Placement

Your filesystem knows three things ZFS doesn't:

1. **What each object is** — its tags, attributes, access pattern
2. **What's related to what** — objects sharing tags are likely accessed together
3. **What the user cares about** — the ontology encodes importance/category

This lets you do **policy-driven placement** instead of blind block allocation.

### The Indirection Layer

First, you need a level of indirection that decouples logical object identity from physical location. Without this, moving data means updating every reference — a nightmare.

```rust
// Object location is always resolved through this table
// Lives in the metadata zone on the fast disk, fully cached in RAM
struct ObjectLocation {
    disk_id: u16,        // which physical disk
    extent_offset: u64,  // byte offset on that disk's blob zone  
    extent_length: u64,  // contiguous bytes
    replica_count: u8,   // how many copies exist
    replicas: [ReplicaRef; 3],  // where the copies are
}

struct ReplicaRef {
    disk_id: u16,
    extent_offset: u64,
}

// Moving an object = copy bytes to new location + update this record
// Everything else (bitmaps, tags, IDs) stays untouched
```

This is similar to ZFS's block pointer table, but at object granularity rather than block granularity. The difference matters: you have millions of entries, not billions. The entire location table for 10M objects at 64 bytes each is 640 MB — fits in RAM, trivially fast to scan.

### Disk Topology Model

```rust
struct Pool {
    id: PoolId,
    disks: Vec<DiskDescriptor>,
    placement_rules: Vec<PlacementRule>,
    redundancy_policy: RedundancyPolicy,
}

struct DiskDescriptor {
    id: DiskId,
    path: String,             // /dev/nvme0n1, /dev/sda, etc.
    capacity: u64,
    used: u64,
    media_type: MediaType,    // NVMe, SSD, HDD, SMR_HDD
    tier: StorageTier,        // derived from media + user config
    health: DiskHealth,
    // Performance characteristics (measured at init)
    seq_read_mbps: u32,
    seq_write_mbps: u32,  
    random_iops: u32,
    latency_us: u32,
}

enum StorageTier {
    Hot,        // NVMe — indexes, metadata, frequently accessed blobs
    Warm,       // SATA SSD — general purpose blobs
    Cold,       // HDD — archive, bulk storage
    Glacier,    // SMR HDD — write-once archival
}
```

### Placement Rules: Tags Drive Disk Selection

This is the key differentiator. Placement rules bind _semantic properties_ to _physical topology_:

```rust
enum PlacementRule {
    // Objects matching this query prefer this tier
    Prefer {
        query: Query,           // e.g. HasTag("active") AND HasTag("project")
        tier: StorageTier,
        priority: u8,
    },
    
    // Objects matching this query MUST be on this tier
    Pin {
        query: Query,           // e.g. HasTag("scratch") OR HasTag("cache")
        tier: StorageTier,
    },
    
    // Objects matching this query need N replicas
    Replicate {
        query: Query,           // e.g. HasTag("critical")
        min_replicas: u8,
        across_disks: bool,     // replicas must be on different disks
    },
    
    // Objects matching this query should be colocated
    // (placed on same disk extents for sequential read perf)
    Colocate {
        query: Query,           // e.g. HasAttr("album", "vacation-2024")
    },
    
    // Tag-based tiering with aging
    AutoTier {
        hot_threshold_days: u32,   // accessed within N days → hot tier
        warm_threshold_days: u32,  // → warm tier  
        cold_after: u32,           // → cold tier
    },
}
```

Example configuration:

```rust
pool.add_rule(PlacementRule::Pin {
    query: HasTag(tag("scratch")),
    tier: StorageTier::Hot,        // scratch data on NVMe only
});

pool.add_rule(PlacementRule::Replicate {
    query: HasTag(tag("critical")),
    min_replicas: 3,
    across_disks: true,
});

pool.add_rule(PlacementRule::Colocate {
    query: HasAttr { key: tag("project"), op: Eq, value: Text("vesper") },
    // All files in project "vesper" placed contiguously
});

pool.add_rule(PlacementRule::Prefer {
    query: HasTag(tag("archive")),
    tier: StorageTier::Cold,       // archives migrate to HDD
    priority: 10,
});
```

ZFS has nothing like this. You can set `primarycache`, `secondarycache`, and `special_small_blocks` per dataset, but a dataset is a rigid hierarchy. Here, the same object can match multiple rules, and the placement engine resolves conflicts by priority.

## Disk Addition

When a new disk joins the pool:

```
BEFORE:                          AFTER:
┌──────┐ ┌──────┐               ┌──────┐ ┌──────┐ ┌──────┐
│Disk 0│ │Disk 1│               │Disk 0│ │Disk 1│ │Disk 2│
│NVMe  │ │SSD   │               │NVMe  │ │SSD   │ │HDD   │
│ 80%  │ │ 90%  │               │ 80%  │ │ 90%  │ │  0%  │
│ full │ │ full │               │ full │ │ full │ │ empty│
└──────┘ └──────┘               └──────┘ └──────┘ └──────┘
```

ZFS approach: new writes go to the new disk. Old data stays put. Eventually balances over months/years of churn.

**Associative FS approach: semantic rebalance.** You know what should move:

```rust
fn plan_rebalance_on_add(pool: &Pool, new_disk: &DiskDescriptor) -> MigrationPlan {
    let mut plan = MigrationPlan::new();
    let new_tier = new_disk.tier;
    
    // Phase 1: Objects that SHOULD be on this tier but aren't
    // (placement rules were unsatisfiable before this disk existed)
    for rule in &pool.placement_rules {
        if let PlacementRule::Prefer { query, tier, .. } 
             | PlacementRule::Pin { query, tier } = rule 
        {
            if *tier == new_tier {
                let matching = execute_query(query);
                for obj_id in matching.iter() {
                    let loc = get_location(obj_id);
                    if pool.disk(loc.disk_id).tier != new_tier {
                        plan.add_move(obj_id, new_disk.id, Priority::High);
                    }
                }
            }
        }
    }
    
    // Phase 2: Rebalance for space — move objects from fullest disk
    // Prefer moving objects that match the new disk's tier
    let fullest = pool.disks.iter()
        .max_by_key(|d| d.used * 100 / d.capacity)
        .unwrap();
    
    if fullest.utilization() > 0.80 {
        let candidates = objects_on_disk(fullest.id);
        // Sort by: best tier match first, then coldest first
        let ranked = rank_for_migration(candidates, new_disk.tier);
        
        let target_bytes = fullest.used - (fullest.capacity * 70 / 100);
        plan.add_moves_until(ranked, new_disk.id, target_bytes);
    }
    
    plan
}
```

The migration itself is background I/O, rate-limited to avoid impacting foreground queries:

```
Migration Pipeline:
                                    
  ┌─────────┐    ┌──────────┐    ┌────────────┐    ┌──────────┐
  │ Scan    │───▶│ Read blob│───▶│ Write to   │───▶│ Update   │
  │ objects │    │ from src │    │ new disk   │    │ location │
  │ to move │    │ disk     │    │ + verify   │    │ table    │
  └─────────┘    └──────────┘    └────────────┘    └──────────┘
                                       │                │
                                       │   Only after   │
                                       │   verify OK:   │
                                       ▼                ▼
                                 ┌────────────┐   ┌──────────┐
                                 │ Free old   │   │ WAL      │
                                 │ extent on  │   │ commit   │
                                 │ src disk   │   │          │
                                 └────────────┘   └──────────┘
```

**Crash safety:** the location table update and old extent free are in the same WAL transaction. If you crash mid-migration, the object still points to the old location (which hasn't been freed). The new copy is orphaned and reclaimed on recovery.

## Disk Removal

This is where ZFS struggles most. `zpool remove` only works for certain vdev types, and it's a full sequential scan of the pool's metadata tree.

Your filesystem has it much easier because the location table is a flat array:

```rust
fn plan_removal(pool: &Pool, removing: DiskId) -> MigrationPlan {
    let mut plan = MigrationPlan::new();
    
    // Scan location table — O(n) but it's a sequential scan of
    // a contiguous in-memory array, so 10M entries in ~50ms
    for obj_id in 0..max_object_id {
        let loc = get_location(obj_id);
        
        if loc.disk_id == removing {
            // Must move. Where?
            let dest = select_destination(pool, obj_id, removing);
            plan.add_move(obj_id, dest, Priority::Critical);
        }
        
        // Also check replicas
        for replica in &loc.replicas {
            if replica.disk_id == removing {
                let dest = select_destination(pool, obj_id, removing);
                plan.add_re_replicate(obj_id, dest);
            }
        }
    }
    
    plan
}

fn select_destination(pool: &Pool, obj_id: ObjectId, excluding: DiskId) -> DiskId {
    // 1. Check placement rules for this object
    let tags = get_tags(obj_id);
    let preferred_tier = evaluate_placement_rules(pool, &tags);
    
    // 2. Find disk matching preferred tier with most free space
    pool.disks.iter()
        .filter(|d| d.id != excluding && d.health.is_healthy())
        .filter(|d| d.tier == preferred_tier || preferred_tier.is_none())
        .max_by_key(|d| d.capacity - d.used)
        .map(|d| d.id)
        .unwrap_or_else(|| {
            // No tier match — just pick the emptiest disk
            pool.emptiest_disk(excluding)
        })
}
```

Notice what happened: disk removal is **tag-aware**. When you pull out an HDD, the archive-tagged objects try to land on another HDD (or cold tier). The active project files try to land on SSD. ZFS would scatter them randomly.

### Degraded Mode

While removal is in progress, the disk is in `Draining` state:

```rust
enum DiskState {
    Online,
    Draining,    // removal in progress, reads OK, no new writes
    Faulted,     // I/O errors, reads may fail
    Offline,     // administratively removed, no I/O
}
```

During draining, reads still go to the old disk (faster than waiting for migration). New writes go elsewhere. The pool remains fully operational throughout.

## Resilver and Redundancy

ZFS resilver walks every block pointer in the pool to find blocks on the failed/replaced disk. This is slow — proportional to total pool metadata size, not to how much data was on the failed disk.

Your approach: scan the location table. You know _immediately_ which objects are affected:

```rust
fn resilver(pool: &Pool, failed: DiskId, replacement: DiskId) {
    // Instant: find all affected objects
    let affected: Vec<ObjectId> = (0..max_object_id)
        .filter(|&id| {
            let loc = get_location(id);
            loc.disk_id == failed || loc.replicas.iter().any(|r| r.disk_id == failed)
        })
        .collect();
    
    println!("Resilvering {} objects", affected.len());
    // Compare with ZFS: "Resilvering... ETA: 47 hours" (scanning everything)
    
    // Prioritize by redundancy level — objects with no remaining
    // replicas are most urgent
    affected.sort_by_key(|&id| {
        let loc = get_location(id);
        let surviving_replicas = loc.replicas.iter()
            .filter(|r| r.disk_id != failed)
            .count();
        surviving_replicas  // 0 = most urgent
    });
    
    for obj_id in affected {
        copy_from_surviving_replica(obj_id, replacement);
        update_location_table(obj_id, failed, replacement);
    }
}
```

ZFS resilver time: proportional to **pool size**. Your resilver time: proportional to **data on the failed disk**. For a pool where the failed disk holds 5% of the data, that's a 20× speedup in recovery.

## Smart Tiering Based on Tags

This is the feature that has no ZFS equivalent at all. Objects automatically move between tiers based on their tags and access patterns:

```rust
struct TieringEngine {
    access_log: RingBuffer<AccessEvent>,  // recent access timestamps per object
}

impl TieringEngine {
    fn periodic_tier_review(&self, pool: &mut Pool) -> MigrationPlan {
        let mut plan = MigrationPlan::new();
        
        for obj_id in 0..max_object_id {
            let loc = get_location(obj_id);
            let current_tier = pool.disk(loc.disk_id).tier;
            let ideal_tier = self.compute_ideal_tier(pool, obj_id);
            
            if current_tier != ideal_tier {
                let dest = pool.best_disk_in_tier(ideal_tier);
                plan.add_move(obj_id, dest, Priority::Background);
            }
        }
        
        plan
    }
    
    fn compute_ideal_tier(&self, pool: &Pool, obj_id: ObjectId) -> StorageTier {
        let tags = get_tags(obj_id);
        
        // Explicit rules take precedence
        if let Some(tier) = evaluate_pin_rules(pool, &tags) {
            return tier;
        }
        
        // Then preference rules
        if let Some(tier) = evaluate_prefer_rules(pool, &tags) {
            // But only if the object's access pattern agrees
            let last_access = self.last_access(obj_id);
            let age_days = now() - last_access;
            
            match tier {
                StorageTier::Hot if age_days > 30 => {
                    // Rule says hot, but object is cold — demote
                    return StorageTier::Warm;
                }
                _ => return tier,
            }
        }
        
        // Default: access-time based
        let last_access = self.last_access(obj_id);
        let age_days = (now() - last_access).as_days();
        
        match age_days {
            0..=7 => StorageTier::Hot,
            8..=90 => StorageTier::Warm,
            _ => StorageTier::Cold,
        }
    }
}
```

The result is something like ZFS + intelligent HSM (hierarchical storage management), but driven by your tag ontology rather than by path prefixes or manual policies.

## The Layout on Multiple Disks

```
 NVMe (Disk 0)          SSD (Disk 1)           HDD (Disk 2)
 ═══════════════         ═══════════════         ═══════════════
 ┌─────────────┐         ┌─────────────┐         ┌─────────────┐
 │ Superblock  │         │ Superblock  │         │ Superblock  │
 │ (primary)   │         │ (mirror)    │         │ (mirror)    │
 ├─────────────┤         ├─────────────┤         ├─────────────┤
 │ WAL         │         │ WAL mirror  │         │             │
 │ (64 MB)     │         │ (64 MB)     │         │             │
 ├─────────────┤         ├─────────────┤         │             │
 │ Location    │         │             │         │             │
 │ Table       │         │             │         │             │
 ├─────────────┤         │             │         │             │
 │ Index Zone  │         │             │         │             │
 │ (bitmaps)   │         │             │         │             │
 ├─────────────┤         │             │         │             │
 │ Metadata    │         │             │         │             │
 │ Zone        │         │             │         │             │
 ├─────────────┤         ├─────────────┤         ├─────────────┤
 │ Hot blobs   │         │ Warm blobs  │         │ Cold blobs  │
 │ (active     │         │ (general    │         │ (archive,   │
 │  projects,  │         │  storage)   │         │  old data)  │
 │  scratch)   │         │             │         │             │
 └─────────────┘         └─────────────┘         └─────────────┘
   ^                       ^                       ^
   │                       │                       │
   tag:"active"            default                 tag:"archive"
   tag:"scratch"           placement               tag:"backup"
   (placement rules)                               (placement rules)
```

## Summary: What You Get Beyond ZFS

| Operation             | ZFS                           | Associative FS                                                  |
| --------------------- | ----------------------------- | --------------------------------------------------------------- |
| Add disk              | New writes only, no rebalance | Semantic rebalance — archives move to HDD, active to SSD        |
| Remove disk           | Full pool metadata scan       | Location table scan (10M objects in ~50ms)                      |
| Resilver              | Proportional to pool size     | Proportional to affected data only                              |
| Placement             | Per-dataset, path-based       | Per-object, tag-query-based                                     |
| Tiering               | Manual (special vdev only)    | Automatic, ontology-driven                                      |
| Colocate related data | Hope for the best             | Explicit rule: `Colocate { query }`                             |
| Redundancy            | Per-vdev, uniform             | Per-object, tag-driven (critical files get 3×, scratch gets 0×) |

# diagram

```mehrmaid
graph TB
%% ═══════════════════════════════════════════
%% CLIENT / QUERY LAYER
%% ═══════════════════════════════════════════

subgraph ClientLayer["Client Layer"]
    CLI["CLI / Shell<br/><i>query DSL, tag ops,<br/>browse, import</i>"]
    API["Library API<br/><i>Rust crate interface</i>"]
    FUSE["Optional FUSE<br/><i>POSIX compat shim</i>"]
end

%% ═══════════════════════════════════════════
%% QUERY ENGINE
%% ═══════════════════════════════════════════

subgraph QueryEngine["Query Engine"]
    Parser["Query Parser<br/><i>text → Query AST</i>"]
    Planner["Query Planner<br/><i>reorder, optimize,<br/>choose index</i>"]
    Executor["Set Executor<br/><i>bitmap AND/OR/NOT,<br/>range scans</i>"]
    FacetCounter["Facet Counter<br/><i>co-occurrence,<br/>refinement tags</i>"]
end

CLI --> Parser
API --> Parser
FUSE --> Parser
Parser --> Planner
Planner --> Executor
Executor --> FacetCounter

%% ═══════════════════════════════════════════
%% DATA MODEL
%% ═══════════════════════════════════════════

subgraph DataModel["Data Model"]
    direction TB
    Objects["Objects<br/><i>ObjectId: u64<br/>content hash, size,<br/>timestamps</i>"]
    Assertions["Assertions<br/><i>Tag(TagId)<br/>Attr(key, value)<br/>Relation(pred, target)</i>"]
    Values["Values<br/><i>Text, Int, Float,<br/>Timestamp, Blob</i>"]
    Objects --- Assertions
    Assertions --- Values
end

%% ═══════════════════════════════════════════
%% ONTOLOGY
%% ═══════════════════════════════════════════

subgraph Ontology["Ontology Layer"]
    ImplGraph["Implication DAG<br/><i>car → vehicle → physical<br/>transitive closure</i>"]
    Aliases["Aliases<br/><i>laptop = notebook</i>"]
    MutexGroups["Mutex Groups<br/><i>active ⊕ discontinued</i>"]
    Materializer["Materializer<br/><i>on tag add: insert<br/>all implied tags</i>"]
end

ImplGraph --> Materializer
Aliases --> Planner
MutexGroups --> Materializer

%% ═══════════════════════════════════════════
%% INDEX LAYER (in-memory, persisted)
%% ═══════════════════════════════════════════

subgraph IndexLayer["Index Layer (RAM-resident, WAL-persisted)"]
    TagIndex["Tag Inverted Index<br/><i>TagId → RoaringBitmap<br/>5000 tags × ~20-50KB each</i>"]
    KVIndex["KV Equality Index<br/><i>(key, value_hash) → RoaringBitmap<br/>compound tag approach</i>"]
    RangeIndex["Range B+ Tree<br/><i>(attr_id, value, obj_id)<br/>ordered scans for &lt; &gt; BETWEEN</i>"]
    ForwardIndex["Forward Index<br/><i>ObjectId → Vec&lt;TagId&gt;<br/>for getattr / tag listing</i>"]
end

Executor --> TagIndex
Executor --> KVIndex
Executor --> RangeIndex
Materializer --> TagIndex
Materializer --> ForwardIndex

%% ═══════════════════════════════════════════
%% METADATA LAYER
%% ═══════════════════════════════════════════

subgraph MetadataLayer["Metadata Layer"]
    ObjTable["Object Table<br/><i>ObjectId → ObjectRecord<br/>128B fixed slots<br/>array indexed by ID</i>"]
    LocTable["Location Table<br/><i>ObjectId → ObjectLocation<br/>disk_id, extent_offset,<br/>replica refs</i>"]
end

Executor --> ObjTable
ObjTable --> LocTable

%% ═══════════════════════════════════════════
%% PLACEMENT & TIERING ENGINE
%% ═══════════════════════════════════════════

subgraph PlacementEngine["Placement & Tiering Engine"]
    Rules["Placement Rules<br/><i>Pin, Prefer, Replicate,<br/>Colocate, AutoTier</i>"]
    TierCalc["Tier Calculator<br/><i>tags + access time<br/>→ ideal tier</i>"]
    Migrator["Migration Planner<br/><i>background moves,<br/>rate-limited I/O</i>"]
    Allocator["Extent Allocator<br/><i>per-disk free space<br/>B+ tree of extents</i>"]
end

Rules --> TierCalc
TierCalc --> Migrator
Migrator --> Allocator
TagIndex -.->|"query matching<br/>objects"| TierCalc
LocTable <-->|"read current /<br/>update after move"| Migrator

%% ═══════════════════════════════════════════
%% TRANSACTION / WAL
%% ═══════════════════════════════════════════

subgraph TxnLayer["Transaction Layer"]
    WAL["Write-Ahead Log<br/><i>circular 64MB buffer<br/>on fastest disk<br/>+ mirror</i>"]
    Checkpoint["Checkpoint Engine<br/><i>flush dirty bitmaps<br/>+ metadata to zones</i>"]
    Recovery["Crash Recovery<br/><i>replay WAL from<br/>last checkpoint</i>"]
end

TagIndex -->|"mutations"| WAL
ForwardIndex -->|"mutations"| WAL
ObjTable -->|"mutations"| WAL
LocTable -->|"mutations"| WAL
WAL --> Checkpoint
Recovery --> WAL

%% ═══════════════════════════════════════════
%% POOL MANAGER
%% ═══════════════════════════════════════════

subgraph PoolManager["Pool Manager"]
    Topology["Topology Map<br/><i>DiskDescriptor[]<br/>media type, tier,<br/>capacity, health</i>"]
    Rebalancer["Rebalancer<br/><i>on disk add/remove:<br/>semantic migration</i>"]
    Resilverer["Resilverer<br/><i>on disk fail/replace:<br/>location table scan<br/>priority by replica count</i>"]
    HealthMon["Health Monitor<br/><i>SMART, I/O errors,<br/>latency tracking</i>"]
end

Topology --> Allocator
Topology --> TierCalc
Rebalancer --> Migrator
Resilverer --> Migrator
HealthMon --> Topology
HealthMon --> Resilverer

%% ═══════════════════════════════════════════
%% PHYSICAL DISK LAYER
%% ═══════════════════════════════════════════

subgraph DiskLayer["Physical Disk Layer (Raw Block Devices)"]

    subgraph Disk0["Disk 0 — NVMe (Hot Tier)"]
        D0Super["Superblock"]
        D0WAL["WAL (primary)"]
        D0Index["Index Zone<br/><i>tag bitmaps,<br/>B+ trees</i>"]
        D0Meta["Metadata Zone<br/><i>object table,<br/>location table</i>"]
        D0Blobs["Hot Blobs<br/><i>tag:active,<br/>tag:scratch</i>"]
    end

    subgraph Disk1["Disk 1 — SATA SSD (Warm Tier)"]
        D1Super["Superblock<br/>(mirror)"]
        D1WAL["WAL (mirror)"]
        D1Blobs["Warm Blobs<br/><i>default placement</i>"]
    end

    subgraph Disk2["Disk 2 — HDD (Cold Tier)"]
        D2Super["Superblock<br/>(mirror)"]
        D2Blobs["Cold Blobs<br/><i>tag:archive,<br/>tag:backup</i>"]
    end

    subgraph DiskN["Disk N — (any tier)"]
        DNSuper["Superblock<br/>(mirror)"]
        DNBlobs["Blobs<br/><i>rule-driven<br/>placement</i>"]
    end

end

WAL -->|"primary"| D0WAL
WAL -->|"mirror"| D1WAL
Checkpoint --> D0Index
Checkpoint --> D0Meta
Allocator --> D0Blobs
Allocator --> D1Blobs
Allocator --> D2Blobs
Allocator --> DNBlobs

%% ═══════════════════════════════════════════
%% BLOB I/O PATH
%% ═══════════════════════════════════════════

subgraph BlobIO["Blob I/O Path"]
    BlobRead["Read Path<br/><i>LocTable → disk_id +<br/>offset → direct I/O</i>"]
    BlobWrite["Write Path<br/><i>allocate extent →<br/>write → update loc<br/>→ WAL commit</i>"]
    ContentAddr["Content Addressing<br/><i>BLAKE3 hash<br/>dedup optional</i>"]
end

LocTable --> BlobRead
BlobRead --> DiskLayer
BlobWrite --> Allocator
BlobWrite --> WAL
ContentAddr --> BlobWrite

%% ═══════════════════════════════════════════
%% STYLING
%% ═══════════════════════════════════════════

classDef client fill:#e8f4f8,stroke:#2980b9,stroke-width:2px,color:#000
classDef query fill:#fef9e7,stroke:#f39c12,stroke-width:2px,color:#000
classDef data fill:#fdf2e9,stroke:#e67e22,stroke-width:2px,color:#000
classDef ontology fill:#f4ecf7,stroke:#8e44ad,stroke-width:2px,color:#000
classDef index fill:#eafaf1,stroke:#27ae60,stroke-width:2px,color:#000
classDef meta fill:#fdebd0,stroke:#d35400,stroke-width:2px,color:#000
classDef placement fill:#ebf5fb,stroke:#2e86c1,stroke-width:2px,color:#000
classDef txn fill:#fdedec,stroke:#e74c3c,stroke-width:2px,color:#000
classDef pool fill:#e8daef,stroke:#7d3c98,stroke-width:2px,color:#000
classDef disk fill:#d5dbdb,stroke:#566573,stroke-width:2px,color:#000
classDef blob fill:#d6eaf8,stroke:#2874a6,stroke-width:2px,color:#000

class CLI,API,FUSE client
class Parser,Planner,Executor,FacetCounter query
class Objects,Assertions,Values data
class ImplGraph,Aliases,MutexGroups,Materializer ontology
class TagIndex,KVIndex,RangeIndex,ForwardIndex index
class ObjTable,LocTable meta
class Rules,TierCalc,Migrator,Allocator placement
class WAL,Checkpoint,Recovery txn
class Topology,Rebalancer,Resilverer,HealthMon pool
class D0Super,D0WAL,D0Index,D0Meta,D0Blobs,D1Super,D1WAL,D1Blobs,D2Super,D2Blobs,DNSuper,DNBlobs disk
class BlobRead,BlobWrite,ContentAddr blob
```

# Ontology (contd. re: playlists and ordering)

You're absolutely right. I was overcomplicating it by treating collections as a storage-layer concern. The _ontology_ knows what a playlist is. The storage layer just needs to support what the ontology describes.

## The Insight

A playlist isn't a different kind of object. It's a tag with ontological properties:

```
The ontology says:
  "playlist" is-a "ordered-collection"
  "album"    is-a "ordered-collection"
  "genre"    is-a "unordered-grouping"
  "artist"   is-a "unordered-grouping"

The storage layer doesn't know about music.
It knows about tags, and that some tags have
the ontological property "ordered."
```

So the question becomes: how does the ontology inform the storage layer that certain tags carry ordering?

## Ontology-Driven Tag Properties

Tags aren't just labels. They have properties defined by the ontology:

```rust
struct TagDefinition {
    id: TagId,
    name: String,
    
    // What kind of tag is this? Defined by ontology.
    semantics: TagSemantics,
    
    // Implication chain
    implies: Vec<TagId>,        // playlist implies "collection", "music-related"
}

enum TagSemantics {
    // Simple label: "electronic", "favorite", "needs-review"
    Label,
    
    // Key-value: "artist=Aphex Twin", "year=1997"  
    Attribute { value_type: ValueType },
    
    // Unordered grouping: "genre:ambient" — just a label really,
    // but ontology says "genre" groups things
    Grouping,
    
    // Ordered collection: "playlist:workout", "album:SAW-II"
    // The storage layer MUST maintain insertion order
    OrderedCollection { 
        element_constraint: Option<TagId>,  // members must have this tag
    },
    
    // Hierarchical: "location:europe/france/paris"
    // Ontology defines parent-child, materializer expands
    Hierarchical,
}
```

The key move: **`OrderedCollection` is a property of the tag, not of the objects.** The storage layer sees that tag `playlist:workout` has `OrderedCollection` semantics and knows to maintain a sequence alongside the bitmap.

```
                    ONTOLOGY DEFINES
                    ════════════════

   "playlist"  ──is-a──▶  "ordered-collection"
                              │
   "album"     ──is-a──▶─────┘
                              │
                              ▼
                    storage layer sees this
                    and maintains:
                    
                    1. RoaringBitmap  (membership, fast query)
                    2. Vec<ObjectId>  (ordering, sequential)
```

## Revised Storage: Tag Has Data

Instead of a separate relation index, the tag index itself becomes richer. Most tags are simple bitmaps. Some tags carry additional structure because the ontology says they should:

```rust
enum TagStore {
    // 99% of tags: just a bitmap
    Simple(RoaringBitmap),
    
    // Ordered collections: bitmap + sequence
    // Bitmap for fast "is X in this playlist?" 
    // Sequence for "give me tracks in order"
    Ordered {
        members: RoaringBitmap,
        sequence: Vec<ObjectId>,
    },
    
    // Future: weighted, scored, ranked collections
    // "top-10 favorite" where position matters and changes
    Ranked {
        members: RoaringBitmap,
        ranked: Vec<(ObjectId, f32)>,  // (id, score)
    },
}
```

Now look at how clean the operations become:

```rust
// Adding track 42 to playlist "workout"
fn tag_object(store: &mut TagStore, obj_id: ObjectId) {
    match store {
        TagStore::Simple(bitmap) => {
            bitmap.insert(obj_id as u32);
        }
        TagStore::Ordered { members, sequence } => {
            members.insert(obj_id as u32);
            sequence.push(obj_id);  // appends to end
        }
        TagStore::Ranked { members, ranked } => {
            members.insert(obj_id as u32);
            ranked.push((obj_id, 0.0));  // default score
        }
    }
}

// Query: "all tracks in workout playlist" — same bitmap lookup regardless
fn query_members(store: &TagStore) -> &RoaringBitmap {
    match store {
        TagStore::Simple(b) => b,
        TagStore::Ordered { members, .. } => members,
        TagStore::Ranked { members, .. } => members,
    }
}

// Query: "tracks in workout, in order" — only works on ordered tags
fn query_ordered(store: &TagStore) -> Option<&[ObjectId]> {
    match store {
        TagStore::Ordered { sequence, .. } => Some(sequence),
        _ => None,
    }
}
```

## How It Looks In Practice

```
Objects in the system:
  obj:42  → tags: [music, electronic, artist="Aphex Twin", year=1997]
  obj:43  → tags: [music, electronic, artist="Autechre", year=1998]  
  obj:44  → tags: [music, ambient, artist="Brian Eno", year=1978]
  obj:55  → tags: [music, rock, artist="Radiohead", year=2000]

Ontology says:
  "playlist:*"  is-a ordered-collection  (of things tagged "music")
  "album:*"     is-a ordered-collection  (of things tagged "music")
  "genre:*"     is-a grouping
  "artist:*"    is-a grouping

Tag stores:
  music          → Simple(bitmap { 42, 43, 44, 55 })
  electronic     → Simple(bitmap { 42, 43 })
  ambient        → Simple(bitmap { 44 })
  
  playlist:workout → Ordered {
      members:  bitmap { 42, 55 },
      sequence: [55, 42],         ← Radiohead first, then Aphex Twin
  }
  
  playlist:sleep → Ordered {
      members:  bitmap { 44, 42 },
      sequence: [44, 42],         ← Eno first, then Aphex Twin
  }
  
  album:SAW-II → Ordered {
      members:  bitmap { 42, 101, 102, 103, ... },
      sequence: [101, 42, 102, 103, ...],  ← actual album track order
  }
```

Queries work uniformly:

```
"electronic music"
  = bitmap(music) AND bitmap(electronic)
  → { 42, 43 }                                 // unordered, normal

"workout playlist contents"  
  = query_ordered(playlist:workout)
  → [55, 42]                                   // ordered!

"tracks that are in BOTH workout and sleep playlists"
  = bitmap(playlist:workout) AND bitmap(playlist:sleep)
  → { 42 }                                     // set intersection, normal

"all playlists containing track 42"
  = scan tags where semantics=OrderedCollection
    AND bitmap.contains(42)
  → [playlist:workout, playlist:sleep]

"all collections containing track 42"  (playlists AND albums)
  = scan tags where ontology implies "ordered-collection"
    AND bitmap.contains(42)
  → [playlist:workout, playlist:sleep, album:SAW-II]
```

That last query is where the ontology really shines — the query doesn't name specific collection types, it queries a _concept_, and the ontology resolves which tags match.

## Ordering Operations

Ordered collections need a few more operations than simple tags:

```rust
impl TagStore {
    // Insert at specific position (playlist reordering)
    fn insert_at(&mut self, obj_id: ObjectId, position: usize) {
        if let TagStore::Ordered { members, sequence } = self {
            members.insert(obj_id as u32);
            sequence.insert(position.min(sequence.len()), obj_id);
        }
    }
    
    // Move within sequence (drag-and-drop reorder)
    fn move_to(&mut self, obj_id: ObjectId, new_position: usize) {
        if let TagStore::Ordered { sequence, .. } = self {
            if let Some(old_pos) = sequence.iter().position(|&id| id == obj_id) {
                sequence.remove(old_pos);
                sequence.insert(new_position.min(sequence.len()), obj_id);
            }
        }
    }
    
    // Remove preserves order of remaining elements
    fn remove(&mut self, obj_id: ObjectId) {
        match self {
            TagStore::Simple(bitmap) => {
                bitmap.remove(obj_id as u32);
            }
            TagStore::Ordered { members, sequence } => {
                members.remove(obj_id as u32);
                sequence.retain(|&id| id != obj_id);
            }
            TagStore::Ranked { members, ranked } => {
                members.remove(obj_id as u32);
                ranked.retain(|(id, _)| *id != obj_id);
            }
        }
    }
}
```

## On-Disk Layout Change

The index zone now has variable-width entries for ordered tags:

```
Tag Index Entry (simple):
┌──────────────┬───────────────────────────┐
│ tag_id: u32  │ roaring bitmap bytes      │
│ type: u8 = 0 │ (20-50 KB typical)        │
└──────────────┴───────────────────────────┘

Tag Index Entry (ordered collection):
┌──────────────┬───────────────────────────┬──────────────────────┐
│ tag_id: u32  │ roaring bitmap bytes      │ sequence: [u64; N]   │
│ type: u8 = 1 │ (membership, same as      │ (8 bytes per member, │
│ count: u32   │  simple — for queries)    │  ordered)            │
└──────────────┴───────────────────────────┴──────────────────────┘
```

A playlist with 500 tracks costs 500 × 8 = 4 KB for the sequence, plus maybe 2 KB for the bitmap. Negligible. Even 10,000 playlists with 500 tracks each is only 40 MB of sequence data.

## The Ontology Becomes the Schema

This is the deeper point you're making. The system doesn't have hard-coded "collection types" or "relation indexes." The ontology _is_ the schema:

```rust
// Ontology rules drive storage behavior

struct OntologyRule {
    // Tags matching this pattern...
    pattern: TagPattern,
    // ...get these storage semantics
    semantics: TagSemantics,
    // ...and these constraints
    constraints: Vec<Constraint>,
}

enum TagPattern {
    Exact(TagId),
    Prefix(String),          // "playlist:*"
    ImpliedBy(TagId),        // anything that is-a "collection"
}

enum Constraint {
    // Members of "album:*" must be tagged "music"
    MembersMustHave(TagId),
    // "playlist:*" can't contain other playlists (no nesting)
    MembersMustNotHave(TagId),
    // "album:*" members must have same "artist" value (optional, soft)
    MembersShouldShareAttr(TagId),
}
```

Adding a new collection type (say, "photo album" or "project folder") is just adding ontology rules — no storage schema changes, no new index types, no code changes. The storage layer sees `OrderedCollection` and does the right thing.

```
New ontology rule:
  "recipe-book:*" is-a ordered-collection
  constraint: members must have tag "recipe"

That's it. The system now supports recipe books with ordering,
membership queries, cross-collection search — everything
playlists get — with zero code changes.
```

This is the real payoff of building the ontology as a first-class layer rather than hardcoding structure.

# Device cluster sync

This is where the design gets its biggest structural advantage. Because metadata is cleanly separated from blob content, and because roaring bitmaps have excellent properties for delta encoding, you can build a cluster where **every node can search everything** while **blobs only exist where needed**.

## The Core Principle

```
METADATA: small, replicated everywhere, always available
CONTENT:  large, lives where placed, fetched on demand

                Node A (laptop)           Node B (NAS)            Node C (S3)
              ┌─────────────────┐      ┌─────────────────┐      ┌──────────────┐
 Metadata     │ ████████████████│      │ ████████████████│      │ █████████████│
 (tags,       │ ALL objects     │      │ ALL objects     │      │ ALL objects  │
  bitmaps,    │ ALL tags        │      │ ALL tags        │      │ ALL tags     │
  ontology)   │ ~500 MB         │      │ ~500 MB         │      │ ~500 MB      │
              ├─────────────────┤      ├─────────────────┤      ├──────────────┤
 Content      │ ░░░░░ 30%      │      │ ██████████ 80%  │      │ █████████ 95%│
 (blobs)      │ hot/active only │      │ most things     │      │ everything   │
              │ rest: stubs     │      │ some stubs      │      │ (cold store) │
              └─────────────────┘      └─────────────────┘      └──────────────┘

Any node can answer: "find all tracks tagged electronic from 1997"
Only nodes with the blob can serve the actual bytes.
```

At 10M objects across the cluster, the full metadata set — all bitmaps, forward index, ontology, object records — is maybe 500 MB to 2 GB. Every laptop, phone, or edge node can hold a full replica. You search locally, instantly, and only reach out to the network when you need bytes.

## Object Identity Across Nodes

Objects need globally unique IDs that don't collide across nodes creating objects independently. Two viable approaches:

```rust
// Option A: Node-prefixed IDs
// Top 16 bits = node ID, bottom 48 bits = local sequence
// Simple, no coordination, 65536 nodes × 281 trillion objects each
#[derive(Copy, Clone, Hash, Eq, PartialEq)]
struct ObjectId(u64);

impl ObjectId {
    fn new(node: u16, local_seq: u48) -> Self {
        Self((node as u64) << 48 | local_seq as u64)
    }
    fn node(&self) -> u16 { (self.0 >> 48) as u16 }
    fn local(&self) -> u64 { self.0 & 0x0000_FFFF_FFFF_FFFF }
}

// Option B: Content-derived IDs
// BLAKE3(content) truncated to 64 bits (collision-resistant enough
// for practical purposes, or use full 128/256 bits)
// Gives you dedup across nodes for free.
// Downside: ID changes if content changes.
```

Option A is better for a mutable filesystem. Content addressing can be a secondary index for dedup, but the primary ID must be stable across edits.

## Content Presence: The Hydration Model

Every node knows about every object, but may not have the bytes. The location table gains a presence state:

```rust
enum ContentPresence {
    // Full blob stored locally
    Local {
        disk_id: u16,
        extent_offset: u64,
        extent_length: u64,
    },
    
    // Blob not local, but we know where it is
    Remote {
        origin_node: NodeId,
        // Optional: multiple remotes for redundancy
        mirrors: Vec<NodeId>,
    },
    
    // Fetching in progress
    Hydrating {
        source: NodeId,
        progress_bytes: u64,
        total_bytes: u64,
    },
    
    // Local but scheduled for eviction (LRU / policy)
    Cached {
        disk_id: u16,
        extent_offset: u64,
        extent_length: u64,
        fetched_at: Timestamp,
        last_accessed: Timestamp,
        evictable: bool,  // false if pinned by placement rule
    },
    
    // Partial: we have a thumbnail / preview / first N bytes
    Partial {
        stub_disk_id: u16,
        stub_offset: u64,
        stub_length: u64,      // what we have locally
        full_length: u64,      // total size
        origin_node: NodeId,
    },
}
```

This is essentially what macOS `fileproviderd` does, but richer because the ontology can drive eviction and prefetch policy:

```rust
// Ontology-driven hydration policy (same as placement rules!)
enum HydrationRule {
    // Always keep local: tag:active AND tag:project-vesper
    Pin {
        query: Query,
    },
    
    // Keep thumbnail/preview locally, full content remote
    StubOnly {
        query: Query,
        stub_strategy: StubStrategy,
    },
    
    // Prefetch when related objects are accessed
    // "If user opens 3 tracks from album X, prefetch the rest"
    Prefetch {
        trigger: Query,
        threshold: usize,    // how many accesses before prefetch
        target: Query,       // what to prefetch
    },
    
    // Evict after N days without access
    AutoEvict {
        query: Query,
        days_unused: u32,
        keep_stub: bool,
    },
}

enum StubStrategy {
    FirstNBytes(u64),        // first 64KB (enough for file type detection)
    Thumbnail,               // image/video thumbnail extracted on origin
    MetadataOnly,            // just the assertions, no content at all
}
```

## Metadata Sync Protocol

This is the heart of it. The metadata must be **eventually consistent** across all nodes, with sync being efficient even over slow links.

### What Needs Syncing

```
Per object (created/modified):
  - ObjectRecord (128 bytes)
  - Assertions (tags, attrs, relations — variable, ~200-400 bytes)
  - Content hash + size (for dedup detection)
  - Origin node (where blob lives)

Per tag (bitmap changed):
  - Bitmap delta (objects added/removed from this tag)

Ontology (rarely changes):
  - Full ontology snapshot (small, <1MB typically)
  - Version number
```

### The Sync Unit: Operation Log

Instead of syncing full state, sync **operations**. Every mutation generates a log entry:

```rust
#[derive(Serialize, Deserialize)]
struct SyncOp {
    // Globally ordered by (timestamp, node_id) — Lamport-ish
    timestamp: HybridTimestamp,
    origin_node: NodeId,
    sequence: u64,           // per-node monotonic, for gap detection
    
    op: SyncOpKind,
}

enum SyncOpKind {
    // Object lifecycle
    CreateObject {
        id: ObjectId,
        content_hash: [u8; 32],
        content_size: u64,
        initial_assertions: Vec<Assertion>,
    },
    DeleteObject {
        id: ObjectId,
    },
    
    // Tag mutations
    AddTag {
        object: ObjectId,
        tag: TagId,
    },
    RemoveTag {
        object: ObjectId,
        tag: TagId,
    },
    AddAttr {
        object: ObjectId,
        key: TagId,
        value: Value,
    },
    
    // Ordered collection ops
    InsertAt {
        tag: TagId,           // the collection tag
        object: ObjectId,
        position: u32,
    },
    Reorder {
        tag: TagId,
        object: ObjectId,
        new_position: u32,
    },
    
    // Ontology changes
    OntologyUpdate {
        version: u64,
        delta: OntologyDelta,
    },
    
    // Tag creation
    CreateTag {
        id: TagId,
        name: String,
        semantics: TagSemantics,
    },
}
```

### Hybrid Timestamps

Pure wall clocks drift. Pure logical clocks lose real-time ordering. Hybrid logical clocks give you both:

```rust
#[derive(Ord, PartialOrd, Eq, PartialEq, Copy, Clone)]
struct HybridTimestamp {
    wall_ms: u64,     // milliseconds since epoch (physical)
    logical: u16,     // tie-breaker for same millisecond
    node: u16,        // final tie-breaker
}

impl HybridTimestamp {
    fn now(node: NodeId, last: &mut Self) -> Self {
        let wall = system_time_ms();
        let ts = if wall > last.wall_ms {
            Self { wall_ms: wall, logical: 0, node: node.0 }
        } else {
            // Clock went backwards or same ms — increment logical
            Self { wall_ms: last.wall_ms, logical: last.logical + 1, node: node.0 }
        };
        *last = ts;
        ts
    }
    
    // On receiving a remote timestamp, advance local clock
    fn receive(remote: Self, node: NodeId, last: &mut Self) -> Self {
        let wall = system_time_ms();
        let new_wall = wall.max(remote.wall_ms).max(last.wall_ms);
        let logical = if new_wall == wall && new_wall == remote.wall_ms {
            last.logical.max(remote.logical) + 1
        } else if new_wall == remote.wall_ms {
            remote.logical + 1
        } else if new_wall == last.wall_ms {
            last.logical + 1
        } else {
            0
        };
        let ts = Self { wall_ms: new_wall, logical, node: node.0 };
        *last = ts;
        ts
    }
}
```

These timestamps give total ordering across all nodes without coordination, which means ops can be replayed in deterministic order on every node.

### Efficient Bitmap Delta Sync

Here's where roaring bitmaps really shine. When node A tags 50 objects with `electronics`, the naive sync sends the full bitmap. But roaring supports efficient XOR:

```rust
// Node A's view after local mutations:
let old_bitmap = electronics_bitmap_at_last_sync();  // 50,000 objects
let new_bitmap = electronics_bitmap_now();            // 50,050 objects

// Delta: what changed?
let added   = &new_bitmap - &old_bitmap;   // ~50 objects, tiny bitmap
let removed = &old_bitmap - &new_bitmap;   // 0 objects

// Serialize delta — NOT the full bitmap
let delta = BitmapDelta {
    tag_id: tag("electronics"),
    added: added.serialize(),     // maybe 200 bytes for 50 objects
    removed: removed.serialize(), // 0 bytes
};

// Over the wire: 200 bytes instead of 50KB for the full bitmap
```

On the receiving node:

```rust
fn apply_bitmap_delta(store: &mut TagStore, delta: &BitmapDelta) {
    match store {
        TagStore::Simple(bitmap) => {
            *bitmap |= &delta.added;
            *bitmap -= &delta.removed;
        }
        TagStore::Ordered { members, sequence } => {
            *members |= &delta.added;
            *members -= &delta.removed;
            // Sequence updates come separately as InsertAt/Reorder ops
            sequence.retain(|id| !delta.removed.contains(*id as u32));
        }
        _ => { /* similar */ }
    }
}
```

### Sync Protocol Flow

```
Node A (laptop)                        Node B (NAS)
─────────────────                      ──────────────

  Creates track, tags it               
  ops: [CreateObj(42),                  
        AddTag(42, music),              
        AddTag(42, electronic)]         
        
  Local oplog:                          
  seq=100: CreateObj(42)                
  seq=101: AddTag(42, music)            
  seq=102: AddTag(42, electronic)       

       ──── periodic sync ────▶         

  "I have ops up to seq=102"            "My last from A was seq=97"
                                        
       ◀── "send me 98..102" ──         

  Sends ops 98..102                     Applies ops:
  (5 ops, maybe 500 bytes)              - creates obj 42 metadata
       ────────────────────▶            - sets presence = Remote(A)
                                        - updates bitmaps
                                        - obj 42 searchable immediately
                                        - blob NOT transferred
                                        
                                        User on B searches "electronic"
                                        → finds obj 42
                                        → tries to open it
                                        
       ◀── "hydrate obj 42" ──          
                                        
  Sends blob content                    Receives blob
  (streaming, async)                    presence → Cached { ... }
       ────────────────────▶            
```

### Batch Sync for Initial Join / Reconnect

When a new node joins or a node reconnects after being offline for a long time, replaying individual ops is too slow. Send a snapshot instead:

```rust
enum SyncMode {
    // Normal: send ops since last sync point
    Incremental {
        since_seq: HashMap<NodeId, u64>,  // per-node sequence watermarks
    },
    
    // Catch-up: too far behind, send compressed full state
    Snapshot {
        // All bitmaps, serialized together (compresses extremely well
        // because roaring is already compact)
        all_bitmaps: Vec<(TagId, Vec<u8>)>,
        
        // All object records, packed
        object_table: Vec<u8>,       // 10M × 128B = 1.28 GB uncompressed
                                     // ~200-400 MB with zstd
        
        // Ontology
        ontology: Vec<u8>,
        
        // Oplog watermarks so incremental can resume
        watermarks: HashMap<NodeId, u64>,
    },
}

// Decision: incremental vs snapshot
fn choose_sync_mode(
    local_watermarks: &HashMap<NodeId, u64>,
    remote_watermarks: &HashMap<NodeId, u64>,
) -> SyncMode {
    let total_gap: u64 = local_watermarks.iter()
        .map(|(node, local_seq)| {
            let remote_seq = remote_watermarks.get(node).unwrap_or(&0);
            local_seq.saturating_sub(*remote_seq)
        })
        .sum();
    
    if total_gap > 1_000_000 {
        // More than 1M ops behind — snapshot is faster
        SyncMode::Snapshot { /* ... */ }
    } else {
        SyncMode::Incremental {
            since_seq: remote_watermarks.clone(),
        }
    }
}
```

## Conflict Resolution

Two nodes tag the same object concurrently. With tags and bitmaps, most conflicts **don't exist** because the operations are commutative:

```
Node A: AddTag(42, jazz)         Node B: AddTag(42, chill)

After sync, both nodes have:  obj 42 → [jazz, chill]
No conflict! Union of tags is the correct answer.
```

```
Node A: AddTag(42, jazz)         Node B: RemoveTag(42, jazz)

Conflict! Resolve by timestamp — last writer wins.
Or by policy: "add wins over remove" (safer for a filesystem).
```

```rust
enum ConflictPolicy {
    // Tag add always wins over tag remove (no accidental data loss)
    AddWins,
    // Last timestamp wins (more intuitive for users)
    LastWriterWins,
    // Keep both, mark as conflicted (user resolves)
    MarkConflict,
}

fn resolve_tag_conflict(
    op_a: &SyncOp,
    op_b: &SyncOp,
    policy: ConflictPolicy,
) -> SyncOp {
    match policy {
        ConflictPolicy::AddWins => {
            // If either op is an add, the tag stays
            match (&op_a.op, &op_b.op) {
                (SyncOpKind::AddTag { .. }, _) => op_a.clone(),
                (_, SyncOpKind::AddTag { .. }) => op_b.clone(),
                _ => if op_a.timestamp > op_b.timestamp { op_a } else { op_b }.clone(),
            }
        }
        ConflictPolicy::LastWriterWins => {
            if op_a.timestamp > op_b.timestamp { op_a } else { op_b }.clone()
        }
        ConflictPolicy::MarkConflict => {
            // Add a "conflicted" tag to the object, let user resolve
            // ...
        }
    }
}
```

Ordered collections are harder — concurrent inserts at position 3 need merging. The practical solution is **RGA (Replicated Growable Array)** or just treating the sequence as LWW per collection (last full reorder wins). For playlists this is fine — if you reorder on your phone and your laptop simultaneously, one wins.

## Remote Storage Backends

S3 and similar object stores slot in as a storage backend that only holds blobs:

```rust
trait BlobBackend: Send + Sync {
    // Read blob content
    async fn read(&self, obj_id: ObjectId, range: Option<Range<u64>>) 
        -> Result<Bytes>;
    
    // Write blob content
    async fn write(&self, obj_id: ObjectId, data: &[u8]) 
        -> Result<()>;
    
    // Delete blob
    async fn delete(&self, obj_id: ObjectId) -> Result<()>;
    
    // Check existence without fetching
    async fn exists(&self, obj_id: ObjectId) -> Result<bool>;
    
    // Backend properties
    fn latency_class(&self) -> LatencyClass;
    fn cost_per_gb(&self) -> f64;
    fn supports_range_reads(&self) -> bool;
}

enum LatencyClass {
    Local,          // <1ms    — NVMe, SSD, HDD
    Lan,            // 1-10ms  — NAS, local cluster
    Wan,            // 10-200ms — remote server, CDN
    ColdStorage,    // seconds to minutes — S3 Glacier, tape
}

// Implementations:
struct LocalDiskBackend { /* raw disk, as designed */ }
struct S3Backend { bucket: String, region: String, client: S3Client }
struct SftpBackend { host: String, path: String }
struct IpfsBackend { /* content-addressed, perfect fit */ }
```

The placement engine treats remote backends as additional tiers:

```
Tier mapping:
  StorageTier::Hot       → local NVMe
  StorageTier::Warm      → local SSD / NAS
  StorageTier::Cold      → S3 Standard
  StorageTier::Glacier   → S3 Glacier / Backblaze B2

Placement rules work identically:
  Pin { query: HasTag("active"), tier: Hot }         → local NVMe
  Prefer { query: HasTag("archive"), tier: Glacier } → S3 Glacier
  AutoEvict { days_unused: 90, keep_stub: true }     → evict locally,
                                                        blob stays on S3
```

## The Full Cluster Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                        CLUSTER METADATA BUS                         │
│                                                                     │
│   SyncOps flow between all nodes (gossip / hub / CRDT)             │
│   Every node has full metadata replica                              │
│                                                                     │
│    Node A ◄──────────► Node B ◄──────────► Node C                   │
│   (laptop)    sync     (NAS)      sync    (always-on               │
│                                            hub, optional)           │
│                                                                     │
│   ┌──── Node D ────┐   ┌──── Node E ────┐                          │
│   │  S3 backend    │   │  Phone/tablet  │                          │
│   │  (blobs only,  │   │  (metadata +   │                          │
│   │   no metadata  │   │   stubs only)  │                          │
│   │   replica)     │   │                │                          │
│   └────────────────┘   └────────────────┘                          │
└─────────────────────────────────────────────────────────────────────┘
```

Node types by capability:

```rust
struct NodeDescriptor {
    id: NodeId,
    name: String,
    
    role: NodeRole,
    
    // What this node stores
    has_full_metadata: bool,     // almost always true
    blob_backends: Vec<BackendRef>,
    
    // Connectivity
    addresses: Vec<NodeAddress>,
    last_seen: Timestamp,
    sync_watermarks: HashMap<NodeId, u64>,
}

enum NodeRole {
    // Full participant: metadata + local blobs + syncs with peers
    Full,
    
    // Metadata + stubs only: searches everything, fetches on demand
    // (phones, laptops with small SSDs)
    Thin,
    
    // Blob storage only: S3, Backblaze, NAS without compute
    // Doesn't replicate metadata, just serves bytes
    BlobStore,
    
    // Always-on hub: routes sync between intermittently connected nodes
    // Optional but useful when nodes aren't always online simultaneously
    Hub,
}
```

### The Hub Problem

If node A and node B are never online simultaneously (laptop at work, desktop at home), they can't sync directly. A hub node (cheap VPS, Raspberry Pi, or just S3 + Lambda) acts as a relay:

```
Node A online 9am-6pm          Hub (always on)         Node B online 6pm-12am
─────────────────────          ──────────────           ──────────────────────

  A pushes ops ──────────────▶ stores ops
                               ────────────────────▶ B pulls ops
  
  A pulls ops ◀────────────── stores ops
                               ◀──────────────────── B pushes ops
```

The hub only needs to store the oplog — it doesn't need the full bitmap index. A bounded oplog (last 7 days, or last 10M ops) is enough. If a node is offline longer than that, it does a full snapshot sync when it reconnects.

## Bandwidth Budget

How much does this actually cost over the wire?

```
Daily sync for a moderately active user:
  ~1000 new objects/day           × 500B metadata each  = 500 KB
  ~5000 tag mutations/day         × 50B each            = 250 KB
  ~10 ontology changes/day        × 200B each           = 2 KB
  bitmap deltas (compressed)                             ≈ 100 KB
                                                    ─────────────
  Total metadata sync:                                ≈ 850 KB/day

  Blob fetches (on demand):       varies entirely by usage
  
Full snapshot (new node join / recovery):
  10M objects × 128B records      = 1.28 GB → ~300 MB zstd
  All bitmaps                     = ~200 MB → ~80 MB zstd  
  Ontology                        = ~1 MB
                                                    ─────────────
  Total snapshot:                                   ≈ 400 MB
```

Under a megabyte per day for metadata sync. A full cluster join in under a minute on broadband. The blobs are the only thing that's big, and those only move when someone actually opens a file.

# Chunking - probably!

It depends entirely on the workload, and for most associative filesystem use cases the answer is: **the chunk index cost outweighs the sync savings, except for one specific scenario**.

Let me work through the numbers honestly.

## What Content-Defined Chunking Does

Rabin-Karp (or the faster alternatives — FastCDC, Gear fingerprinting) slides a window over file content and splits at content-determined boundaries. The key property: if you insert 10 bytes in the middle of a 100 MB file, only the chunk containing the edit changes. All other chunks have identical hashes.

```
Original file (100 MB):
┌──────────┬──────────┬──────────┬──────────┬──────────┐
│ Chunk A  │ Chunk B  │ Chunk C  │ Chunk D  │ Chunk E  │
│ 1.2 MB   │ 0.8 MB   │ 1.1 MB   │ 0.9 MB   │ 1.0 MB   │ ...
│ hash:a1  │ hash:b2  │ hash:c3  │ hash:d4  │ hash:e5  │
└──────────┴──────────┴──────────┴──────────┴──────────┘

After editing middle of Chunk C:
┌──────────┬──────────┬───────────┬──────────┬──────────┐
│ Chunk A  │ Chunk B  │ Chunk C'  │ Chunk D  │ Chunk E  │
│ 1.2 MB   │ 0.8 MB   │ 1.15 MB  │ 0.9 MB   │ 1.0 MB   │ ...
│ hash:a1  │ hash:b2  │ hash:c7  │ hash:d4  │ hash:e5  │
└──────────┴──────────┴───────────┴──────────┴──────────┘
  same       same       CHANGED     same       same

Sync cost: transfer 1.15 MB instead of 100 MB
```

This is spectacular for that case. But let's look at what it costs.

## The Chunk Index

Every file is no longer a single extent. It's a list of chunk references:

```rust
// WITHOUT chunking (current design):
struct ObjectLocation {
    disk_id: u16,
    extent_offset: u64,
    extent_length: u64,
    // ...
}
// 24 bytes per object. 10M objects = 240 MB.

// WITH chunking:
struct ChunkedObjectLocation {
    chunk_count: u32,
    // For small files (< target chunk size): inline, same as before
    // For large files: pointer to chunk list
    chunks: ChunkListRef,
}

struct ChunkRef {
    hash: [u8; 32],     // BLAKE3 of chunk content
    length: u32,        // chunk size (typically 64KB-4MB)
}
// 36 bytes per chunk.

// A 100 MB file with 1 MB average chunks = 100 chunks × 36B = 3.6 KB
// A 10 KB config file = 1 chunk × 36B = 36 bytes (overhead!)
```

The new index that's needed:

```rust
// Chunk Content Index: hash → physical location(s)
// This is the dedup table — multiple objects can reference same chunk
struct ChunkStore {
    // hash → where the bytes are
    index: HashMap<ChunkHash, ChunkLocation>,
    // reference count for GC
    refcounts: HashMap<ChunkHash, u32>,
}

struct ChunkLocation {
    disk_id: u16,
    offset: u64,
    length: u32,
    // Which nodes have this chunk (for cluster sync)
    present_on: SmallVec<[NodeId; 4]>,
}
```

### The Size of the Chunk Index

This is where it gets expensive. Let's calculate for a realistic dataset:

```
10M objects, average 1 MB each = 10 TB total content
Average chunk size: 1 MB (typical for FastCDC)
Total chunks: ~10M

Chunk index:
  10M entries × (32B hash + 16B location + 4B refcount) = 520 MB

Chunk-to-object mapping:
  10M objects × average 10 chunks × 36B = 3.6 GB
  (most objects are small — 1-2 chunks — but large files dominate)

Compare to current design:
  Location table: 10M × 24B = 240 MB
  
Chunk overhead: ~4 GB vs 240 MB = 16× more metadata
```

That 4 GB now needs to be synced across the cluster too. It doesn't fit comfortably in RAM on a thin node. And every file open requires resolving a chunk list instead of a single extent read.

## Read Path Cost

```
WITHOUT chunks (current):
  open file → location table → single extent → sequential read
  1 seek, 1 sequential read
  Latency: ~50 μs (NVMe) to ~10 ms (HDD)

WITH chunks:
  open file → chunk list → for each chunk: lookup hash → location → read
  N seeks, N reads (though sequential if chunks are colocated)
  Latency: ~50 μs × N (best case, colocated)
           ~50 μs × N + overhead per lookup (worst case)
```

For a 100 MB video file with 100 chunks, even if chunks are colocated on disk, you're doing 100 index lookups instead of 1. On the read path this is pure overhead unless you're doing dedup or partial sync.

## When Chunking Actually Helps

There are exactly three scenarios where it pays off:

### 1. Large Files That Get Small Edits

Database files, virtual machine images, large documents (InDesign, Photoshop). You edit 1% of the file, sync 1% instead of 100%.

```
File type              Avg size    Edit pattern         Chunk benefit
─────────────────────────────────────────────────────────────────────
Music track (MP3/FLAC) 5-30 MB     Immutable            NONE
Photo (RAW/JPEG)       5-50 MB     Immutable            NONE
Video (MP4)            100MB-10GB  Immutable             NONE
PDF                    1-50 MB     Replaced wholesale    NONE
Source code            1-100 KB    Small edits           MINIMAL (file is small)
VM disk image          10-100 GB   Scattered writes      HUGE
Database file          1-100 GB    Scattered writes      HUGE
Photoshop PSD          50MB-2GB    Layer edits           SIGNIFICANT
Word/Excel (zipped)    1-50 MB     Re-zipped = all new   NONE (zip breaks CDC)
```

For a music/photo/video collection — which is a primary use case for an associative filesystem — almost every file is **write-once, read-many**. Chunking adds overhead on every read and gives back nothing on sync because the whole file changed (or didn't change at all).

### 2. Deduplication Across Objects

If many objects share content (copies of the same file, templates, boilerplate), chunk-level dedup catches partial overlap:

```
Document A: [header][chapter1][chapter2][appendix]
Document B: [header][chapter1][chapter3][appendix]
                                  ↑ different

Without chunks: 2 full copies stored
With chunks: header, chapter1, appendix stored once
             chunk refcount = 2
Savings: ~50% for this pair
```

But in an associative filesystem, exact file dedup is already handled by content hashing at the object level — same hash means same file, store once. Chunk-level dedup only catches _partial_ overlap, which is less common than it sounds for most file types.

### 3. Resumable Transfer Over Flaky Links

If node A is syncing a 2 GB file to node B over a connection that drops every 5 minutes, chunks let you resume from where you left off. Without chunks, you restart the whole file.

```
WITH chunks:
  Send chunk 1 ✓
  Send chunk 2 ✓
  Send chunk 3 ✓
  CONNECTION DROPS
  Reconnect...
  "I have chunks 1-3, send from chunk 4"
  Send chunk 4 ✓
  ...

WITHOUT chunks:
  Send file, 60% complete...
  CONNECTION DROPS
  Reconnect...
  Send file from beginning (or use HTTP range if backend supports it)
```

But this is solvable without full CDC. Simple fixed-size chunking (split at every 4 MB boundary) gives you resumability without the complexity of content-defined boundaries. You lose the "edit in the middle" delta property, but you keep resumability.

## The Hybrid Answer

Don't chunk everything. Chunk selectively based on — you guessed it — the ontology:

```rust
enum ContentStorage {
    // Default: single extent, no chunking
    // For: music, photos, videos, PDFs, small files
    Extent,
    
    // Content-defined chunking with dedup
    // For: VM images, databases, large mutable documents
    Chunked {
        algorithm: ChunkAlgorithm,
        target_size: u32,     // e.g., 1 MB
        min_size: u32,        // e.g., 256 KB
        max_size: u32,        // e.g., 4 MB
    },
    
    // Fixed-size chunking (resumability without CDC overhead)
    // For: large immutable files transferred over unreliable links
    FixedChunked {
        chunk_size: u32,      // e.g., 4 MB
    },
}

enum ChunkAlgorithm {
    FastCDC,          // best performance, most common
    RabinKarp,        // classic, slower
    Gear,             // simpler, nearly as good as FastCDC
}
```

The ontology decides:

```rust
// Ontology-driven chunking policy
enum ChunkingRule {
    // VM images → always CDC chunked
    Chunk {
        query: Query,                    // HasTag("vm-image")
        storage: ContentStorage,
    },
    
    // Music, photos, video → never chunked
    NoChunk {
        query: Query,                    // HasTag("media")
    },
    
    // Large files over threshold → fixed chunking for resumability
    ChunkIfLarge {
        query: Query,                    // any object
        threshold: u64,                  // > 100 MB
        storage: ContentStorage,
    },
}
```

### The On-Disk Impact

Only chunked objects need the chunk index. The location table becomes a two-tier structure:

```rust
enum ObjectContent {
    // Majority of objects: direct extent reference
    // No chunk index involved
    Direct {
        disk_id: u16,
        offset: u64,
        length: u64,
    },
    
    // Chunked objects: indirect through chunk list
    Chunked {
        chunk_list_offset: u64,  // points into chunk list area
        chunk_count: u32,
        total_length: u64,
    },
}
```

```
Location Table (unchanged for most objects):
┌──────────┬──────────────────────────────────────┐
│ obj 0    │ Direct { disk:0, off:0x1000, len:5MB }│
│ obj 1    │ Direct { disk:1, off:0x8000, len:200K}│
│ obj 2    │ Direct { disk:0, off:0x6000, len:12MB}│
│ obj 3    │ Chunked { list:0xA00, count:150 }     │  ← VM image
│ obj 4    │ Direct { disk:2, off:0x1000, len:3MB } │
│ ...      │                                        │
└──────────┴──────────────────────────────────────┘

Chunk List Area (only for chunked objects):
┌──────────┬─────────────────────────────────────────────┐
│ 0xA00    │ [hash:aa11, len:1MB] [hash:bb22, len:1.2MB] │
│          │ [hash:cc33, len:0.9MB] ... × 150             │
└──────────┴─────────────────────────────────────────────┘

Chunk Content Index (maps hash → physical location):
┌──────────┬──────────────────────────────┐
│ hash:aa11│ disk:1, off:0x50000, ref:2   │  ← shared by 2 objects
│ hash:bb22│ disk:1, off:0x60000, ref:1   │
│ hash:cc33│ disk:0, off:0x90000, ref:1   │
└──────────┴──────────────────────────────┘
```

If only 1% of objects are chunked (the VM images, large databases), the chunk index is 100K entries instead of 10M. That's 5 MB instead of 4 GB. Completely manageable.

## Sync Protocol Change

The sync protocol gains one new op type for chunk-level delta sync:

```rust
enum SyncOpKind {
    // ... existing ops ...
    
    // Blob sync for non-chunked objects (current behavior):
    // "Send me the whole file"
    RequestBlob { object: ObjectId },
    
    // Blob sync for chunked objects:
    // "Here are the chunk hashes I have, send me what I'm missing"
    RequestChunkedBlob {
        object: ObjectId,
        have_chunks: Vec<ChunkHash>,  // what the requester already has
    },
    
    // Response: only the missing chunks
    ChunkData {
        object: ObjectId,
        chunks: Vec<(ChunkHash, Bytes)>,
    },
}
```

The flow for syncing a modified VM image:

```
Node A (edited VM)                    Node B (has old version)
──────────────────                    ────────────────────────

VM image modified:                    Has old chunk list:
  150 chunks total                      150 chunks, 145 same hashes
  5 chunks changed                      5 chunks with old hashes

         ◀── RequestChunkedBlob ───
             have: [aa11, bb22, cc33, ...]  (150 hashes, ~5KB)

Diff: they're missing 5 chunks
Send only those 5 × 1MB = 5 MB

         ─── ChunkData ──────────▶   Receives 5 MB instead of 150 MB
                                     Stores new chunks
                                     Updates chunk list
                                     Object now identical to Node A
```

## Bottom Line

```
Workload                     Chunk?    Why
────────────────────────────────────────────────────────────────
Music/photo/video collection  No       Immutable files, pure overhead
Source code repos              No       Files are small, git handles this
VM images, databases          Yes (CDC) Huge files, small edits, massive sync savings
Large mutable docs (PSD)      Yes (CDC) Significant edit-locality
Large immutable transfers     Maybe    Fixed-chunk for resumability only
Mixed general filesystem      Selective Ontology decides per-object
```

Implement extent-only first. Add chunking as an ontology-driven option for specific tag patterns. The infrastructure cost — chunk index, modified sync protocol, chunk GC — is only justified when the objects are large and mutable, and the ontology is exactly the right place to make that decision.

# Transparent compression and encryption

Compression and encryption both transform data between "what the upper layers see" and "what hits the disk." The question is where in the I/O stack they sit, and whether they should be the same layer or separate.

## The Transform Pipeline

Every read and write already passes through a logical pipeline. Compression and encryption slot in as transform stages:

```
WRITE PATH (top to bottom):

  Application data (plaintext, uncompressed)
       │
       ▼
  ┌─────────────────────┐
  │ Metadata extraction │  tags, assertions, content hash
  │ (operates on plain  │  (hash of ORIGINAL data, not transformed)
  │  data, always)      │
  └─────────┬───────────┘
            │
            ▼
  ┌─────────────────────┐
  │ Compression         │  optional, per-object policy
  │                     │  reduces size before encryption
  └─────────┬───────────┘
            │
            ▼
  ┌─────────────────────┐
  │ Encryption          │  optional, per-object or whole-fs
  │                     │  operates on compressed data
  └─────────┬───────────┘
            │
            ▼
  ┌─────────────────────┐
  │ Extent allocation   │  allocates based on POST-compression size
  │ + disk write        │  writes ciphertext to disk
  └─────────────────────┘


READ PATH (bottom to top):  exact reverse
```

The order matters. **Compress then encrypt**, always. Encrypted data is indistinguishable from random — it doesn't compress. Compressing first is universally correct and is what ZFS, LUKS+btrfs, and FileVault all do.

The content hash must be computed on the **original plaintext** data, before any transforms. This ensures dedup works across nodes that may use different compression algorithms, and that content verification is independent of storage format.

```rust
struct TransformMeta {
    // Original data properties (before any transform)
    content_hash: [u8; 32],     // BLAKE3 of plaintext
    original_size: u64,
    
    // What was applied (stored in object record)
    compression: CompressionState,
    encryption: EncryptionState,
    
    // Post-transform (what's actually on disk)
    stored_size: u64,           // after compression + encryption
}

enum CompressionState {
    None,
    Compressed {
        algorithm: CompressionAlg,
        // No need to store compressed size separately —
        // it's stored_size (before encryption overhead)
    },
    Incompressible,  // tried, didn't shrink, stored raw
                     // prevents retrying on every read
}

enum EncryptionState {
    None,
    Encrypted {
        scheme: EncryptionScheme,
        key_id: KeyId,          // which key encrypted this
        nonce: [u8; 12],        // unique per object (AES-GCM)
        // auth tag stored with ciphertext
    },
}
```

## Compression

### Algorithm Selection

Different data compresses differently. The ontology knows what the data is:

```rust
enum CompressionAlg {
    Zstd { level: i8 },    // -3 to 19, default 3. Best general purpose.
    Lz4,                    // Fastest decompress. Good for hot data.
    Zlib { level: u8 },     // Compatibility. Rarely the right choice.
    None,                   // Explicitly uncompressed.
}
```

Ontology-driven compression policy — same pattern as placement rules and chunking:

```rust
enum CompressionRule {
    // Already-compressed formats: don't waste CPU
    Skip {
        query: Query,   // HasTag("video") OR HasTag("jpeg") OR HasTag("mp3")
    },
    
    // Text, source code, logs: compress aggressively
    Aggressive {
        query: Query,   // HasTag("text") OR HasTag("source") OR HasTag("log")
        algorithm: CompressionAlg,  // Zstd { level: 9 }
    },
    
    // Hot data: fast compression, fast decompression
    Fast {
        query: Query,   // HasTag("active") OR HasTag("cache")
        algorithm: CompressionAlg,  // Lz4
    },
    
    // Default for everything else
    Default {
        algorithm: CompressionAlg,  // Zstd { level: 3 }
    },
}
```

Why this matters in practice:

```
File type        Raw size   Zstd-3    Ratio   Notes
──────────────────────────────────────────────────────────
JSON/XML         10 MB      0.8 MB    12:1    Compress aggressively
Source code      500 KB     120 KB    4:1     Compress aggressively
SQLite DB        50 MB      15 MB     3:1     Good candidate
PDF (text-heavy) 2 MB       1.6 MB    1.25:1  Marginal
JPEG photo       5 MB       4.95 MB   1.01:1  Skip — waste of CPU
MP3 audio        8 MB       7.98 MB   1.00:1  Skip
H.264 video      500 MB     499 MB    1.00:1  Skip
VM image         10 GB      4 GB      2.5:1   Sparse regions compress well
```

Trying to compress a JPEG wastes ~50 μs of CPU for zero gain. The ontology knows it's a JPEG (from the `image` tag or MIME type), so it skips compression entirely. ZFS can only decide this per-dataset, not per-file.

### Compression Granularity

Two options, each with tradeoffs:

**Per-object compression** (simpler, current design fits naturally):

```
Object on disk:
┌─────────────┬──────────────────────────────┐
│ Extent hdr  │ zstd-compressed blob         │
│ 16 bytes    │ (single compression frame)   │
└─────────────┴──────────────────────────────┘

Decompress: read entire extent, decompress, return to caller
Random access: must decompress whole object first
```

Works well when objects are read entirely (music, photos, documents). Doesn't work for random access into large files.

**Per-block compression** (like ZFS, needed for large files):

```
Object on disk (4KB block compression):
┌────────┬────────┬────────┬────────┬────────┐
│Block 0 │Block 1 │Block 2 │Block 3 │Block 4 │ ...
│ 2.1 KB │ 3.8 KB │ 1.9 KB │ 4.0 KB │ 2.5 KB │
│ (cmpr) │ (cmpr) │ (cmpr) │ (raw)  │ (cmpr) │
└────────┴────────┴────────┴────────┴────────┘

Block 3 didn't compress (ratio < 1.0), stored raw.
Each block independently decompressible.
Random read at offset 12288: decompress only Block 3.
```

The problem: variable-size compressed blocks need a block map to find them on disk. This is essentially the same problem as chunking. For non-chunked objects you'd need a small block offset table:

```rust
// Stored alongside compressed object extent
struct CompressedBlockMap {
    logical_block_size: u32,        // e.g., 4096
    block_count: u32,
    // Cumulative offsets into the compressed extent
    // block N starts at offsets[N] bytes from extent start
    offsets: Vec<u32>,
}
```

The practical answer: **per-object for files under ~16 MB** (read the whole thing anyway), **per-block for large files** that need random access. The threshold is configurable and — again — the ontology can inform it. A 50 MB VM image needs per-block. A 50 MB movie doesn't (it's already incompressible and read sequentially).

### Interaction with Chunking

If an object is both chunked and compressed, the question is: compress each chunk independently, or compress the whole file then chunk?

**Compress then chunk** — terrible. CDC boundaries depend on byte values. Compression changes all byte values. Different compression levels produce different boundaries. Dedup across nodes with different compression settings breaks completely.

**Chunk then compress each chunk independently** — correct. Each chunk is compressed separately. Chunk hashes are computed on uncompressed content (so dedup works). Compressed chunks are what hits disk and network.

```
Data flow for chunked + compressed object:

  Original file bytes
       │
       ├── CDC split ──▶ Chunk A (1.2 MB plaintext)
       │                     │
       │                     ├── content_hash = BLAKE3(plaintext)
       │                     ├── compress(Zstd-3) → 400 KB
       │                     └── stored on disk: 400 KB
       │
       ├── CDC split ──▶ Chunk B (0.9 MB plaintext)
       │                     │
       │                     ├── content_hash = BLAKE3(plaintext)
       │                     ├── compress(Zstd-3) → 850 KB (barely compressed)
       │                     └── stored on disk: 850 KB
       ...
```

## Encryption

### Threat Model

What are you protecting against?

```
Threat                        Defense needed
─────────────────────────────────────────────────────────────
Stolen/lost disk              At-rest encryption (like FileVault)
Compromised remote node       Per-object encryption with local keys
Compromised S3 bucket         Encrypt before upload, keys never leave node
Untrusted hub node            Hub sees only ciphertext + encrypted metadata
Insider at cloud provider     Client-side encryption, no server-side keys
Multi-tenant on shared NAS    Per-user key isolation
```

The design needs to handle all of these, which means encryption at the object level with a key hierarchy, not just whole-disk encryption.

### Key Hierarchy

```
                    ┌───────────────────────┐
                    │ User Passphrase       │
                    │ (or hardware key,     │
                    │  TPM, biometric)      │
                    └───────────┬───────────┘
                                │
                         PBKDF2 / Argon2id
                                │
                                ▼
                    ┌───────────────────────┐
                    │ Master Key (KEK)      │
                    │ Key Encryption Key    │
                    │ Never used directly   │
                    │ for data              │
                    └───────────┬───────────┘
                                │
              ┌─────────────────┼──────────────────┐
              │                 │                  │
              ▼                 ▼                  ▼
    ┌──────────────┐  ┌──────────────┐   ┌──────────────┐
    │ Metadata Key │  │ Blob Key     │   │ Sync Key     │
    │ (MK)         │  │ Pool (BKP)   │   │ (SK)         │
    │              │  │              │   │              │
    │ Encrypts:    │  │ Encrypts:    │   │ Encrypts:    │
    │ - tag index  │  │ - default    │   │ - sync ops   │
    │ - fwd index  │  │   blob key   │   │ - over wire  │
    │ - ontology   │  │              │   │              │
    │ - obj records│  │              │   │              │
    └──────────────┘  └──────┬───────┘   └──────────────┘
                             │
                    Per-tag key derivation
                             │
               ┌─────────────┼─────────────┐
               ▼             ▼             ▼
         ┌──────────┐ ┌──────────┐  ┌──────────┐
         │ BK:work  │ │ BK:personal│ │BK:shared │
         │          │ │          │  │          │
         │ Objects  │ │ Objects  │  │ Objects  │
         │ tagged   │ │ tagged   │  │ tagged   │
         │ "work"   │ │ "personal"│ │ "shared" │
         └──────────┘ └──────────┘  └──────────┘
```

The per-tag blob keys are powerful. You can share the `BK:shared` key with another user without revealing your `BK:personal` data. This is impossible with whole-disk encryption.

```rust
struct KeyHierarchy {
    // Derived from passphrase, stored nowhere
    master_kek: Zeroizing<[u8; 32]>,
    
    // Encrypted under master_kek, stored in superblock
    metadata_key: WrappedKey,
    blob_key_pool: WrappedKey,
    sync_key: WrappedKey,
    
    // Per-tag blob keys, derived from blob_key_pool + tag_id
    // Not stored — derived deterministically on demand
    // HKDF(blob_key_pool, context=tag_id) → per-tag key
}

struct WrappedKey {
    ciphertext: [u8; 32 + 16],  // AES-256-GCM: 32B key + 16B tag
    nonce: [u8; 12],
}

impl KeyHierarchy {
    fn blob_key_for_tag(&self, tag_id: TagId) -> Zeroizing<[u8; 32]> {
        let pool_key = self.unwrap(&self.blob_key_pool);
        let mut okm = Zeroizing::new([0u8; 32]);
        hkdf_expand(
            &pool_key,
            &tag_id.to_le_bytes(),  // context
            &mut okm,
        );
        okm
    }
    
    // Object encryption key: derived from the "most specific"
    // tag that has a key policy, or falls back to pool key
    fn blob_key_for_object(&self, tags: &[TagId], policy: &EncryptionPolicy) 
        -> Zeroizing<[u8; 32]> 
    {
        // Find most specific encryption scope for this object's tags
        for tag in tags {
            if let Some(scope) = policy.key_scope(*tag) {
                return self.blob_key_for_tag(scope);
            }
        }
        // Default: pool-wide key
        self.unwrap(&self.blob_key_pool)
    }
}
```

### Encryption Scheme

AES-256-GCM for blobs (hardware-accelerated on all modern CPUs, authenticated). ChaCha20-Poly1305 as fallback for platforms without AES-NI.

```rust
enum EncryptionScheme {
    Aes256Gcm,          // Hardware accelerated, ~4 GB/s on modern x86
    ChaCha20Poly1305,   // Software-fast, constant-time, ~1.5 GB/s
    // Future: AES-256-GCM-SIV for nonce-reuse resistance
}

struct EncryptedBlob {
    nonce: [u8; 12],        // Unique per object write (random or counter)
    ciphertext: Vec<u8>,    // Encrypted (and possibly compressed) data
    auth_tag: [u8; 16],     // GCM authentication tag
}
```

Per-object nonces. Never reuse a nonce with the same key. With random 96-bit nonces and AES-GCM, you get a comfortable margin up to ~2^32 encryptions per key before birthday collision risk becomes concerning. With per-tag keys and millions of objects per tag, a counter-based nonce is safer:

```rust
fn generate_nonce(node_id: u16, counter: &AtomicU64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    // Bytes 0-1: node ID (prevents collision across nodes)
    nonce[0..2].copy_from_slice(&node_id.to_le_bytes());
    // Bytes 2-9: monotonic counter (prevents collision on same node)
    let c = counter.fetch_add(1, Ordering::Relaxed);
    nonce[2..10].copy_from_slice(&c.to_le_bytes());
    // Bytes 10-11: zero (or random for extra paranoia)
    nonce
}
```

### What Gets Encrypted and What Doesn't

This is the critical design decision. There's a spectrum:

```
Level 0: Nothing encrypted
  Everything plaintext. Simplest. No protection.

Level 1: Blobs only (FileVault-like)
  Tag index, metadata: plaintext (searchable!)
  Blob content: encrypted
  Threat model: stolen disk, curious admin
  
Level 2: Blobs + metadata
  Tag index: encrypted at rest, decrypted in RAM
  Object records: encrypted at rest
  Blob content: encrypted
  Threat model: stolen disk, forensic analysis

Level 3: Blobs + metadata + selective index
  Some tags visible (for indexing by untrusted nodes)
  Other tags encrypted (private tags)
  Blob content: always encrypted
  Threat model: untrusted cloud node

Level 4: Everything encrypted, including tag names
  Only the node with the key can search
  Untrusted nodes store opaque ciphertext
  Threat model: fully untrusted storage
```

For a practical system, **Level 2 with selective Level 3** is the sweet spot:

```rust
enum EncryptionPolicy {
    // Local-only node: encrypt at rest, decrypt into RAM at boot
    // User unlocks once (passphrase/biometric), everything searchable
    AtRest {
        unlock: UnlockMethod,
    },
    
    // Cloud/remote node: encrypt blobs before upload,
    // metadata synced encrypted with sync key
    RemoteEncrypted {
        blob_encryption: EncryptionScheme,
        metadata_encryption: EncryptionScheme,
    },
    
    // Selective: some tags define encryption boundaries
    // Objects tagged "medical" get a separate key
    // that isn't shared with the work cluster
    Selective {
        default: EncryptionScheme,
        overrides: Vec<TagEncryptionOverride>,
    },
}

struct TagEncryptionOverride {
    query: Query,            // HasTag("medical") OR HasTag("financial")
    key_scope: TagId,        // derive key from this tag
    share_with: Vec<NodeId>, // which nodes get this key
}
```

## Metadata Encryption Subtlety: Searchable Encryption

Here's the hard problem. If metadata is encrypted on a remote node, that node can't search it. You have three options:

**Option A: Decrypt in RAM on every node.** Every node gets the metadata key, decrypts the index into RAM at startup. Simple, but the metadata key must be distributed to every node — including untrusted ones. Protects against disk theft but not against a compromised node.

**Option B: Searchable symmetric encryption (SSE).** Encrypt the index such that the server can test "does this encrypted tag match this encrypted query?" without learning either. This is an active research area. Practical schemes exist (ORAM, encrypted bloom filters) but they're 10-100× slower than plaintext search.

**Option C: Tiered trust.** The pragmatic answer:

```
Trusted nodes (your devices):
  Full metadata decrypted in RAM
  Full search capability
  Hold master key and all derived keys

Semi-trusted nodes (NAS, home server):
  Full metadata decrypted in RAM (key provided at boot)
  Full search capability
  Keys in memory only, not persisted

Untrusted nodes (S3, cloud VPS, hub):
  NO metadata decryption
  Store encrypted metadata oplog (for relay to trusted nodes)
  Store encrypted blobs
  Cannot search, cannot read content
  Serve bytes on request (still encrypted)
```

```rust
enum NodeTrust {
    // Has master key. Searches locally. Full capability.
    Trusted,
    
    // Gets metadata key at boot (e.g., via network unlock).
    // Can search while running. Loses access on reboot until re-unlocked.
    SemiTrusted {
        unlock: RemoteUnlock,
    },
    
    // Never has keys. Stores and relays ciphertext only.
    Untrusted,
}

enum RemoteUnlock {
    // Another trusted node provides the key over TLS at boot
    NetworkKey { trusted_node: NodeId },
    // TPM/HSM on the semi-trusted node holds the key
    Hsm,
    // Manual passphrase entry (SSH or local console)
    Passphrase,
}
```

## The Transform Stack In Practice

Putting it all together, here's the complete write path:

```rust
async fn write_object(
    pool: &Pool,
    obj_id: ObjectId,
    data: &[u8],
    tags: &[TagId],
    policy: &StoragePolicy,
) -> Result<()> {
    // 1. Content hash (always on plaintext)
    let content_hash = blake3::hash(data);
    
    // 2. Determine transforms from ontology
    let compression = policy.compression_for(tags);
    let encryption = policy.encryption_for(tags);
    let chunking = policy.chunking_for(tags);
    
    // 3. Chunk if needed (on plaintext, before compression)
    let chunks: Vec<&[u8]> = match chunking {
        ContentStorage::Extent => vec![data],
        ContentStorage::Chunked { algorithm, .. } => {
            fastcdc_split(data, algorithm)
        }
        ContentStorage::FixedChunked { chunk_size } => {
            data.chunks(chunk_size as usize).collect()
        }
    };
    
    // 4. Per-chunk: compress then encrypt
    let mut stored_chunks = Vec::with_capacity(chunks.len());
    let blob_key = pool.keys.blob_key_for_object(tags, &encryption);
    
    for (i, chunk) in chunks.iter().enumerate() {
        // 4a. Hash chunk plaintext (for dedup / integrity)
        let chunk_hash = blake3::hash(chunk);
        
        // 4b. Compress
        let compressed = match compression {
            CompressionState::None => chunk.to_vec(),
            CompressionState::Compressed { algorithm } => {
                let c = compress(algorithm, chunk);
                if c.len() >= chunk.len() {
                    // Incompressible — store raw, mark it
                    chunk.to_vec()
                } else {
                    c
                }
            }
        };
        
        // 4c. Encrypt
        let encrypted = match &encryption {
            EncryptionState::None => compressed,
            EncryptionState::Encrypted { scheme, .. } => {
                let nonce = generate_nonce(pool.node_id, &pool.nonce_counter);
                encrypt(*scheme, &blob_key, &nonce, &compressed)
            }
        };
        
        // 4d. Allocate and write
        let location = pool.allocator.allocate(
            tags,                     // for placement rule evaluation
            encrypted.len() as u64,   // post-transform size!
        )?;
        pool.disk_write(&location, &encrypted).await?;
        
        stored_chunks.push(StoredChunk {
            content_hash: chunk_hash,
            location,
            original_size: chunk.len() as u64,
            stored_size: encrypted.len() as u64,
        });
    }
    
    // 5. Update metadata (WAL-protected)
    let txn = pool.begin_txn();
    txn.update_object_record(obj_id, TransformMeta {
        content_hash,
        original_size: data.len() as u64,
        compression,
        encryption,
        stored_size: stored_chunks.iter().map(|c| c.stored_size).sum(),
    });
    txn.update_location(obj_id, &stored_chunks);
    txn.commit().await?;
    
    Ok(())
}
```

The read path is the exact reverse — look up location, read ciphertext, decrypt, decompress, return plaintext. Each step is a pure function; the metadata tells you which transforms to undo.

## Performance Impact

```
Operation          No transforms   +Compression    +Encryption     +Both
                                   (Zstd-3)        (AES-256-GCM)
──────────────────────────────────────────────────────────────────────────
Write 1MB blob     ~200 μs         +150 μs         +50 μs          +200 μs
  (NVMe)                           (CPU bound)     (AES-NI)        

Read 1MB blob      ~200 μs         +80 μs          +50 μs          +130 μs
  (NVMe)                           (decompress     (AES-NI)
                                    faster)

Write 1MB blob     ~10 ms          +150 μs         +50 μs          +200 μs
  (HDD)                            (invisible      (invisible 
                                    behind I/O)     behind I/O)

Effective          1 GB/s          ~800 MB/s       ~4 GB/s         ~700 MB/s
throughput                         (CPU limited)   (HW accel)      (compress 
(sequential)                                                        limited)
```

Encryption with AES-NI is essentially free — 4 GB/s is faster than any storage device. Compression is the bottleneck, but it often _improves_ net throughput because you're writing fewer bytes to disk. On HDD, compression nearly always wins: the CPU time is invisible behind the I/O latency, and you read/write fewer sectors.

## Interaction with Cluster Sync

Encrypted sync has one important property: **encrypt before sending, decrypt only on trusted receivers**.

```
Node A (trusted)              Hub (untrusted)            Node B (trusted)
────────────────              ───────────────            ────────────────

SyncOp: AddTag(42, medical)
  │
  ├─ encrypt with sync_key
  │  → opaque bytes
  │
  └──────────────────────▶  Stores ciphertext    ─────────────────▶
                            Can't read ops            │
                            Can't search              ├─ decrypt with sync_key
                            Just relays               ├─ apply: AddTag(42, medical)
                                                      └─ update local bitmaps

Blob request for obj 42:
  Node B asks Hub
  Hub has encrypted blob
  Hub sends ciphertext ──────────────────────────▶ Node B decrypts locally
```

The hub never sees plaintext metadata or content. It's a dumb encrypted relay. This is the same model Signal uses for message relay, applied to filesystem sync.

## Updated Architecture Diagram Entry

The transform pipeline becomes a new layer between the query engine and the physical storage:

```
             ┌─────────────────────────────────────────┐
             │          Transform Pipeline              │
             │                                         │
             │  ┌─────────┐  ┌──────────┐  ┌────────┐ │
             │  │ Content │  │Compress- │  │Encrypt-│ │
             │  │ Hash    │→ │ ion      │→ │ ion    │ │
             │  │ (BLAKE3)│  │ (Zstd/   │  │(AES-GCM│ │
             │  │         │  │  LZ4)    │  │ChaCha) │ │
             │  └─────────┘  └──────────┘  └────────┘ │
             │                                         │
             │  Policy driven by ontology:             │
             │  tag:"video" → skip compression         │
             │  tag:"medical" → separate key scope     │
             │  tag:"scratch" → no encryption          │
             └─────────────────────────────────────────┘
```

Every decision — which algorithm, which key, whether to bother — comes from the ontology evaluating the object's tags. The storage layer is completely generic; it just reads the `TransformMeta` and applies or reverses the transforms. Adding a new compression algorithm or encryption scheme is a configuration change, not a code change.

# HCTR2/XTS update

You're right to call this out. AES-GCM is fundamentally wrong for disk-level encryption. I was thinking about it as a network/message cipher (where it's appropriate for sync traffic) and carelessly applied it to at-rest storage. The problems are real and well-understood.

## Why AES-GCM Fails for Disk Encryption

Three properties that disk encryption requires, which AES-GCM violates:

**1. Length preservation.** AES-GCM expands data by 28 bytes per encrypted unit (12-byte nonce + 16-byte auth tag). If you encrypt a 4 KB sector, you get 4124 bytes. That doesn't fit back in the same sector. You'd waste ~0.7% of disk capacity, and worse, you need somewhere to store the nonces and tags — a separate metadata structure for every block on disk.

**2. In-place rewrite safety.** AES-GCM is CTR mode underneath. If you rewrite a sector with a new nonce, fine. But if you ever reuse a nonce with the same key (crash during write, power loss, wear-leveling on SSD that exposes old state), the XOR of the two ciphertexts reveals the XOR of the two plaintexts. With GCM specifically, nonce reuse also leaks the authentication key — catastrophic, complete break.

**3. Random access without per-sector state.** GCM needs a unique nonce per encryption. That means storing 12 bytes of nonce per sector. For a 1 TB disk with 4 KB sectors, that's 750 MB of nonces. A proper disk cipher derives everything from the sector address and key — no stored state.

```
AES-GCM for disk:                  XTS-AES for disk:

Sector write:                      Sector write:
  need unique nonce (stored where?)   tweak = sector number (free!)
  ciphertext + 16B tag (doesn't fit)  ciphertext = same size as plaintext
  nonce reuse = catastrophic          rewrite same sector = safe (same tweak)
  random access needs nonce lookup    random access = compute tweak from address

Verdict: WRONG for disk              Verdict: DESIGNED for disk
```

## The Right ModesThis is an important correction. The encryption design needs to be split into two fundamentally different regimes based on the nature of the data being encrypted.

## The Two Regimes

The mistake was treating all encryption as the same problem. There are actually two completely different scenarios in this filesystem:

```
REGIME 1: At-rest encryption of blob zones + metadata zones
  ─────────────────────────────────────────────────────────
  Properties needed:
    • Length-preserving (no expansion)
    • No per-sector stored state (nonces, tags)
    • Safe to rewrite same location with different data
    • Random access to individual sectors/blocks
    • Tweak derived from position (sector number)
  
  → This is classical disk encryption
  → XTS-AES or HCTR2

REGIME 2: Message encryption for sync, WAL entries, network traffic
  ─────────────────────────────────────────────────────────
  Properties needed:
    • Authenticated (detect tampering)
    • Each message is unique (append-only log, unique sync ops)
    • Small expansion acceptable (16-28 bytes per message)
    • Nonces are naturally unique (monotonic sequence numbers)
  
  → This is message/stream encryption
  → AES-GCM or ChaCha20-Poly1305 (both correct here)
```

AES-GCM belongs exclusively in regime 2. The previous design wrongly applied it to regime 1.

## Narrow-Block vs Wide-Block

The fundamental choice for at-rest encryption. As the Wikipedia article explains, this determines the granularity at which an attacker can observe changes:

```
XTS-AES (narrow-block):                   HCTR2 (wide-block):

Sector: 4096 bytes = 256 AES blocks       Sector: 4096 bytes = 1 wide block

Change 1 byte of plaintext:               Change 1 byte of plaintext:
┌────┬────┬────┬────┬────┬────┐            ┌────┬────┬────┬────┬────┬────┐
│same│same│DIFF│same│same│same│            │DIFF│DIFF│DIFF│DIFF│DIFF│DIFF│
└────┴────┴────┴────┴────┴────┘            └────┴────┴────┴────┴────┴────┘
 16B blocks                                 entire sector changes

Attacker can see WHICH 16-byte block       Attacker can only see THAT the
within the sector changed.                 sector changed, not where.
```

HCTR2 is a tweakable super-pseudorandom permutation: any change to the plaintext will result in an unrecognizably different ciphertext and vice versa.

The practical question is: does this matter for our filesystem? And what's the performance cost?

### XTS-AES Performance

On a system with AES-NI, AES-XTS with a 256-bit data encryption key achieves approximately 1823 MB/s encryption and 1900 MB/s decryption. That's faster than any single NVMe drive. Essentially zero overhead on the I/O path.

XTS requires double the key size — 512 bits for AES-256 equivalent security — because the XTS standard requires using a different key for the IV encryption than for the block encryption.

### HCTR2 Performance

When measured with real I/O (not the broken cryptsetup benchmark), the performance of HCTR2 is quite similar to that of XTS for block-sized writes. For reads there is still a slight difference of around 20% compared to XTS.

An advantage: HCTR2 does not require twice the key length as XTS does. 128-bit key gives 128-bit security, versus XTS needing 256-bit key for 128-bit security.

### Adiantum/HBSH

The Adiantum scheme used in low-end Android devices specifically chooses NH, 256-bit AES, ChaCha12, and Poly1305. The construction is tweakable and wide-block. It requires three passes over the data, but is still faster than AES-128-XTS on an ARM Cortex-A7 which has no AES instruction set.

This matters for our target: the Raspberry Pi 4 (Cortex-A72) has ARMv8 crypto extensions (AES + PMULL), so XTS and HCTR2 are hardware-accelerated. But if you ever want to run on older ARM boards without crypto extensions, Adiantum is the right fallback.

## The Right Choice for Each Zone

```
              Zone           Mode          Why
              ────           ────          ───
              Blob zone      HCTR2         Wide-block hides internal structure.
              (file content)               Attacker can't see which part of 
                                           a file changed. ~20% slower reads 
                                           but blob reads are I/O-bound anyway.

              Metadata zone  XTS-AES       Object records accessed randomly.
              (object table,               XTS is faster for small random reads.
               location table)             Narrow-block leak is acceptable —
                                           metadata structure is known anyway.

              Index zone     XTS-AES       Bitmaps are read sequentially,
              (tag bitmaps)                decrypted in bulk at boot into RAM.
                                           Speed matters, structure leak doesn't.

              WAL            AES-GCM       Append-only log with monotonic LSN
                                           as nonce. Authenticated — detects
                                           corruption. Small expansion OK
                                           (WAL entries are not length-constrained).

              Sync traffic   AES-GCM or    Network messages, unique per-op.
                             ChaCha20      Authentication required.
                             -Poly1305     GCM if AES-NI, ChaCha otherwise.
```

This is a better design than uniform encryption across everything. Different data patterns get different modes.

## Tweak Construction

The tweak is what makes each sector unique without storing per-sector state. For classic disk encryption it's the sector number. But our filesystem has richer identity:

```rust
// XTS tweak for metadata/index zones:
// Simple sector number — same as dm-crypt plain64
fn xts_tweak_sector(sector_number: u64) -> [u8; 16] {
    let mut tweak = [0u8; 16];
    tweak[0..8].copy_from_slice(&sector_number.to_le_bytes());
    tweak
}

// HCTR2 tweak for blob zone:
// We can use richer tweaks because HCTR2 supports arbitrary tweak sizes.
// Include the object ID so identical content in different objects
// encrypts differently.
fn hctr2_tweak_blob(object_id: u64, sector_within_extent: u64) -> [u8; 32] {
    let mut tweak = [0u8; 32];
    tweak[0..8].copy_from_slice(&object_id.to_le_bytes());
    tweak[8..16].copy_from_slice(&sector_within_extent.to_le_bytes());
    // bytes 16..32 could include epoch/generation for re-encryption
    tweak
}
```

The object-ID-in-tweak trick means that even if two files have identical content at the same sector offset, they produce different ciphertext. With XTS, an adversary who can observe the ciphertext at a given location can later rewrite it, causing it to decrypt to the same plaintext. Including the object ID as part of the tweak narrows this attack — you can only replay a sector to the _same object's same offset_, not to a different object.

## Integrity: The Missing Piece

XTS mode is susceptible to data manipulation and tampering, and applications must employ measures to detect modifications of data if manipulation and tampering is a concern.

Neither XTS nor HCTR2 provide authentication. An attacker with disk access can flip bits, and decryption will produce corrupted plaintext without any error. ZFS solves this with per-block checksums in the Merkle tree. Our filesystem needs the same:

```rust
// Every extent on disk has a checksum stored in the object record
// (which is in a different zone, so attacker must corrupt both)

struct ObjectRecord {
    // ... existing fields ...
    
    // Integrity: BLAKE3 of PLAINTEXT (computed before encryption)
    content_hash: [u8; 32],
    
    // Optional: per-sector checksums for large objects
    // Allows detecting WHICH sector was corrupted
    sector_checksums: ChecksumRef,
}

enum ChecksumRef {
    // Small objects: single hash covers everything
    Whole,
    // Large objects: array of per-sector hashes
    // Stored in a separate extent (like chunk lists)
    PerSector {
        offset: u64,
        count: u32,
    },
}
```

The checksums are computed on plaintext (before encryption) and stored in the metadata zone (which is encrypted under a different key and in a different disk location). An attacker would need to corrupt both the blob zone and the metadata zone consistently, which is much harder than just flipping bits in a sector.

```
WRITE:
  plaintext → BLAKE3 hash → store in object record (metadata zone, XTS-encrypted)
  plaintext → compress → HCTR2 encrypt → store in blob zone

READ:
  blob zone → HCTR2 decrypt → decompress → plaintext
  plaintext → BLAKE3 hash → compare with object record
  mismatch? → corruption detected, try replica or report error
```

This is better than what XTS gives you alone, and doesn't have the overhead of per-sector authenticated encryption (which would need 16 bytes of auth tag per sector — 0.4% space overhead and a separate tag store).

## Revised Key Hierarchy

The key hierarchy changes because different modes need different key types:

```
User Passphrase / Hardware Key
         │
    Argon2id (memory-hard KDF)
         │
         ▼
┌─────────────────────┐
│  Master KEK (256b)  │
│  Key-Encryption Key │
└────────┬────────────┘
         │
    ┌────┴──────────────────────────────────────┐
    │                                           │
    ▼                                           ▼
┌──────────────────┐                   ┌──────────────────┐
│ Disk Keys (KEKs) │                   │ Message Keys     │
│ for at-rest      │                   │ for sync/WAL     │
└──┬───────────────┘                   └──┬───────────────┘
   │                                      │
   ├──▶ XTS-AES-256 key (512 bits!)       ├──▶ AES-GCM-256 key (256b)
   │    for metadata + index zones        │    for WAL entries
   │                                      │
   ├──▶ HCTR2-AES-128 key (128 bits)     └──▶ ChaCha20-Poly1305 key (256b)
   │    for blob zone (pool-wide)              for sync traffic
   │
   └──▶ Per-tag HCTR2 keys (128 bits each)
        derived via HKDF from blob pool key
        HKDF-SHA256(blob_pool_key, tag_id) → per-tag key
```

Note the asymmetry: XTS needs 512 bits of key for AES-256 equivalent security (two independent 256-bit keys). HCTR2 needs only 128 bits for 128-bit security. This is a genuine practical advantage of HCTR2.

## Platform-Adaptive Algorithm Selection

```rust
enum DiskCipher {
    // Primary: hardware AES + CLMUL available (x86 with AES-NI, ARMv8 CE)
    // This covers RPi 4/5, all modern x86, Apple Silicon
    Hctr2Aes128,        // blob zone: wide-block
    XtsAes256,          // metadata/index: narrow-block, fast random access
    
    // Fallback: no hardware AES (old ARM, RISC-V without crypto ext)
    Adiantum,           // wide-block using ChaCha12 — fast without AES-NI
    
    // Message encryption (sync, WAL) — always one of these
    Aes256Gcm,          // if AES-NI available
    ChaCha20Poly1305,   // otherwise (or always for sync — constant-time)
}

fn select_disk_cipher() -> (DiskCipher, DiskCipher) {
    let has_aes_ni = detect_aes_hardware();
    let has_clmul = detect_clmul_hardware();
    
    let blob_cipher = if has_aes_ni && has_clmul {
        DiskCipher::Hctr2Aes128      // best: wide-block, hw accelerated
    } else {
        DiskCipher::Adiantum          // fallback: wide-block, sw fast
    };
    
    let meta_cipher = if has_aes_ni {
        DiskCipher::XtsAes256         // fast random access reads
    } else {
        DiskCipher::Adiantum          // Adiantum is also fine for metadata
    };
    
    (blob_cipher, meta_cipher)
}
```

## Interaction with Compression

The transform pipeline ordering is now more precise:

```
WRITE PATH:

  Plaintext data
       │
       ├─── content_hash = BLAKE3(plaintext)     ← integrity reference
       │
       ▼
  ┌─────────────┐
  │ Compression │  Ontology-driven: skip for JPEG/MP3/video
  │ (Zstd/LZ4)  │  Result: may be smaller, may be same size
  └──────┬──────┘
         │
         ▼
  ┌─────────────────────┐
  │ Sector alignment    │  Pad compressed output to sector boundary
  │                     │  (XTS/HCTR2 operate on full sectors)
  └──────┬──────────────┘
         │
         ▼
  ┌─────────────────────┐
  │ Disk encryption     │  HCTR2 for blobs, XTS for metadata
  │ (length-preserving) │  Tweak = object_id + sector_offset
  │                     │  No expansion, no stored nonces
  └──────┬──────────────┘
         │
         ▼
  Raw ciphertext on disk (same size as padded input)
```

The sector alignment step is important. XTS and HCTR2 operate on sector-sized units (typically 4096 bytes). If compression produces 5000 bytes, you need to pad to 8192 bytes (2 sectors) before encrypting. The `stored_size` in the object record reflects the padded/encrypted size, while `compressed_size` reflects the actual data length within those sectors.

```rust
fn write_encrypted_extent(
    data: &[u8],                // compressed (or raw) data
    cipher: &DiskCipher,
    key: &[u8],
    object_id: u64,
    disk: &mut DiskWriter,
) -> Result<ExtentRef> {
    let sector_size = 4096u64;
    
    // Pad to sector boundary
    let padded_len = (data.len() as u64 + sector_size - 1) & !(sector_size - 1);
    let mut padded = vec![0u8; padded_len as usize];
    padded[..data.len()].copy_from_slice(data);
    // Remaining bytes are zero-padded (encrypted zeros are indistinguishable)
    
    // Allocate contiguous extent
    let extent = disk.allocate(padded_len)?;
    
    // Encrypt sector by sector
    for (i, sector) in padded.chunks_mut(sector_size as usize).enumerate() {
        let tweak = make_tweak(object_id, i as u64);
        
        match cipher {
            DiskCipher::Hctr2Aes128 => {
                hctr2_encrypt_in_place(key, &tweak, sector);
            }
            DiskCipher::XtsAes256 => {
                xts_encrypt_in_place(key, &tweak, sector);
            }
            DiskCipher::Adiantum => {
                adiantum_encrypt_in_place(key, &tweak, sector);
            }
            _ => unreachable!("not a disk cipher"),
        }
    }
    
    disk.write_extent(&extent, &padded)?;
    
    Ok(ExtentRef {
        offset: extent.offset,
        stored_sectors: (padded_len / sector_size) as u32,
        data_length: data.len() as u32,  // actual data within sectors
    })
}
```

## Corrected Encryption Summary

```
┌──────────────────────────────────────────────────────────────────┐
│                     ENCRYPTION ARCHITECTURE                      │
├──────────────┬───────────────────┬───────────────────────────────┤
│ Component    │ Mode              │ Rationale                     │
├──────────────┼───────────────────┼───────────────────────────────┤
│ Blob zone    │ HCTR2-AES-128     │ Wide-block: entire sector     │
│ (file data)  │ tweak: obj+sector │ changes on any edit.          │
│              │                   │ Hides internal structure.     │
│              │                   │ 128b key = 128b security.     │
├──────────────┼───────────────────┼───────────────────────────────┤
│ Metadata     │ XTS-AES-256       │ Fast random-access reads.     │
│ zone         │ tweak: sector     │ Narrow-block leak acceptable  │
│              │                   │ for structured metadata.      │
│              │                   │ 512b key = 256b security.     │
├──────────────┼───────────────────┼───────────────────────────────┤
│ Index zone   │ XTS-AES-256       │ Bulk decrypt at boot.         │
│ (bitmaps)    │ tweak: sector     │ Speed > security granularity. │
├──────────────┼───────────────────┼───────────────────────────────┤
│ WAL          │ AES-256-GCM       │ Append-only: monotonic LSN    │
│              │ nonce: LSN        │ as nonce. Authentication      │
│              │                   │ detects corruption.           │
├──────────────┼───────────────────┼───────────────────────────────┤
│ Sync traffic │ ChaCha20-Poly1305 │ Network messages. Unique      │
│              │ nonce: HLC+seq    │ nonces from hybrid timestamps.│
│              │                   │ Constant-time (side-channel   │
│              │                   │ safe on all platforms).       │
├──────────────┼───────────────────┼───────────────────────────────┤
│ Integrity    │ BLAKE3 checksums  │ Stored in metadata zone       │
│ (all zones)  │ on plaintext      │ (different key). Detects      │
│              │                   │ tampering that XTS/HCTR2      │
│              │                   │ cannot.                       │
├──────────────┼───────────────────┼───────────────────────────────┤
│ Fallback     │ Adiantum          │ Wide-block via ChaCha12.      │
│ (no AES-NI)  │ tweak: sector     │ For ARM without crypto ext.   │
└──────────────┴───────────────────┴───────────────────────────────┘
```

The filesystem knows which mode to apply where because it controls the entire I/O stack — something dm-crypt can't do (it sees only sector reads and writes, not "this is a tag bitmap" versus "this is a video frame"). That structural knowledge lets you apply exactly the right cryptographic mode for each data type.
