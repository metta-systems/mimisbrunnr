//! Compile-time size assertions mirroring the format specification (doc/agentic/format-design.md and the scope design documents it lists).
//! Each `const _` assertion fails to compile if the size is wrong.

#![allow(dead_code)]

use std::mem::size_of;

// =====================================================================
// Symbolic flag constants — mirrors FORMAT-DESIGN§1.3, BTREE-DESIGN§1.1, BTREE-DESIGN§1.6, JOURNAL-DESIGN§1.1, JOURNAL-DESIGN§1.2,
// METADATA-DESIGN§1.1, METADATA-DESIGN§2.1, INDEX-DESIGN§2.1, UNIX-DESIGN§1, SNAPSHOTS-DESIGN§1.1, ALLOCATION-DESIGN§1.2 (BucketAllocEntry), RECONCILE-DESIGN§1.2.
// Asserted-by-existence: any code referring to these names compiles iff
// the constants stay defined here.
// =====================================================================

// BlockHeader.flags
pub const BLOCK_FLAG_ENCRYPTED:    u32 = 1 << 0;
pub const BLOCK_FLAG_CONTINUATION: u32 = 1 << 1;

// =====================================================================
// FORMAT-DESIGN§1.3 BlockKind / BtreeKind discriminants — pinned numerically.
// Reordering or renumbering is a format break. Mirrors the doc tables
// verbatim; any divergence between this list and the engine's enum
// definitions is caught at integration time. Variants are grouped here
// by functional role for readability — the numeric values are stable.
// =====================================================================

// BlockKind (BlockHeader.kind, magic = "MIMR")
// Filesystem identity, on-disk geometry, and pool composition
pub const BLOCK_KIND_SUPERBLOCK:        u16 = 0;
pub const BLOCK_KIND_ZONE_MAP:          u16 = 1;
pub const BLOCK_KIND_POOL_STATE_ROOT:   u16 = 2;
// WAL framing
pub const BLOCK_KIND_WAL_SEGMENT:       u16 = 3;
pub const BLOCK_KIND_CHECKPOINT:        u16 = 4;
// Tag-store pages
pub const BLOCK_KIND_TAG_BITMAP_PAGE:   u16 = 5;
pub const BLOCK_KIND_SEQUENCE_PAGE:     u16 = 6;   // INDEX-DESIGN§3.3 OrderedStore page
pub const BLOCK_KIND_RANKED_PAGE:       u16 = 7;   // INDEX-DESIGN§3.3 RankedStore page
// KV equality pages
pub const BLOCK_KIND_KV_HASH_DIRECTORY: u16 = 8;
pub const BLOCK_KIND_KV_HASH_BUCKET:    u16 = 9;
// Per-object metadata overflow
pub const BLOCK_KIND_OVERFLOW_RECORD:   u16 = 10;
pub const BLOCK_KIND_COUNT:             u16 = 11;
// Sanity: every BlockKind must be unique and densely numbered from 0.
const _: () = assert!(BLOCK_KIND_OVERFLOW_RECORD + 1 == BLOCK_KIND_COUNT);

// BtreeKind (Preamble.kind of a "BREG" region), FORMAT-DESIGN§1.3 — contiguous 0–26
// Object metadata
pub const BTREE_KIND_OBJECT_ROOTS:            u16 = 0;
pub const BTREE_KIND_OBJECT_TABLE:            u16 = 1;
pub const BTREE_KIND_OBJECT_HISTORY:          u16 = 2;
pub const BTREE_KIND_CONTENT_MAP:             u16 = 3;
// Term index
pub const BTREE_KIND_FORWARD:                 u16 = 4;
pub const BTREE_KIND_FORWARD_OVERFLOW:        u16 = 5;
pub const BTREE_KIND_TERM_DICTIONARY:         u16 = 6;
pub const BTREE_KIND_TERM_TABLE:              u16 = 7;
pub const BTREE_KIND_POSTINGS:                u16 = 8;
pub const BTREE_KIND_SEQUENCES:               u16 = 9;
// Deduplication
pub const BTREE_KIND_CHUNK_INDEX:             u16 = 10;
// Physical space
pub const BTREE_KIND_BACKPOINTER:             u16 = 11;
pub const BTREE_KIND_BUCKET_ALLOC:            u16 = 12;
pub const BTREE_KIND_FREESPACE_LRU:           u16 = 13;
// Ontology and snapshots
pub const BTREE_KIND_ONTOLOGY:                u16 = 14;
pub const BTREE_KIND_SNAPSHOTS:               u16 = 15;
// Subscriptions
pub const BTREE_KIND_SUBSCRIPTIONS:           u16 = 16;
pub const BTREE_KIND_SUBSCRIPTION_MEMBERS:    u16 = 17;
pub const BTREE_KIND_SUBSCRIPTION_EVENTS:     u16 = 18;
// Pool and cluster
pub const BTREE_KIND_POOL_STATE:              u16 = 19;
pub const BTREE_KIND_DISK_DESCRIPTORS:        u16 = 20;
pub const BTREE_KIND_PLACEMENT_RULES:         u16 = 21;
pub const BTREE_KIND_CLUSTER_PEERS:           u16 = 22;
pub const BTREE_KIND_TRANSFERS:               u16 = 23;
// Reconcile
pub const BTREE_KIND_RECONCILE_WORK:          u16 = 24;
pub const BTREE_KIND_RECONCILE_PENDING:       u16 = 25;
pub const BTREE_KIND_RECONCILE_SCAN:          u16 = 26;
pub const BTREE_KIND_COUNT:                   u16 = 27;
const _: () = assert!(BTREE_KIND_RECONCILE_SCAN + 1 == BTREE_KIND_COUNT);

// SNAPSHOTS-DESIGN§1.2 — the snapshot-aware btrees (the yes-rows of its table), which
// one snapshot cleanup pass walks (SNAPSHOTS-DESIGN§1.5).
pub const SNAPSHOT_AWARE_BTREE_COUNT: usize = 7;
const _: () = {
    // Enumerate them so additions to BtreeKind force a review here.
    let snapshot_aware: [u16; SNAPSHOT_AWARE_BTREE_COUNT] = [
        BTREE_KIND_FORWARD,
        BTREE_KIND_TERM_DICTIONARY,
        BTREE_KIND_POSTINGS,
        BTREE_KIND_SEQUENCES,
        BTREE_KIND_CONTENT_MAP,
        BTREE_KIND_ONTOLOGY,
        BTREE_KIND_OBJECT_HISTORY,    // sidecar of the object table
    ];
    let _ = snapshot_aware;
};

// FORMAT-DESIGN§1.3 Preamble — 8 B, first field of every persistent header
#[repr(C)]
pub struct Preamble {
    pub magic: u32,     // [0..4] little-endian ASCII
    pub version: u16,   // [4..6]
    pub kind: u16,      // [6..8] BtreeKind for a region, else 0
}
const _: () = assert!(size_of::<Preamble>() == 8);
const _: () = assert!(std::mem::offset_of!(Preamble, version) == 4);
const _: () = assert!(std::mem::offset_of!(Preamble, kind) == 6);
pub const MAGIC_SUPERBLOCK:     u32 = u32::from_le_bytes(*b"MIMR");
pub const MAGIC_REGION:         u32 = u32::from_le_bytes(*b"BREG");
pub const MAGIC_RUN_INDEX:      u32 = u32::from_le_bytes(*b"BIDX");
pub const MAGIC_PAGE_RUN_INDEX: u32 = u32::from_le_bytes(*b"BPGS");
pub const MAGIC_JOURNAL_SET:    u32 = u32::from_le_bytes(*b"JRNL");

// BtreeNodeHeader.flags (BTREE-DESIGN§1.1; bit 0 unassigned)
pub const BTREE_NODE_FLAG_HEAD_OF_CHAIN: u8 = 1 << 1;  // first region of a positional chain

// FieldFormat.flags (BTREE-DESIGN§1.6)
pub const FIELD_FORMAT_FLAG_SIGNED:    u8 = 1 << 0;
pub const FIELD_FORMAT_FLAG_MSB_FIRST: u8 = 1 << 1;

// WalEntryHeader.flags
pub const WAL_ENTRY_FLAG_ENCRYPTED:  u16 = 1 << 0;
pub const WAL_ENTRY_FLAG_COMPRESSED: u16 = 1 << 1;

// ObjectRecord.flags
pub const OBJECT_FLAG_HAS_OVERFLOW: u8 = 1 << 0;
pub const OBJECT_FLAG_CHUNKED:      u8 = 1 << 1;

// ObjectLocation.flags
pub const LOCATION_FLAG_CHUNKED:     u8 = 1 << 0;
pub const LOCATION_FLAG_REMOTE_ONLY: u8 = 1 << 1;

// LeafEntry.header (INDEX-DESIGN§2.1)
pub const LEAF_ENTRY_SPILL_FLAG: u16 = 1 << 15;
pub const LEAF_ENTRY_TOTAL_MASK: u16 = 0x7FFF;
const _: () = assert!(LEAF_ENTRY_SPILL_FLAG | LEAF_ENTRY_TOTAL_MASK == 0xFFFF);
const _: () = assert!(LEAF_ENTRY_SPILL_FLAG & LEAF_ENTRY_TOTAL_MASK == 0);

