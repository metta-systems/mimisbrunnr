# High-level concepts

Superblocks
Object Records
Object Index
Object Locators (extents and replicas)
Write-Ahead Log

Disk layout/selection - ?
Disk storage layers - DiskDescriptors + Hot, Warm, Cold, Glacier + Online/Draining/Removed/Faulted
Semantic placement rules (tag based)

Hashing, Compression + Encryption with Trust Tiers

Selective FastCDC chunking for types selected by tags (large modifiable structures like VM images).
"When active: chunk plaintext, hash each chunk, compress per-chunk, encrypt." -- file extents record is "chunked" perhaps

Erasure Coding for cold and glacier data (better redundancy at lower overhead).



seaweedfs.topology.VolumeLayout - descriptors of what is where in the volume set
- VolumeServer → Store → DiskLocation → Volume → Needle

Erasure Coding:

- Convert volumes to shards
- Default: 10 data + 4 parity shards (can withstand 4 failures)
- Reduces storage cost by ~40% vs. 2x replication
- Reed-Solomon encoding
- Rebuild missing shards


## Object Record

Every file is an **object** with a globally unique ID and a bag of assertions about it. Objects have no intrinsic name — "name" is just another attribute. (Id's are never recycled).

## Object Index

Bag of assertions format:

enum Assertion {
    Tag(TagId),                                      // "this is electronic music"
    Attr { key: TagId, value: Value },               // "artist = Aphex Twin"
    Relation { predicate: TagId, target: ObjectId }, // "member_of playlist:900"
}

enum Value {
    Text(String),
    Int(i64),
    Float(f64),
    Timestamp(i64),
    Blob(Vec<u8>),
}

## Indices

Tag Inverted Index

Roading Bitmaps + Ordered + Ranked

KV Equality Index - ?

Range B+ Tree - ?

Forward Index