// SNAPSHOTS-DESIGN§1.3 — whiteout (snapshot tombstone) sentinel encodings, per leaf shape.
// The snapshot-aware leaves don't share a common discriminator slot; the
// whiteout encoding is per leaf shape.
//
//   Forward (INDEX-DESIGN§2.1):  LeafEntry.header == LEAF_ENTRY_SPILL_FLAG  (spill bit set,
//                    total = 0). Real spills imply total > 8, so total = 0 in
//                    spill mode is the unambiguous tombstone.
//   Range (INDEX-DESIGN§4.2):    leaf BlockRef value zeroed (disk_id = sector = bucket_no =
//                    generation = 0) — never a valid bitmap pointer.
//   TagDirectory (INDEX-DESIGN§3.1): TagIndexLeafEntry.store_kind == STORE_KIND_WHITEOUT
//                    with store_root zeroed.
//   Ontology, Subscriptions (ONTOLOGY-DESIGN§1, SUBSCRIPTIONS-DESIGN§1): CBOR null (0xF6) at the value
//                    position, in place of the usual record.
//   ObjectHistory, LocationHistory (SNAPSHOTS-DESIGN§1.2): a 1-byte discriminator precedes
//                    the shadowed record; 0xFF == KEY_TYPE_WHITEOUT means
//                    whiteout, otherwise the record follows.
pub const FORWARD_WHITEOUT_HEADER:  u16 = LEAF_ENTRY_SPILL_FLAG;            // total = 0, spill set
pub const STORE_KIND_WHITEOUT:      u8  = 0xFF;                             // INDEX-DESIGN§3.1 TagDirectory
pub const CBOR_NULL_BYTE:           u8  = 0xF6;                             // ONTOLOGY-DESIGN§1, SUBSCRIPTIONS-DESIGN§1
pub const KEY_TYPE_WHITEOUT:        u8  = 0xFF;                             // SNAPSHOTS-DESIGN§1.2 history sidecars
// Forward whiteout: header has the spill bit set with total = 0.
const _: () = assert!(FORWARD_WHITEOUT_HEADER & LEAF_ENTRY_TOTAL_MASK == 0);
const _: () = assert!(FORWARD_WHITEOUT_HEADER & LEAF_ENTRY_SPILL_FLAG != 0);
// STORE_KIND_WHITEOUT must not collide with a real store_kind (Simple,
// Ordered, Ranked are 0/1/2 by INDEX-DESIGN§3.3 prose; reserve 0xFF).
pub const STORE_KIND_SIMPLE:  u8 = 0;
pub const STORE_KIND_ORDERED: u8 = 1;
pub const STORE_KIND_RANKED:  u8 = 2;
const _: () = assert!(STORE_KIND_WHITEOUT != STORE_KIND_SIMPLE);
const _: () = assert!(STORE_KIND_WHITEOUT != STORE_KIND_ORDERED);
const _: () = assert!(STORE_KIND_WHITEOUT != STORE_KIND_RANKED);

// SnapshotNode.flags
pub const SNAPSHOT_FLAG_LEAF:    u8 = 1 << 0;
pub const SNAPSHOT_FLAG_DELETED: u8 = 1 << 1;

// BucketAllocEntry.kind_flags: low nibble BucketKind (0–7 assigned), high nibble flags
pub const BUCKET_FLAG_NEEDS_DISCARD: u8 = 1 << 4;
pub const BUCKET_KIND_BAD:           u8 = 7;
const _: () = assert!(BUCKET_FLAG_NEEDS_DISCARD & 0x0f == 0);

// WorkItem.flags
pub const WORK_FLAG_RATELIMITED: u32 = 1 << 0;
pub const WORK_FLAG_PERSISTENT:  u32 = 1 << 1;

// =====================================================================
// ENGINE-DESIGN§1.2 BlockRef — 16 B, bucket-relative
// =====================================================================
#[repr(C)]
pub struct BlockRef {
    pub disk_id: u16,        //  [0..2]
    pub sector: u16,         //  [2..4]   4 KiB sector within the bucket
    pub bucket_no: u32,      //  [4..8]
    pub generation: u32,     //  [8..12]  bucket generation at write time (ALLOCATION-DESIGN§1.3)
    pub _reserved: u32,      // [12..16]  zero
}
const _: () = assert!(size_of::<BlockRef>() == 16);
const _: () = assert!(std::mem::align_of::<BlockRef>() == 4);
const _: () = assert!(std::mem::offset_of!(BlockRef, disk_id) == 0);
const _: () = assert!(std::mem::offset_of!(BlockRef, sector) == 2);
const _: () = assert!(std::mem::offset_of!(BlockRef, bucket_no) == 4);
const _: () = assert!(std::mem::offset_of!(BlockRef, generation) == 8);
const _: () = assert!(std::mem::offset_of!(BlockRef, _reserved) == 12);
// A sector of the largest (256 MiB, 2^28) bucket fits `sector: u16`.
const _: () = assert!((1u64 << 28) / 4096 - 1 == u16::MAX as u64);
// Bucket 2^32 − 1 of the largest bucket size stays inside u64.
const _: () = assert!(((u32::MAX as u64) << 28) + (u16::MAX as u64) * 4096 < u64::MAX);

// ENGINE-DESIGN§1.2 NodePtr — R BlockRefs (R = metadata_replicas, 1..=4), 16 × R bytes.
pub const MAX_METADATA_REPLICAS: usize = 4;
pub const fn node_ptr_size(replicas: usize) -> usize {
    size_of::<BlockRef>() * replicas
}
const _: () = assert!(node_ptr_size(1) == 16);
const _: () = assert!(node_ptr_size(2) == 32);
const _: () = assert!(node_ptr_size(MAX_METADATA_REPLICAS) == 64);

// =====================================================================
// FORMAT-DESIGN§1.3 BlockPreamble — 8 B (shared by BlockHeader and BtreeNodeHeader)
// =====================================================================
#[repr(C)]
pub struct BlockPreamble {
    pub magic: [u8; 4],          // "MIMR" (4 KiB block) or "MIMB" (256 KiB region)
    pub kind: u16,               // BlockKind or BtreeKind, scoped by magic
    pub format_version: u16,
}
const _: () = assert!(size_of::<BlockPreamble>() == 8);

// =====================================================================
// FORMAT-DESIGN§1.3 BlockHeader — 32 B
// =====================================================================
#[repr(C)]
pub struct BlockHeader {
    pub pre: BlockPreamble,
    pub payload_length: u32,
    pub generation: [u8; 8],     // little-endian u64 at offset 12
    pub lsn: [u8; 8],            // little-endian u64 at offset 20
    pub flags: u32,
}
const _: () = assert!(size_of::<BlockHeader>() == 32);

// =====================================================================
// BTREE-DESIGN§1.1–1.2, §1.6 — write-once regions (crates/storage/src/region.rs)
// =====================================================================
#[repr(C)]
pub struct RegionNodeHeader {
    pub pre: Preamble,                // [0..8]   "BREG", kind ∈ BtreeKind
    pub node_seq: u64,                // [8..16]
    pub created_lsn: u64,             // [16..24]
    pub region_size_log2: u8,         // [24..25]
    pub level: u8,                    // [25..26]
    pub layout: u8,                   // [26..27] 0 runs, 1 positional pages
    pub flags: u8,                    // [27..28]
    pub logical_pages: u16,           // [28..30]
    pub kind_header_len: u16,         // [30..32]
    pub _reserved: [u8; 24],          // [32..56]
    pub checksum: u64,                // [56..64]
}
const _: () = assert!(size_of::<RegionNodeHeader>() == 64);
const _: () = assert!(std::mem::offset_of!(RegionNodeHeader, checksum) == 56);

#[repr(C)]
pub struct RunHeader {
    pub pre: Preamble,                // [0..8]   "BIDX", kind 0
    pub seq: u32,                     // [8..12]
    pub block_count: u16,             // [12..14]
    pub flags: u16,                   // [14..16]
    pub journal_seq: u64,             // [16..24]
    pub entry_count: u32,             // [24..28]
    pub region_tag: u32,              // [28..32] low 32 bits of block 0's checksum
    pub checksum: u64,                // [32..40]
}
const _: () = assert!(size_of::<RunHeader>() == 40);
const _: () = assert!(std::mem::offset_of!(RunHeader, checksum) == 32);

#[repr(C)]
pub struct KeyFormatHead {
    pub nr_fields: u8,
    pub key_width: u8,
    pub value_kind: u8,
    pub _pad: u8,
    pub fixed_value_len: u32,
}
const _: () = assert!(size_of::<KeyFormatHead>() == 8);

#[repr(C)]
pub struct RunFieldFormat {
    pub width: u8,                    // 0, 1, 2, 4 or 8 bytes
    pub flags: u8,
    pub _pad: [u8; 6],
    pub base: u64,
}
const _: () = assert!(size_of::<RunFieldFormat>() == 16);

#[repr(C)]
pub struct RunBlockEntry {
    pub checksum: u64,                // [0..8]
    pub entry_count: u16,             // [8..10]
    pub flags: u16,                   // [10..12]
    pub min_snapshot: u32,            // [12..16]
    pub max_snapshot: u32,            // [16..20]
    pub _pad: u32,                    // [20..24]
}
const _: () = assert!(size_of::<RunBlockEntry>() == 24);

#[repr(C)]
pub struct DataBlockHead {
    pub entry_count: u16,
    pub value_area: u16,
    pub flags: u16,
    pub _reserved: u16,
}
const _: () = assert!(size_of::<DataBlockHead>() == 8);

#[repr(C)]
pub struct LargeValueRef {
    pub first_block: u16,
    pub block_count: u16,
    pub length: u32,
}
const _: () = assert!(size_of::<LargeValueRef>() == 8);

// BTREE-DESIGN§1.7 positional regions (crates/storage/src/page_region.rs)
#[repr(C)]
pub struct PageRunHeader {
    pub pre: Preamble,                // [0..8]   "BPGS", kind 0
    pub seq: u32,                     // [8..12]
    pub page_count: u16,              // [12..14]
    pub _reserved: u16,               // [14..16]
    pub journal_seq: u64,             // [16..24]
    pub region_tag: u32,              // [24..28]
    pub _pad: u32,                    // [28..32]
    pub checksum: u64,                // [32..40]
}
const _: () = assert!(size_of::<PageRunHeader>() == 40);
const _: () = assert!(std::mem::offset_of!(PageRunHeader, region_tag) == 24);

#[repr(C)]
pub struct PageRunEntry {
    pub page_no: u16,
    pub _pad: [u8; 6],
    pub checksum: u64,
}
const _: () = assert!(size_of::<PageRunEntry>() == 16);
// logical_pages = ¾ of a 256 KiB region's 64 blocks.
pub const POSITIONAL_LOGICAL_PAGES_256_KIB: usize = 64 * 3 / 4;
const _: () = assert!(POSITIONAL_LOGICAL_PAGES_256_KIB == 48);

pub const RUN_FLAG_ENCRYPTED:      u16 = 1 << 0;
pub const RUN_BLOCK_FLAG_VALUE:    u16 = 1 << 0;
pub const RUN_ENTRY_FLAG_WHITEOUT: u8 = 1 << 0;
pub const RUN_ENTRY_FLAG_LARGE:    u8 = 1 << 1;

// =====================================================================
// Legacy region header, still used by the forward-overflow chain (TODO(INDEX14)).
// =====================================================================
#[repr(C)]
pub struct BtreeNodeHeader {
    pub pre: BlockPreamble,
    pub seq: u64,
    pub last_persisted_lsn: u64,
    pub region_size_log2: u8,
    pub level: u8,
    pub sorted_run_count: u8,
    pub flags: u8,
    pub payload_used: u32,
    pub min_key: [u8; 16],
    pub max_key: [u8; 16],
}
const _: () = assert!(size_of::<BtreeNodeHeader>() == 64);
// First 8 bytes of each header structure are the shared BlockPreamble:
const _: () = assert!(std::mem::offset_of!(BlockHeader, pre) == 0);
const _: () = assert!(std::mem::offset_of!(BtreeNodeHeader, pre) == 0);


// =====================================================================
// ENGINE-DESIGN§1.1 ZoneExtent — 24 B
// =====================================================================
#[repr(C)]
pub struct ZoneExtent {
    pub offset: u64,
    pub length: u64,
    pub flags: u32,
    pub _pad: u32,
}
const _: () = assert!(size_of::<ZoneExtent>() == 24);

// =====================================================================
// ENGINE-DESIGN§1.1 ZoneMap (4 KiB block) and ZoneMapEntry (32 B)
// ZoneMapEntry embeds the full 24 B ZoneExtent so each entry carries
// `flags` for future per-extent hints.
// =====================================================================
#[repr(C)]
pub struct ZoneMapEntry {
    pub zone_kind: u8,                  //  [0..1]
    pub _pad: [u8; 7],                  //  [1..8]   align embedded extent to u64
    pub extent: ZoneExtent,             //  [8..32]  full 24 B form (offset, length, flags, _pad)
}
const _: () = assert!(size_of::<ZoneMapEntry>() == 32);

#[repr(C)]
pub struct ZoneMap {
    pub header: BlockHeader,            //   32
    pub extent_count: u16,              //    2
    pub _pad: [u8; 6],                  //    6
    pub extents: [ZoneMapEntry; 126],   // 4032 (126 × 32)
    pub _pad_tail: [u8; 20],            //   20
    pub trailing_crc: u32,              //    4
}
const _: () = assert!(size_of::<ZoneMap>() == 4096);

// =====================================================================
// ENGINE-DESIGN§1.2 RootPointer — 408 B
// Anchors every persistent btree root atomically (one COW commit flips
// the active superblock root; every tree advances together).
// =====================================================================
#[repr(C)]
pub struct RootPointer {
    pub seq: u64,                                // 8
    pub lsn: u64,                                // 8
    pub object_table_root: BlockRef,             // 16
    pub object_history_root: BlockRef,
    pub location_table_root: BlockRef,
    pub location_history_root: BlockRef,
    pub forward_index_root: BlockRef,
    pub tag_index_root: BlockRef,
    pub kv_index_root: BlockRef,
    pub range_index_root: BlockRef,
    pub chunk_index_root: BlockRef,
    pub value_spill_root: BlockRef,
    pub backpointer_root: BlockRef,
    pub ontology_root: BlockRef,
    pub subscriptions_root: BlockRef,
    pub pool_state_root: BlockRef,
    pub snapshot_chain_root: BlockRef,
    pub reconcile_work_root: BlockRef,
    pub reconcile_high_prio_root: BlockRef,
    pub reconcile_work_phys_root: BlockRef,
    pub reconcile_high_prio_phys_root: BlockRef,
    pub reconcile_pending_root: BlockRef,
    pub reconcile_scan_root: BlockRef,
    pub disks_overflow_root: BlockRef,           // POOL-DESIGN§1 — populated iff disk_count > 12
    pub placement_rules_root: BlockRef,          // POOL-DESIGN§1 — placement rules btree
    pub cluster_peers_root: BlockRef,            // POOL-DESIGN§1 — cluster peers btree
    pub flags: u32,
    pub crc: u32,
}
const _: () = assert!(size_of::<RootPointer>() == 408);

// =====================================================================
// ENGINE-DESIGN§1.1 Superblock — 4096 B
// Two alternating RootPointer slots (root_a, root_b) for atomic commit;
// the trailing _reserved region absorbs whatever space remains after
// the static layout fields.
// =====================================================================
#[repr(C)]
pub struct Superblock {
    pub header: BlockHeader,                    // [0..32]
    pub magic_full: [u8; 16],                   // [32..48]
    pub fs_uuid: [u8; 16],                      // [48..64]
    pub node_id: u16,                           // [64..66]
    pub disk_id: u16,                           // [66..68]
    pub media_type: u8,                         // [68..69]
    pub tier: u8,                               // [69..70]
    pub _pad0: [u8; 2],                         // [70..72]
    pub device_capacity: u64,                   // [72..80]
    pub block_size_log2: u8,                    // [80..81]
    pub _pad1: [u8; 7],                         // [81..88]
    pub creation_timestamp_ns: i64,             // [88..96]
    pub last_mount_timestamp_ns: i64,           // [96..104]
    pub mount_count: u64,                       // [104..112]
    pub root_a: RootPointer,                    // [112..520]
    pub root_b: RootPointer,                    // [520..928]
    pub active_root: u8,                        // [928..929]
    pub _pad2: [u8; 7],                         // [929..936]
    pub wal_offset: u64,                        // [936..944]
    pub wal_size: u64,                          // [944..952]
    pub bucket_size_log2: u8,                   // [952..953]
    pub copygc_reserve_pct: u8,                 // [953..954]
    pub btree_node_size_log2: u8,               // [954..955]
    pub _pad3: [u8; 5],                         // [955..960]
    pub bootstrap_buckets: u32,                 // [960..964]
    pub _pad4: [u8; 4],                         // [964..968]
    pub zone_map_offset: u64,                   // [968..976]
    pub index_zone: ZoneExtent,                 // [976..1000]
    pub metadata_zone: ZoneExtent,              // [1000..1024]
    pub blob_zone: ZoneExtent,                  // [1024..1048]
    pub encryption_keyid: [u8; 16],             // [1048..1064]
    pub fs_format_version: u32,                 // [1064..1068]
    pub fs_min_on_disk: u32,                    // [1068..1072]
    pub compat_features: u64,                   // [1072..1080]
    pub ro_compat_features: u64,                // [1080..1088]
    pub incompat_features: u64,                 // [1088..1096]
    pub downgrade_log_ref: BlockRef,            // [1096..1112]
    // SPECIFICATION-DESIGN§3.5 / SPECIFICATION-DESIGN§8.3 default storage policy
    pub default_chunking_threshold: u64,        // [1112..1120]
    pub default_chunking: ChunkParamsRecord,    // [1120..1136]
    pub _reserved: [u8; 2956],                  // [1136..4092]
    pub trailing_crc: u32,                      // [4092..4096]
}
const _: () = assert!(size_of::<Superblock>() == 4096);

// =====================================================================
// JOURNAL-DESIGN§1.1 WalHeader — 4096 B (one block; A/B-alternated using
// BlockHeader.generation, no separate seq field)
// =====================================================================
#[repr(C)]
pub struct WalHeader {
    pub header: BlockHeader,            //   32
    pub next_lsn: u64,                  //    8
    pub write_cursor: u64,              //    8
    pub read_cursor: u64,               //    8
    pub used_bytes: u64,                //    8
    pub last_checkpoint_lsn: u64,       //    8  newest Checkpoint entry's LSN;
                                        //       no separate physical pointer — recovery
                                        //       scans forward from this LSN.
    pub segment_size: u32,              //    4
    pub _pad: u32,                      //    4
    pub encryption_keyid: [u8; 16],     //   16
    pub _reserved: [u8; 3996],          // 3996
    pub trailing_crc: u32,              //    4
}
const _: () = assert!(size_of::<WalHeader>() == 4096);

// =====================================================================
// JOURNAL-DESIGN§1.2 WalEntryHeader — 40 B
// HybridTimestamp uses ns resolution (matches every other *_ns field on disk)
// and u16 node_id (matches Superblock.node_id). Sort order: physical_ns →
// logical → node_id (SPECIFICATION-DESIGN§10.4).
// =====================================================================
#[repr(C)]
pub struct HybridTimestamp {
    pub physical_ns: i64,    // [0..8]   monotonic wall-clock nanoseconds
    pub logical: u16,        // [8..10]  same-tick disambiguation
    pub node_id: u16,        // [10..12] originating node (matches Superblock.node_id)
    pub _pad: u32,           // [12..16] tail pad to multiple-of-8
}
const _: () = assert!(size_of::<HybridTimestamp>() == 16);
const _: () = assert!(std::mem::offset_of!(HybridTimestamp, physical_ns) == 0);
const _: () = assert!(std::mem::offset_of!(HybridTimestamp, logical) == 8);
const _: () = assert!(std::mem::offset_of!(HybridTimestamp, node_id) == 10);

#[repr(C)]
pub struct WalEntryHeader {
    pub magic: u32,
    pub op_kind: u8,
    pub format_version: u8,
    pub flags: u16,
    pub lsn: u64,
    pub timestamp: HybridTimestamp,
    pub payload_length: u32,
    pub payload_crc: u32,
}
const _: () = assert!(size_of::<WalEntryHeader>() == 40);

// JOURNAL-DESIGN§1.2 / ENCRYPTION-DESIGN§1 — encrypted WAL entry framing.
// Plaintext entry overhead = WalEntryHeader (40) + framing checksum (4, low half of checksum64) = 44 B.
// Encrypted entry overhead = header (40) + GCM tag (16) + framing checksum (4, low half of checksum64) = 60 B.
// The GCM tag does NOT replace `payload_crc` (4 B is too narrow for a 16 B tag);
// it sits between the ciphertext and the trailing framing CRC.
pub const WAL_FRAMING_CRC_BYTES: usize = 4;
pub const WAL_GCM_TAG_BYTES:     usize = 16;          // AES-256-GCM standard tag
pub const WAL_GCM_NONCE_BYTES:   usize = 12;          // 96-bit nonce (LSN || 0)
pub const WAL_GCM_AAD_BYTES:     usize =
    size_of::<WalEntryHeader>() - size_of::<u32>();   // header[0..36] excludes payload_crc
const _: () = assert!(WAL_GCM_AAD_BYTES == 36);

pub const WAL_ENTRY_PLAINTEXT_OVERHEAD: usize =
    size_of::<WalEntryHeader>() + WAL_FRAMING_CRC_BYTES;
pub const WAL_ENTRY_ENCRYPTED_OVERHEAD: usize =
    size_of::<WalEntryHeader>() + WAL_GCM_TAG_BYTES + WAL_FRAMING_CRC_BYTES;
const _: () = assert!(WAL_ENTRY_PLAINTEXT_OVERHEAD == 44);
const _: () = assert!(WAL_ENTRY_ENCRYPTED_OVERHEAD == 60);

// AES-GCM is length-preserving: payload_length counts ciphertext bytes,
// equal to plaintext bytes. The encrypted layout adds exactly tag-bytes
// of overhead beyond plaintext.
const _: () = assert!(
    WAL_ENTRY_ENCRYPTED_OVERHEAD - WAL_ENTRY_PLAINTEXT_OVERHEAD == WAL_GCM_TAG_BYTES
);

// Nonce derivation: 64-bit LSN || 32-bit zero = 96 bits = 12 B.
const _: () = assert!(size_of::<u64>() + size_of::<u32>() == WAL_GCM_NONCE_BYTES);

// =====================================================================
// METADATA-DESIGN§1.1 ObjectRecord — 128 B
// =====================================================================
#[repr(C)]
pub struct ObjectRecord {
    pub id: u64,
    pub generation: u32,
    pub state: u8,
    pub flags: u8,
    pub record_version: u16,
    pub content_hash: [u8; 32],
    pub blob_offset: u64,
    pub blob_length: u64,
    pub created_ns: i64,
    pub modified_ns: i64,
    pub tag_count: u16,
    pub attr_count: u16,
    pub compression: u8,
    pub encryption: u8,
    pub relation_count: u16,            // was _pad0; absorbs into the slot at offset 86
    pub inline_tags: [u32; 4],
    pub overflow_offset: u64,
    pub stored_size: u64,
    pub last_modify_lsn: u64,
}
const _: () = assert!(size_of::<ObjectRecord>() == 128);
// Per-object cardinality fields land at fixed offsets — important because
// they're hot-path on every mutation (tag/attr/relation add/remove).
const _: () = assert!(std::mem::offset_of!(ObjectRecord, tag_count) == 80);
const _: () = assert!(std::mem::offset_of!(ObjectRecord, attr_count) == 82);
const _: () = assert!(std::mem::offset_of!(ObjectRecord, relation_count) == 86);

// =====================================================================
// METADATA-DESIGN§1.2 OverflowRecord per-attribute layout — 32 B (spill) or 112 B (inline)
// `OverflowAttr.flags` is the discriminator that tells a reader how many
// bytes to consume for the body (the spill arm is 16 B, the inline arm
// is 96 B; without the flag the variant is unrecoverable).
// =====================================================================
pub const OVERFLOW_ATTR_FLAG_SPILL: u8 = 1 << 0;

// ENGINE-DESIGN§1.2 BlobRef — 16 B, bucket-relative; a length in place of a generation.
#[repr(C)]
pub struct BlobRef {
    pub disk_id: u16,        //  [0..2]
    pub sector: u16,         //  [2..4]
    pub bucket_no: u32,      //  [4..8]
    pub length: u64,         //  [8..16]  bytes
}
const _: () = assert!(size_of::<BlobRef>() == 16);
const _: () = assert!(std::mem::align_of::<BlobRef>() == 8);
const _: () = assert!(std::mem::offset_of!(BlobRef, disk_id) == 0);
const _: () = assert!(std::mem::offset_of!(BlobRef, sector) == 2);
const _: () = assert!(std::mem::offset_of!(BlobRef, bucket_no) == 4);
const _: () = assert!(std::mem::offset_of!(BlobRef, length) == 8);

#[repr(C)]
pub struct OverflowAttrHead {                // 16 B common prefix
    pub flags: u8,                           // OVERFLOW_ATTR_FLAG_*
    pub _pad0: [u8; 3],                      // align `key` to u32
    pub key: u32,
    pub value_hash: u64,
}
const _: () = assert!(size_of::<OverflowAttrHead>() == 16);
const _: () = assert!(std::mem::offset_of!(OverflowAttrHead, key) == 4);
const _: () = assert!(std::mem::offset_of!(OverflowAttrHead, value_hash) == 8);

// Inline-body variant: head + 96 B value.
#[repr(C)]
pub struct OverflowAttrInline {
    pub head: OverflowAttrHead,
    pub inline_value: [u8; 96],
}
const _: () = assert!(size_of::<OverflowAttrInline>() == 112);

// Spill-body variant: head + BlobRef (no length needed inline, the
// referenced extent carries it).
#[repr(C)]
pub struct OverflowAttrSpill {
    pub head: OverflowAttrHead,
    pub spill: BlobRef,
}
const _: () = assert!(size_of::<OverflowAttrSpill>() == 32);

// The inline arm is exactly 80 B larger than the spill arm — the delta
// between a 96 B inline value and a 16 B BlobRef.
const _: () = assert!(
    size_of::<OverflowAttrInline>() - size_of::<OverflowAttrSpill>()
        == 96 - size_of::<BlobRef>()
);

// =====================================================================
// METADATA-DESIGN§2.1 ObjectLocation — 48 B, ReplicaRef — 8 B
// ReplicaRef is bucket-relative, mirroring BackpointerKey's layout
// (METADATA-DESIGN§2.2) so move/scrub/resilver paths share field-level conversions.
// Per-disk reach: 2^32 buckets × bucket_size = 4 PiB (1 MiB buckets)
// up to 16 PiB (4 MiB buckets).
// =====================================================================
#[repr(C)]
pub struct ReplicaRef {
    pub disk_id: u16,
    pub sector_offset: u16,
    pub bucket_no: u32,
}
const _: () = assert!(size_of::<ReplicaRef>() == 8);

// Symmetric replicas: every physical copy lives in replicas[0..replica_count].
// No distinguished "primary"; replicas[0] is read-preferred by convention.
// replica_count is ground-truth cardinality (≤ 4); enumerating it from
// backpointers must yield exactly replica_count matches.
#[repr(C, align(8))]
pub struct ObjectLocation {
    pub flags: u8,
    pub replica_count: u8,
    pub _pad: [u8; 6],
    pub extent_length: u64,
    pub replicas: [ReplicaRef; 4],
}
const _: () = assert!(size_of::<ObjectLocation>() == 48);

// =====================================================================
// METADATA-DESIGN§2.2 BackpointerKey — 8 B, BackpointerValue — 24 B
// =====================================================================
#[repr(C)]
pub struct BackpointerKey {
    pub disk_id: u16,
    pub bucket_no: [u8; 4],      // little-endian u32 at offset 2
    pub sector_offset: u16,
}
const _: () = assert!(size_of::<BackpointerKey>() == 8);

#[repr(C)]
pub struct BackpointerValue {
    pub owner_kind: u8,
    pub _pad: u8,
    pub length_sectors: u16,
    pub bucket_gen: u32,
    pub owner_key: [u8; 16],
}
const _: () = assert!(size_of::<BackpointerValue>() == 24);

// =====================================================================
// INDEX-DESIGN§2.1 PackedAssertion — 16 B
// =====================================================================
#[repr(C)]
pub struct PackedAssertion {
    pub kind: u8,
    pub origin: u8,
    pub _pad: u16,
    pub a: u32,
    pub b: u64,
}
const _: () = assert!(size_of::<PackedAssertion>() == 16);

// =====================================================================
// INDEX-DESIGN§3.1 TagIndexLeafEntry — 48 B
// Snapshot-aware (SNAPSHOTS-DESIGN§1.2): key is (tag_id, snapshot). Fields ordered so every
// multi-byte field lands at its natural alignment. The hot key+pointer set
// (store_root through store_kind) all fits within [0..48], a single 64-byte
// cache line.
// =====================================================================
#[repr(C)]
pub struct TagIndexLeafEntry {
    pub last_modify_lsn: u64,    //  8 @  0   (8-aligned)
    pub store_root: BlockRef,    // 16 @  8   (4-aligned BlockRef at an 8-aligned offset)
    pub tag_id: u32,             //  4 @ 24   sort key
    pub snapshot: u32,           //  4 @ 28   sort key (suffix); SNAPSHOTS-DESIGN§1.2
    pub cardinality: u32,        //  4 @ 32
    pub generation: u32,         //  4 @ 36
    pub store_kind: u8,          //  1 @ 40
    pub _pad: [u8; 7],           //  7 @ 41
                                 // 48 total, struct alignment = 8
}
const _: () = assert!(size_of::<TagIndexLeafEntry>() == 48);
const _: () = assert!(std::mem::align_of::<TagIndexLeafEntry>() == 8);

// SNAPSHOTS-DESIGN§1.2 — TagIndexLeafEntry gained 4 bytes of `snapshot` field; with
// 8-byte struct alignment the on-disk size grew from 40 → 48 (4 B field
// + 4 B trailing alignment pad). The snapshot field is at offset 28
// (immediately after the tag_id sort key).
const _: () = assert!(std::mem::offset_of!(TagIndexLeafEntry, snapshot) == 28);
const _: () = assert!(std::mem::offset_of!(TagIndexLeafEntry, tag_id) == 24);

// =====================================================================
// INDEX-DESIGN§4.1 KvDirectory — 4096 B (was [BlockRef; 512] which overflowed)
// Capacity: 252 entries; supports global_depth ≤ 7 (128 active buckets).
// Larger pools spill via spillover_root (next-level BTREE-DESIGN§1 region).
// =====================================================================
#[repr(C)]
pub struct KvDirectory {
    pub header: BlockHeader,                    //  32
    pub global_depth: u8,                       //   1
    pub _pad0: [u8; 3],                         //   3
    pub bucket_count: u32,                      //   4
    pub entries: [BlockRef; 252],               // 252*16 = 4032
    pub spillover_root: BlockRef,               //  16
    pub _pad_tail: [u8; 4],                     //   4
    pub trailing_crc: u32,                      //   4
}
const _: () = assert!(size_of::<KvDirectory>() == 4096);

// =====================================================================
// INDEX-DESIGN§4.1 KvBucket — 4096 B
// =====================================================================
#[repr(C)]
pub struct KvBucketEntry {
    pub tag_id: u32,
    pub value_hash: [u8; 8],     // little-endian u64 at offset 4
    pub bitmap_ref: BlockRef,
}
const _: () = assert!(size_of::<KvBucketEntry>() == 28);

// 4096 - 32 (header) - 1 (local_depth) - 2 (entry_count) - 1 (_pad) - 4 (trailing CRC) = 4056
// 4056 / 28 = 144 entries
// Let's use 144 to maximise.
#[repr(C)]
pub struct KvBucket {
    pub header: BlockHeader,                    //  32
    pub local_depth: u8,                        //   1
    pub entry_count: [u8; 2],                   //   2  little-endian u16 at offset 33
    pub _pad: u8,                               //   1
    pub entries: [KvBucketEntry; 144],          // 144*28 = 4032
    pub _pad_tail: [u8; 24],                    //  24
    pub trailing_crc: u32,                      //   4
}
const _: () = assert!(size_of::<KvBucket>() == 4096);

// =====================================================================
// INDEX-DESIGN§3.3 SequencePage / RankedPage — 4096 B each
// Each page is a 4 KiB block under the standard FORMAT-DESIGN§1.3 BlockHeader framing,
// with a trailing `next: BlockRef` slot (zero on the tail page) forming
// a singly-linked chain.
// =====================================================================
#[repr(C)]
pub struct SequencePage {
    pub header: BlockHeader,                 //   32
    pub entry_count: u16,                    //    2
    pub _pad: [u8; 6],                       //    6
    pub next: BlockRef,                      //   16  (0 = tail of chain)
    pub entries: [u64; 504],                 // 4032 (504 × 8)
    pub _pad_tail: [u8; 4],                  //    4
    pub trailing_crc: u32,                   //    4
}
const _: () = assert!(size_of::<SequencePage>() == 4096);

#[repr(C)]
pub struct RankedEntry {                     // 16 B
    pub oid: u64,
    pub score: f32,
    pub _pad: u32,
}
const _: () = assert!(size_of::<RankedEntry>() == 16);

#[repr(C)]
pub struct RankedPage {
    pub header: BlockHeader,                 //   32
    pub entry_count: u16,                    //    2
    pub _pad: [u8; 6],                       //    6
    pub next: BlockRef,                      //   16  (0 = tail of chain)
    pub entries: [RankedEntry; 252],         // 4032 (252 × 16)
    pub _pad_tail: [u8; 4],                  //    4
    pub trailing_crc: u32,                   //    4
}
const _: () = assert!(size_of::<RankedPage>() == 4096);

// =====================================================================
// INDEX-DESIGN§4.1 KvBucket explicit packing — assert the 144-entry calculation
// matches the 4 KiB block budget exactly. The overall size_of assertion
// already proves it indirectly; this version pins the arithmetic so the
// doc's 144 figure stays load-bearing.
// =====================================================================
pub const KV_BUCKET_HEADER:           usize = size_of::<BlockHeader>();      // 32
pub const KV_BUCKET_LOCAL_DEPTH:      usize = 1;
pub const KV_BUCKET_ENTRY_COUNT:      usize = 2;
pub const KV_BUCKET_PAD_PRE_ENTRIES:  usize = 1;
pub const KV_BUCKET_PAD_TAIL:         usize = 24;
pub const KV_BUCKET_TRAILING_CRC:     usize = 4;
pub const KV_BUCKET_OVERHEAD:         usize =
    KV_BUCKET_HEADER
        + KV_BUCKET_LOCAL_DEPTH
        + KV_BUCKET_ENTRY_COUNT
        + KV_BUCKET_PAD_PRE_ENTRIES
        + KV_BUCKET_PAD_TAIL
        + KV_BUCKET_TRAILING_CRC;
pub const KV_BUCKET_ENTRIES:          usize =
    (BLOCK_SIZE - KV_BUCKET_OVERHEAD) / size_of::<KvBucketEntry>();
const _: () = assert!(KV_BUCKET_ENTRIES == 144);
const _: () =
    assert!(KV_BUCKET_OVERHEAD + KV_BUCKET_ENTRIES * size_of::<KvBucketEntry>() == BLOCK_SIZE);

// =====================================================================
// POOL-DESIGN§1 inline-disks limit — must match Superblock's PoolStateRoot.inline_disks
// array length and the doc's "disk_count ≤ 12 → inline" rule.
// =====================================================================
pub const INLINE_DISK_LIMIT: usize = 12;
const _: () = assert!(
    size_of::<PoolStateRoot>()
        == size_of::<BlockHeader>()
            + 4 + 4                                          // disk_count + cluster_node_count
            + INLINE_DISK_LIMIT * size_of::<DiskDescriptorOnDisk>()
            + 980 + 4                                        // _reserved + trailing_crc
);

// =====================================================================
// INDEX-DESIGN§4.3 ChunkIndexLeafEntry — 56 B
// `blob` is a BlobRef (ENGINE-DESIGN§1.2): chunk-extent freshness comes from the
// owning bucket's generation at deref time, not from a per-pointer gen
// counter. The leaf entry duplicates the chunk's plaintext length in
// `length: u32` for fast iteration without dereferencing the BlobRef.
// =====================================================================
#[repr(C)]
pub struct ChunkIndexLeafEntry {
    pub chunk_hash: [u8; 32],
    pub ref_count: u32,
    pub length: u32,
    pub blob: BlobRef,
}
const _: () = assert!(size_of::<ChunkIndexLeafEntry>() == 56);

// =====================================================================
// INDEX-DESIGN§4.3 ChunkListEntry — 40 B (positional within ChunkList region)
// Region capacity: (262 144 − 64 − 16) / 40 = 6 551 entries before chaining
// through the trailing BlockRef slot.
// =====================================================================
#[repr(C)]
pub struct ChunkListEntry {
    pub chunk_hash: [u8; 32],   // [0..32]   BLAKE3 of plaintext, indexes ChunkIndex
    pub length: u32,            // [32..36]  plaintext length of this chunk
    pub flags: u32,             // [36..40]  reserved
}
const _: () = assert!(size_of::<ChunkListEntry>() == 40);

// =====================================================================
// INDEX-DESIGN§4.3 ChunkParamsRecord — 16 B, lives in the ChunkList head region only
// (BTREE-DESIGN§1.1 BTREE_NODE_FLAG_HEAD_OF_CHAIN). Records the chunking parameters
// resolved at write time (SPECIFICATION-DESIGN§3.5). Reconcile compares this against
// the currently-resolved policy and re-chunks on mismatch (RECONCILE-DESIGN§1.1).
// =====================================================================
#[repr(C)]
pub struct ChunkParamsRecord {
    pub algo: u8,                            // [0..1]   ChunkingAlgo discriminant
    pub flags: u8,                           // [1..2]   reserved
    pub _pad: [u8; 2],                       // [2..4]
    pub min_size: u32,                       // [4..8]   plaintext bytes; ignored when algo == None
    pub avg_size: u32,                       // [8..12]
    pub max_size: u32,                       // [12..16]
}
const _: () = assert!(size_of::<ChunkParamsRecord>() == 16);

// ChunkingAlgo discriminants — pinned numerically. 3+ reserved for future.
pub const CHUNKING_ALGO_NONE:       u8 = 0;
pub const CHUNKING_ALGO_FIXED_SIZE: u8 = 1;
pub const CHUNKING_ALGO_FAST_CDC:   u8 = 2;
pub const CHUNKING_ALGO_COUNT:      u8 = 3;
const _: () = assert!(CHUNKING_ALGO_FAST_CDC + 1 == CHUNKING_ALGO_COUNT);

// INDEX-DESIGN§4.3 head-region capacity. The 16 B ChunkParamsRecord lives in space that
// was previously trailing pad, so capacity stays at 6 551 entries — same as
// chained (non-head) regions. Pin both shapes.
pub const CHUNK_LIST_HEAD_PAYLOAD: usize =
    REGION
        - size_of::<BtreeNodeHeader>()
        - size_of::<ChunkParamsRecord>()
        - size_of::<BlockRef>();             // trailing chain ref
pub const CHUNK_LIST_HEAD_ENTRIES: usize =
    CHUNK_LIST_HEAD_PAYLOAD / size_of::<ChunkListEntry>();
const _: () = assert!(CHUNK_LIST_HEAD_PAYLOAD == 262_048);
const _: () = assert!(CHUNK_LIST_HEAD_ENTRIES == 6_551);
const _: () = assert!(CHUNK_LIST_HEAD_ENTRIES == CHUNK_LIST_ENTRIES);  // same as chained

// UNIX-DESIGN§1 — Path projections are encoded as ordinary tag/attribute assertions
// (Value::Scoped { context, inner }; INDEX-DESIGN§1.3). Nothing here to validate at the
// layout level; ontology validation handles the type rules.

// =====================================================================
// POOL-DESIGN§1 DiskDescriptorOnDisk — 256 B (path: [u8; 192])
// path_len = number of valid UTF-8 bytes in path[]; remainder zero-padded.
// 192 B path covers full /dev/disk/by-id/... names, NVMe-oF discovery URLs,
// long S3/HTTP endpoints, etc.
// =====================================================================
#[repr(C)]
pub struct DiskDescriptorOnDisk {
    pub disk_id: u16,
    pub media_type: u8,
    pub tier: u8,
    pub state: u8,
    pub _pad0: u8,
    pub path_len: u8,
    pub _pad1: u8,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub bucket_count: u32,
    pub first_usable_bucket: u32,
    pub buckets_root: BlockRef,
    pub freespace_root: BlockRef,
    pub path: [u8; 192],
}
const _: () = assert!(size_of::<DiskDescriptorOnDisk>() == 256);

// =====================================================================
// POOL-DESIGN§1 PoolStateRoot — 4096 B
// Hybrid disk storage:
//   disk_count ≤ 12: all disks live in inline_disks[0..disk_count];
//                    RootPointer.disks_overflow_root is zero.
//   disk_count > 12: all disks live in the disks-overflow B+ tree
//                    (BtreeKind::DiskDescriptors) keyed by disk_id;
//                    inline_disks[] is unused. Crossing the threshold
//                    migrates the inline contents into the tree.
// =====================================================================
#[repr(C)]
pub struct PoolStateRoot {
    pub header: BlockHeader,                        //   32 B
    pub disk_count: u32,                            //    4 B
    pub cluster_node_count: u32,                    //    4 B
    pub inline_disks: [DiskDescriptorOnDisk; 12],   // 3072 B  (12 × 256)
    pub _reserved: [u8; 980],                       //  980 B  (room for pool-wide tunables)
    pub trailing_crc: u32,                          //    4 B
}
const _: () = assert!(size_of::<PoolStateRoot>() == 4096);

// =====================================================================
// SNAPSHOTS-DESIGN§1.1 SnapshotNode — 64 B
// First-child / next-sibling topology supports arbitrary fan-out at no
// extra per-node cost vs. the previous fixed [u32; 2] children array.
// `ancestor_bitmap: u128` is placed at offset 16 so the u128 hits its
// natural alignment on the hot ancestry-check path.
// =====================================================================
#[repr(C)]
pub struct SnapshotNode {
    pub id: u32,                // [0..4]
    pub parent: u32,            // [4..8]
    pub first_child: u32,       // [8..12]
    pub next_sibling: u32,      // [12..16]
    pub ancestor_bitmap: u128,  // [16..32]   16-byte aligned
    pub skiplist: [u32; 3],     // [32..44]
    pub depth: u16,             // [44..46]
    pub flags: u8,              // [46..47]
    pub _pad: u8,               // [47..48]
    pub created_ns: i64,        // [48..56]
    pub label_offset: u32,      // [56..60]
    pub _reserved: u32,         // [60..64]
}
const _: () = assert!(size_of::<SnapshotNode>() == 64);
const _: () = assert!(std::mem::offset_of!(SnapshotNode, ancestor_bitmap) == 16);

// =====================================================================
// ALLOCATION-DESIGN§1.2 BucketAllocEntry — 8 B
// =====================================================================
#[repr(C)]
pub struct BucketAllocEntry {
    pub generation: u32,         // [0..4]
    pub kind_flags: u8,          // [4..5]
    pub live_sectors: [u8; 3],   // [5..8] u24 LE
}
const _: () = assert!(size_of::<BucketAllocEntry>() == 8);
const _: () = assert!(std::mem::offset_of!(BucketAllocEntry, kind_flags) == 4);
const _: () = assert!(std::mem::offset_of!(BucketAllocEntry, live_sectors) == 5);

// =====================================================================
// RECONCILE-DESIGN§1.2 WorkItem — 48 B
// =====================================================================
#[repr(C)]
pub struct WorkItem {
    pub target_kind: u8,
    pub work_kind: u8,
    pub attempt_count: u8,
    pub last_error_code: u8,
    pub flags: u32,
    pub enqueued_lsn: u64,
    pub desired_state_ref: BlockRef,
    pub owner_key: [u8; 16],
}
const _: () = assert!(size_of::<WorkItem>() == 48);

// =====================================================================
// BTREE-DESIGN§1.2, §1.5–1.7 region capacity in the write-once format, and the
// figures the design documents quote from it (METADATA-DESIGN§1, METADATA-DESIGN§2.2,
// INDEX-DESIGN§4.3, ALLOCATION-DESIGN§1.1, FORMAT-DESIGN§16).
// =====================================================================
pub const REGION: usize = 256 * 1024; // 262 144
pub const REGION_BLOCKS: usize = REGION / BLOCK_SIZE; // 64
// One run: block 0, its index block, then data blocks.
pub const REGION_DATA_BLOCKS_ONE_RUN: usize = REGION_BLOCKS - 2;
const _: () = assert!(REGION_DATA_BLOCKS_ONE_RUN == 62);

const fn align8(value: usize) -> usize { value.div_ceil(8) * 8 }

/// Bytes an entry block uses for `count` entries: DataBlockHead, keys, entry flags and (for
/// variable values) u16 value ends, padded to 8, then each value 8-aligned.
const fn entry_block_bytes(count: usize, key_width: usize, value_len: usize, variable: bool) -> usize {
    let value_ends = if variable { count * 2 } else { 0 };
    align8(size_of::<DataBlockHead>() + count * key_width + count + value_ends) + count * align8(value_len)
}

/// Entries one 4 KiB entry block holds (BTREE-DESIGN§1.2).
pub const fn entries_per_block(key_width: usize, value_len: usize, variable: bool) -> usize {
    let mut count = 0;
    while entry_block_bytes(count + 1, key_width, value_len, variable) <= BLOCK_SIZE {
        count += 1;
    }
    count
}

/// Data blocks one index block can describe for keys of `fields` fields and `key_width` bytes.
pub const fn blocks_per_index_block(fields: usize, key_width: usize) -> usize {
    (BLOCK_SIZE - size_of::<RunHeader>() - size_of::<KeyFormatHead>() - fields * size_of::<RunFieldFormat>())
        / (size_of::<RunBlockEntry>() + key_width)
}

/// Entries a leaf holds as one run filling the region.
pub const fn entries_per_leaf(key_width: usize, value_len: usize, variable: bool) -> usize {
    entries_per_block(key_width, value_len, variable) * REGION_DATA_BLOCKS_ONE_RUN
}

pub const OBJECTS_10M: usize = 10_000_000;

// --- Positional radix (METADATA-DESIGN§1, BTREE-DESIGN§1.7) -----------------
pub const RADIX_PAGES: usize = REGION_BLOCKS * 3 / 4;
const _: () = assert!(RADIX_PAGES == POSITIONAL_LOGICAL_PAGES_256_KIB);
pub const OBJECT_TABLE_LEAF_RECORDS: usize = RADIX_PAGES * (BLOCK_SIZE / size_of::<ObjectRecord>());
const _: () = assert!(OBJECT_TABLE_LEAF_RECORDS == 1_536);          // "1 536 per leaf"
/// NodePtr slots of an inner page at R metadata replicas (16 × R bytes each).
pub const fn radix_inner_slots(replicas: usize) -> usize {
    RADIX_PAGES * (BLOCK_SIZE / (size_of::<BlockRef>() * replicas))
}
const _: () = assert!(radix_inner_slots(1) == 12_288);               // "12 288 / R per node"
const _: () = assert!(radix_inner_slots(2) == 6_144);
pub const OBJECT_TABLE_LEAVES_10M: usize = OBJECTS_10M.div_ceil(OBJECT_TABLE_LEAF_RECORDS);
const _: () = assert!(OBJECT_TABLE_LEAVES_10M == 6_511);             // "6 511 leaves"
const _: () = assert!(OBJECT_TABLE_LEAVES_10M <= radix_inner_slots(1)); // under 1 inner at R = 1
const _: () = assert!(OBJECT_TABLE_LEAVES_10M > radix_inner_slots(2) / 1); // depth 2 at R = 2 needs > 6 144 leaves
// Records 10 M × 128 B ≈ 1.19 GiB; leaves 6 511 × 256 KiB ≈ 1.59 GiB ("~1.6 GiB").
const _: () = assert!(OBJECTS_10M * size_of::<ObjectRecord>() == 1_280_000_000);
const _: () = assert!(OBJECT_TABLE_LEAVES_10M * REGION > 1_700_000_000 && OBJECT_TABLE_LEAVES_10M * REGION < 1_710_000_000);

// METADATA-DESIGN§1 depth table and MAX_LEVELS = 4 (inner depth).
pub const MAX_LEVELS: u32 = 4;
pub const LOCAL_ID_SPACE: u64 = 1u64 << 48;
pub const fn radix_capacity(inner_levels: u32, replicas: usize) -> u64 {
    (OBJECT_TABLE_LEAF_RECORDS as u64) * (radix_inner_slots(replicas) as u64).pow(inner_levels)
}
const _: () = assert!(radix_capacity(1, 1) == 18_874_368);          // "≈ 18.9 M"
const _: () = assert!(radix_capacity(1, 2) == 9_437_184);           // "≈ 9.4 M"
const _: () = assert!(radix_capacity(3, 2) >= LOCAL_ID_SPACE);      // depth 3 for R ≤ 2
const _: () = assert!(radix_capacity(3, 4) < LOCAL_ID_SPACE);
const _: () = assert!(radix_capacity(MAX_LEVELS, 4) >= LOCAL_ID_SPACE); // depth 4 for R ≤ 4
// A flush appends a page run per changed node: one changed page costs an index block
// and the page (BTREE-DESIGN§1.7), so a one-record change at depth D writes (D + 1) × 8 KiB.
pub const fn single_page_flush_bytes(depth: u32) -> usize { (depth as usize + 1) * 2 * BLOCK_SIZE }
const _: () = assert!(single_page_flush_bytes(1) == 16 * 1024);

// --- Keyed runs (BTREE-DESIGN§1.2, §1.6) ----------------------------------
// Every tree below fits one run's data blocks in one index block.
const _: () = assert!(blocks_per_index_block(7, 24) >= REGION_DATA_BLOCKS_ONE_RUN);
const _: () = assert!(blocks_per_index_block(5, 16) >= REGION_DATA_BLOCKS_ONE_RUN);

// Content map (INDEX-DESIGN§4.3): ~5-byte keys (oid, end_offset, snapshot) and 88-byte
// fixed ChunkEntry values — "about 43 entries per 4 KiB block … about 2 650 chunks".
pub const CONTENT_MAP_ENTRY_BYTES: usize = 88;
const _: () = assert!(entries_per_block(5, CONTENT_MAP_ENTRY_BYTES, false) == 43);
const _: () = assert!(entries_per_block(6, CONTENT_MAP_ENTRY_BYTES, false) == 43);
const _: () = assert!(entries_per_leaf(5, CONTENT_MAP_ENTRY_BYTES, false) == 2_666);

// Backpointers (METADATA-DESIGN§2.2): ~12-byte keys and 24-byte fixed values, ~37 B per
// entry — "about 6 800 backpointers per leaf".
const _: () = assert!(entries_per_block(12, 24, false) == 110);
const _: () = assert!(entries_per_leaf(12, 24, false) == 6_820);

// Bucket alloc table (ALLOCATION-DESIGN§1.1, FORMAT-DESIGN§16): 2-byte bucket keys and
// 8-byte entries, 11 B per entry — 23 002 buckets per leaf.
pub const BUCKET_ALLOC_PER_LEAF: usize = entries_per_leaf(2, size_of::<BucketAllocEntry>(), false);
const _: () = assert!(entries_per_block(2, size_of::<BucketAllocEntry>(), false) == 371);
const _: () = assert!(BUCKET_ALLOC_PER_LEAF == 23_002);
pub const fn bucket_alloc_table_bytes(buckets: usize) -> usize {
    buckets.div_ceil(BUCKET_ALLOC_PER_LEAF) * REGION
}
const MIB: usize = 1024 * 1024;
const _: () = assert!(bucket_alloc_table_bytes(64 << 20) / MIB == 729);  // 256 KiB buckets, 16 TiB: "~730 MiB"
const _: () = assert!(bucket_alloc_table_bytes(16 << 20) / MIB == 182);  // 1 MiB buckets: "~180 MiB"
const _: () = assert!(bucket_alloc_table_bytes(4 << 20) / MIB == 45);    // 4 MiB buckets: "~46 MiB"
const _: () = assert!(bucket_alloc_table_bytes(256 << 10) / MIB == 3);   // 64 MiB buckets: "~3 MiB"
const _: () = assert!(bucket_alloc_table_bytes(64 << 10) == 3 * REGION); // 256 MiB buckets: "~768 KiB"

// Forward index (INDEX-DESIGN§2, FORMAT-DESIGN§16): ~3-byte (oid, snapshot) keys and
// variable ForwardEntry values — 58 B at 8 direct terms, 48 B at 4.
pub const FWD_PER_LEAF_8: usize = entries_per_leaf(3, 58, true);
pub const FWD_PER_LEAF_4: usize = entries_per_leaf(3, 48, true);
const _: () = assert!(entries_per_block(3, 58, true) == 58);
const _: () = assert!(FWD_PER_LEAF_8 == 3_596);
pub const FWD_LEAVES_10M_8: usize = OBJECTS_10M.div_ceil(FWD_PER_LEAF_8);
const _: () = assert!(FWD_LEAVES_10M_8 == 2_781);                   // "~2 800 leaves"
const _: () = assert!(FWD_LEAVES_10M_8 * REGION / MIB == 695);       // "~0.7 GiB"
pub const FWD_LEAVES_10M_4: usize = OBJECTS_10M.div_ceil(FWD_PER_LEAF_4);
const _: () = assert!(FWD_LEAVES_10M_4 * REGION / MIB == 537);       // "~0.5 GiB", about a quarter less

// Term dictionary (INDEX-DESIGN§4.1): tag terms have ~4-byte keys (kind, value and
// parent constant) and 56-byte fixed DictEntry values — "about 4 000 tag entries fit one leaf".
const _: () = assert!(entries_per_block(4, 56, false) == 67);
const _: () = assert!(entries_per_leaf(4, 56, false) == 4_154);

// =====================================================================
// JOURNAL-DESIGN§1.2 hard cap: a single WAL op fits in one 4 KiB sector. No continuation
// framing — bulk data goes through the blob zone (INDEX-DESIGN§1.1) and is referenced
// by hash. Sector-aligned framing keeps recovery trivial: a torn 4 KiB
// write loses at most one entry, and replay never reassembles fragments.
//   plaintext bound : 4096 − WalEntryHeader (40) − framing CRC (4)        = 4052
//   encrypted bound : plaintext − GCM tag (16)                            = 4036
// `WAL_OP_MAX_PAYLOAD` is the conservative cap producers must enforce
// (works in both modes; debug_assert! in the appender catches violations).
// =====================================================================
pub const BLOCK_SIZE: usize = 4096;
pub const WAL_MAX_PAYLOAD_PLAINTEXT: usize = BLOCK_SIZE - WAL_ENTRY_PLAINTEXT_OVERHEAD;
pub const WAL_MAX_PAYLOAD_ENCRYPTED: usize = BLOCK_SIZE - WAL_ENTRY_ENCRYPTED_OVERHEAD;
pub const WAL_OP_MAX_PAYLOAD:        usize = WAL_MAX_PAYLOAD_ENCRYPTED;
const _: () = assert!(WAL_MAX_PAYLOAD_PLAINTEXT == 4_052);
const _: () = assert!(WAL_MAX_PAYLOAD_ENCRYPTED == 4_036);
const _: () = assert!(WAL_OP_MAX_PAYLOAD == 4_036);

// JOURNAL-DESIGN§1.3 variable-length op-field caps that keep every op under WAL_OP_MAX_PAYLOAD
// even with CBOR overhead. Sized so the largest op (Checkpoint with a 408 B
// RootPointer plus ~20 B CBOR framing, or SnapshotCreate with a 256 B label)
// stays comfortably below the sector bound. owner_key is naturally bounded
// by METADATA-DESIGN§2.2's 16 B BackpointerValue.
pub const WAL_LABEL_MAX_BYTES:      usize = 256;
pub const WAL_CURSOR_KEY_MAX_BYTES: usize = 256;
pub const WAL_OWNER_KEY_BYTES:      usize = 16;     // METADATA-DESIGN§2.2 BackpointerValue.owner_key
const _: () = assert!(WAL_LABEL_MAX_BYTES      < WAL_OP_MAX_PAYLOAD);
const _: () = assert!(WAL_CURSOR_KEY_MAX_BYTES < WAL_OP_MAX_PAYLOAD);

// =====================================================================
// JOURNAL-DESIGN§1.3.1 Chunked-object WAL ops (INDEX-DESIGN§4.3).
// Per-entry sizes (raw, unpacked):
//   ChunkInsertBatch entry: chunk_hash (32) + extent: BlockRef (16) + length (4) = 52 B
//   ChunkListReplace hash:  32 B
// CBOR adds ~4 B framing per array + ~3 B per record. We allow generous slack so the
// producer can pick any N that fits, capped by these constants — debug_assert! in the
// appender catches CBOR overflow.
// =====================================================================
pub const CHUNK_INSERT_ENTRY_BYTES:  usize = 32 + 16 + 4;     // 52 B
pub const CHUNK_INSERT_FRAMING_SLACK: usize = 64;             // op-kind + array prefix + per-entry CBOR overhead
pub const CHUNK_INSERT_BATCH_MAX:    usize =
    (WAL_OP_MAX_PAYLOAD - CHUNK_INSERT_FRAMING_SLACK) / CHUNK_INSERT_ENTRY_BYTES;
// (4036 − 64) / 52 = 76 entries — producer picks any N in 1..=76.
const _: () = assert!(CHUNK_INSERT_BATCH_MAX == 76);
const _: () = assert!(
    CHUNK_INSERT_FRAMING_SLACK
        + CHUNK_INSERT_BATCH_MAX * CHUNK_INSERT_ENTRY_BYTES
        <= WAL_OP_MAX_PAYLOAD
);

pub const CHUNK_LIST_REPLACE_HASH_BYTES: usize = 32;
pub const CHUNK_LIST_REPLACE_FRAMING_SLACK: usize = 32;        // oid + position + array prefix + CBOR
pub const CHUNK_LIST_REPLACE_MAX_HASHES: usize =
    (WAL_OP_MAX_PAYLOAD - CHUNK_LIST_REPLACE_FRAMING_SLACK) / CHUNK_LIST_REPLACE_HASH_BYTES;
// (4036 − 32) / 32 = 125 hashes per op.
const _: () = assert!(CHUNK_LIST_REPLACE_MAX_HASHES == 125);

// JOURNAL-DESIGN§1.3.1 throughput-model constants. Five regimes (4 KiB → 8 MiB) span FastCDC's
// real-world parameter range and pin every figure in the doc's spectrum table.
// All compute over a 32 GiB bulk write with no dedup.
pub const CHUNK_LIST_ENTRIES_PER_REGION: usize = CHUNK_LIST_ENTRIES;        // 6_551

const _: () = assert!(CHUNK_LIST_ENTRIES_PER_REGION == 6_551);

pub const CHUNK_BULK_WRITE_BYTES:        usize = 32 * 1024 * 1024 * 1024;   // 32 GiB
pub const WAL_RING_BYTES_DEFAULT:        usize = 64 * 1024 * 1024;          // JOURNAL-DESIGN§1 default

const fn chunks_for(avg: usize) -> usize { CHUNK_BULK_WRITE_BYTES / avg }
const fn insert_ops_for(avg: usize) -> usize {
    chunks_for(avg).div_ceil(CHUNK_INSERT_BATCH_MAX)
}
const fn list_ops_for(avg: usize) -> usize {
    chunks_for(avg).div_ceil(CHUNK_LIST_ENTRIES_PER_REGION)
}
// Worst-case WAL bytes: one full 4 KiB sector per op (real ChunkListAppend is ~50 B,
// real ChunkInsertBatch fills the sector — pinning the upper bound is the honest
// way to assert "WAL traffic is bounded, ring is circular, reclaim drains it").
const fn wal_bytes_for(avg: usize) -> usize {
    (insert_ops_for(avg) + list_ops_for(avg)) * BLOCK_SIZE
}

// --- 4 KiB extreme (small-chunk / fine-grained-edit regime) -------------
pub const CHUNK_AVG_4KIB:               usize = 4 * 1024;
pub const CHUNK_4KIB_CHUNKS:            usize = chunks_for(CHUNK_AVG_4KIB);
pub const CHUNK_4KIB_INSERT_OPS:        usize = insert_ops_for(CHUNK_AVG_4KIB);
pub const CHUNK_4KIB_LIST_OPS:          usize = list_ops_for(CHUNK_AVG_4KIB);
pub const CHUNK_4KIB_WAL_BYTES_MAX:     usize = wal_bytes_for(CHUNK_AVG_4KIB);
const _: () = assert!(CHUNK_4KIB_CHUNKS    == 8_388_608);                    // 8 M
const _: () = assert!(CHUNK_4KIB_INSERT_OPS == 110_377);                     // ~110 K
const _: () = assert!(CHUNK_4KIB_LIST_OPS   == 1_281);                       // ~1.28 K
// ~436 MiB worst-case — exceeds 64 MiB ring by ~6.8×; ring cycles continuously.
const _: () = assert!(CHUNK_4KIB_WAL_BYTES_MAX > 6 * WAL_RING_BYTES_DEFAULT);
const _: () = assert!(CHUNK_4KIB_WAL_BYTES_MAX < 7 * WAL_RING_BYTES_DEFAULT);

// --- 16 KiB (VM / database canonical workload) -------------------------
pub const CHUNK_AVG_16KIB:              usize = 16 * 1024;
pub const CHUNK_16KIB_CHUNKS:           usize = chunks_for(CHUNK_AVG_16KIB);
pub const CHUNK_16KIB_INSERT_OPS:       usize = insert_ops_for(CHUNK_AVG_16KIB);
pub const CHUNK_16KIB_LIST_OPS:         usize = list_ops_for(CHUNK_AVG_16KIB);
pub const CHUNK_16KIB_WAL_BYTES_MAX:    usize = wal_bytes_for(CHUNK_AVG_16KIB);
const _: () = assert!(CHUNK_16KIB_CHUNKS    == 2_097_152);                   // ~2 M
const _: () = assert!(CHUNK_16KIB_INSERT_OPS == 27_595);                     // ~28 K
const _: () = assert!(CHUNK_16KIB_LIST_OPS   == 321);
// ~115 MiB worst-case — exceeds ring by ~1.8×; ring cycles ~2× over the write.
const _: () = assert!(CHUNK_16KIB_WAL_BYTES_MAX > WAL_RING_BYTES_DEFAULT);
const _: () = assert!(CHUNK_16KIB_WAL_BYTES_MAX < 2 * WAL_RING_BYTES_DEFAULT);

// --- 256 KiB (generic FastCDC default) ---------------------------------
pub const CHUNK_AVG_256KIB:             usize = 256 * 1024;
pub const CHUNK_256KIB_CHUNKS:          usize = chunks_for(CHUNK_AVG_256KIB);
pub const CHUNK_256KIB_INSERT_OPS:      usize = insert_ops_for(CHUNK_AVG_256KIB);
pub const CHUNK_256KIB_LIST_OPS:        usize = list_ops_for(CHUNK_AVG_256KIB);
pub const CHUNK_256KIB_WAL_BYTES_MAX:   usize = wal_bytes_for(CHUNK_AVG_256KIB);
const _: () = assert!(CHUNK_256KIB_CHUNKS    == 131_072);                    // ~131 K
const _: () = assert!(CHUNK_256KIB_INSERT_OPS == 1_725);
const _: () = assert!(CHUNK_256KIB_LIST_OPS   == 21);
// ~7 MiB — fits inside the ring with room to spare.
const _: () = assert!(CHUNK_256KIB_WAL_BYTES_MAX < WAL_RING_BYTES_DEFAULT / 8);

// --- 1 MiB (backup-tool default, restic-class) -------------------------
pub const CHUNK_AVG_1MIB:               usize = 1024 * 1024;
pub const CHUNK_1MIB_CHUNKS:            usize = chunks_for(CHUNK_AVG_1MIB);
pub const CHUNK_1MIB_INSERT_OPS:        usize = insert_ops_for(CHUNK_AVG_1MIB);
pub const CHUNK_1MIB_LIST_OPS:          usize = list_ops_for(CHUNK_AVG_1MIB);
pub const CHUNK_1MIB_WAL_BYTES_MAX:     usize = wal_bytes_for(CHUNK_AVG_1MIB);
const _: () = assert!(CHUNK_1MIB_CHUNKS    == 32_768);
const _: () = assert!(CHUNK_1MIB_INSERT_OPS == 432);
const _: () = assert!(CHUNK_1MIB_LIST_OPS   == 6);
// ~1.8 MiB total.

// --- 8 MiB extreme (large-chunk / resumability regime) -----------------
pub const CHUNK_AVG_8MIB:               usize = 8 * 1024 * 1024;
pub const CHUNK_8MIB_CHUNKS:            usize = chunks_for(CHUNK_AVG_8MIB);
pub const CHUNK_8MIB_INSERT_OPS:        usize = insert_ops_for(CHUNK_AVG_8MIB);
pub const CHUNK_8MIB_LIST_OPS:          usize = list_ops_for(CHUNK_AVG_8MIB);
pub const CHUNK_8MIB_WAL_BYTES_MAX:     usize = wal_bytes_for(CHUNK_AVG_8MIB);
const _: () = assert!(CHUNK_8MIB_CHUNKS    == 4_096);
const _: () = assert!(CHUNK_8MIB_INSERT_OPS == 54);
const _: () = assert!(CHUNK_8MIB_LIST_OPS   == 1);                           // single region
// ~220 KiB total — bookkeeping overhead negligible vs 32 GiB plaintext.
const _: () = assert!(CHUNK_8MIB_WAL_BYTES_MAX < 256 * 1024);

// Spread across regimes: ~2 000× on every axis. Pin the magnitude.
const _: () = assert!(CHUNK_4KIB_CHUNKS / CHUNK_8MIB_CHUNKS == 2_048);
const _: () =
    assert!(CHUNK_4KIB_WAL_BYTES_MAX / CHUNK_8MIB_WAL_BYTES_MAX > 1_500);
const _: () =
    assert!(CHUNK_4KIB_WAL_BYTES_MAX / CHUNK_8MIB_WAL_BYTES_MAX < 2_500);

// Per-object ChunkList footprint (in 256 KiB regions) at the extremes.
// 4 KiB: 1 281 regions × 256 KiB ≈ 320 MiB ChunkList per object (~1 % of plaintext).
// 8 MiB: 1 region × 256 KiB = 256 KiB ChunkList per object.
pub const CHUNK_4KIB_LIST_FOOTPRINT_BYTES: usize = CHUNK_4KIB_LIST_OPS * REGION;
pub const CHUNK_8MIB_LIST_FOOTPRINT_BYTES: usize = CHUNK_8MIB_LIST_OPS * REGION;
const _: () = assert!(CHUNK_4KIB_LIST_FOOTPRINT_BYTES > 300 * 1024 * 1024);
const _: () = assert!(CHUNK_4KIB_LIST_FOOTPRINT_BYTES < 340 * 1024 * 1024);
const _: () = assert!(CHUNK_8MIB_LIST_FOOTPRINT_BYTES == REGION);            // 256 KiB

// =====================================================================
// INDEX-DESIGN§2.2 ForwardOverflow region: positional PackedAssertion array minus
// trailing-chain BlockRef. Region capacity claim: 16 379 entries.
// =====================================================================
pub const FORWARD_OVERFLOW_PAYLOAD: usize =
    REGION - size_of::<BtreeNodeHeader>() - size_of::<BlockRef>();
pub const FORWARD_OVERFLOW_ENTRIES: usize =
    FORWARD_OVERFLOW_PAYLOAD / size_of::<PackedAssertion>();
const _: () = assert!(FORWARD_OVERFLOW_PAYLOAD == 262_064);
const _: () = assert!(FORWARD_OVERFLOW_ENTRIES == 16_379);

// =====================================================================
// INDEX-DESIGN§4.3 ChunkList region: positional ChunkListEntry array minus
// trailing-chain BlockRef. Region capacity claim: 6 551 entries.
// =====================================================================
pub const CHUNK_LIST_PAYLOAD: usize =
    REGION - size_of::<BtreeNodeHeader>() - size_of::<BlockRef>();
pub const CHUNK_LIST_ENTRIES: usize =
    CHUNK_LIST_PAYLOAD / size_of::<ChunkListEntry>();
const _: () = assert!(CHUNK_LIST_PAYLOAD == 262_064);
const _: () = assert!(CHUNK_LIST_ENTRIES == 6_551);

// =====================================================================
// INDEX-DESIGN§4.1 KvDirectory inline capacity: 252 entries supports global_depth ≤ 7.
// At global_depth ≥ 8 the directory spills into a region of 16 380 BlockRef entries,
// supporting global_depth ≤ 13. TODO(INDEX12): goes with the extendible hash.
// =====================================================================
const KV_SPILLOVER_ENTRIES: usize = 16_380;
pub const KV_INLINE_ENTRIES: usize = 252;
pub const KV_INLINE_GLOBAL_DEPTH_MAX: u32 = 7;
const _: () = assert!((1u32 << KV_INLINE_GLOBAL_DEPTH_MAX) <= KV_INLINE_ENTRIES as u32);
// One step further would overflow:
const _: () = assert!((1u32 << (KV_INLINE_GLOBAL_DEPTH_MAX + 1)) > KV_INLINE_ENTRIES as u32);
pub const KV_SPILLOVER_GLOBAL_DEPTH_MAX: u32 = 13;
const _: () = assert!((1usize << KV_SPILLOVER_GLOBAL_DEPTH_MAX) <= KV_SPILLOVER_ENTRIES);
const _: () = assert!((1usize << (KV_SPILLOVER_GLOBAL_DEPTH_MAX + 1)) > KV_SPILLOVER_ENTRIES);

// =====================================================================
// ALLOCATION-DESIGN§1.4 Freespace LRU fragmentation_band: ⌈255 × live / sectors⌉
// Endpoints:
//   band == 0   iff live == 0 (empty bucket — full free)
//   band == 255 when live is full *or near-full* (the formula saturates
//               for the last few sectors because ⌈255·l/s⌉ rounds up)
// Intermediate live values map into 1..=254. Reorder cost is bounded by
// allocation pressure, not write volume: bands are recomputed lazily when
// live_sectors crosses an 8-sector boundary (ALLOCATION-DESIGN§1.4 prose).
// =====================================================================
pub const fn fragmentation_band(live: u64, sectors_per_bucket: u64) -> u8 {
    let band = (255u64 * live).div_ceil(sectors_per_bucket);
    band as u8
}
// Default bucket: 1 MiB / 4 KiB = 256 sectors.
const _: () = assert!(fragmentation_band(0,   256) == 0);     // empty → 0
const _: () = assert!(fragmentation_band(1,   256) == 1);     // barely occupied → 1
const _: () = assert!(fragmentation_band(128, 256) == 128);   // half full → 128
const _: () = assert!(fragmentation_band(254, 256) == 254);   // ⌈255*254/256⌉ = 254
const _: () = assert!(fragmentation_band(255, 256) == 255);   // saturates one sector early
const _: () = assert!(fragmentation_band(256, 256) == 255);   // fully occupied → 255
// 4 MiB bucket: 1024 sectors. Coarser scaling — multiple dirty values map
// to the same band, and saturation kicks in earlier (relative to capacity).
const _: () = assert!(fragmentation_band(0,    1024) == 0);
const _: () = assert!(fragmentation_band(1,    1024) == 1);   // ⌈255/1024⌉ = 1
const _: () = assert!(fragmentation_band(1024, 1024) == 255); // fully occupied
// Monotonicity sanity: band is non-decreasing in dirty.
const _: () = assert!(fragmentation_band(64,  256) <= fragmentation_band(128, 256));
const _: () = assert!(fragmentation_band(128, 256) <= fragmentation_band(192, 256));
// Empty-iff-zero invariant (used by foreground allocator to scan band == 0).
const _: () = assert!(fragmentation_band(0, 256)  == 0 && fragmentation_band(1, 256)  != 0);
const _: () = assert!(fragmentation_band(0, 1024) == 0 && fragmentation_band(1, 1024) != 0);

// =====================================================================
// BTREE-DESIGN§1.5 depth table (10 M objects, 5000 tags, 100 snapshots) — pin the
// concrete tier counts cited in the prose.
// =====================================================================
const _: () = assert!(OBJECT_TABLE_LEAVES_10M == 6_511);         // depth 1, under one inner
// Forward index: ~2 800 leaves under one inner of separator keys (~3 B + NodePtr 16 B).
const _: () = assert!(FWD_LEAVES_10M_8 <= entries_per_leaf(3, size_of::<BlockRef>(), false));
// Term dictionary: about 4 000 tag entries fit one leaf.
const _: () = assert!(entries_per_leaf(4, 56, false) >= 4_000);
