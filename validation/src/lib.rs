//! Compile-time size assertions mirroring docs/IMPLEMENTATION.md.
//! Each `const _` assertion fails to compile if the size is wrong.

#![allow(dead_code)]

use std::mem::size_of;

// =====================================================================
// Symbolic flag constants — mirrors §1.3, §1.5.1, §1.5.6, §3.1, §3.2,
// §5.1, §6.1, §7.1, §10.3, §11.1, §12.2 (BucketAllocEntry), §17.2.
// Asserted-by-existence: any code referring to these names compiles iff
// the constants stay defined here.
// =====================================================================

// BlockHeader.flags
pub const BLOCK_FLAG_ENCRYPTED:    u32 = 1 << 0;
pub const BLOCK_FLAG_CONTINUATION: u32 = 1 << 1;

// =====================================================================
// §1.3 BlockKind / BtreeKind discriminants — pinned numerically.
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
pub const BLOCK_KIND_SEQUENCE_PAGE:     u16 = 6;   // §8.3 OrderedStore page
pub const BLOCK_KIND_RANKED_PAGE:       u16 = 7;   // §8.3 RankedStore page
// KV equality pages
pub const BLOCK_KIND_KV_HASH_DIRECTORY: u16 = 8;
pub const BLOCK_KIND_KV_HASH_BUCKET:    u16 = 9;
// Per-object metadata overflow
pub const BLOCK_KIND_OVERFLOW_RECORD:   u16 = 10;
pub const BLOCK_KIND_COUNT:             u16 = 11;
// Sanity: every BlockKind must be unique and densely numbered from 0.
const _: () = assert!(BLOCK_KIND_OVERFLOW_RECORD + 1 == BLOCK_KIND_COUNT);

// BtreeKind (BtreeNodeHeader.kind, magic = "MIMB")
// Logical radix (current view + per-snapshot history sidecars)
pub const BTREE_KIND_OBJECT_TABLE:            u16 = 0;
pub const BTREE_KIND_OBJECT_HISTORY:          u16 = 1;
pub const BTREE_KIND_LOCATION_TABLE:          u16 = 2;
pub const BTREE_KIND_LOCATION_HISTORY:        u16 = 3;
// Forward index family
pub const BTREE_KIND_FORWARD:                 u16 = 4;
pub const BTREE_KIND_FORWARD_OVERFLOW:        u16 = 5;
// Inverted / range indexes
pub const BTREE_KIND_TAG_DIRECTORY:           u16 = 6;
pub const BTREE_KIND_RANGE:                   u16 = 7;
// KV equality + value storage
pub const BTREE_KIND_KV_DIRECTORY:            u16 = 8;
pub const BTREE_KIND_VALUE_SPILL:             u16 = 9;
// Chunk content-addressing
pub const BTREE_KIND_CHUNK_INDEX:             u16 = 10;
pub const BTREE_KIND_CHUNK_LIST:              u16 = 11;
// Physical reverse mapping
pub const BTREE_KIND_BACKPOINTER:             u16 = 12;
// Catalogs (snapshot-aware)
pub const BTREE_KIND_ONTOLOGY:                u16 = 13;
pub const BTREE_KIND_SUBSCRIPTIONS:           u16 = 14;
// Snapshot tree itself
pub const BTREE_KIND_SNAPSHOTS:               u16 = 15;
// Per-disk physical allocation
pub const BTREE_KIND_BUCKET_ALLOC:            u16 = 16;
pub const BTREE_KIND_FREESPACE_LRU:           u16 = 17;
// Pool / cluster state
pub const BTREE_KIND_DISK_DESCRIPTORS:        u16 = 18;
pub const BTREE_KIND_PLACEMENT_RULES:         u16 = 19;
pub const BTREE_KIND_CLUSTER_PEERS:           u16 = 20;
// Reconcile queues (transient)
pub const BTREE_KIND_RECONCILE_WORK:          u16 = 21;
pub const BTREE_KIND_RECONCILE_HIGH_PRIO:     u16 = 22;
pub const BTREE_KIND_RECONCILE_WORK_PHYS:     u16 = 23;
pub const BTREE_KIND_RECONCILE_HIGH_PRIO_PHYS:u16 = 24;
pub const BTREE_KIND_RECONCILE_PENDING:       u16 = 25;
pub const BTREE_KIND_RECONCILE_SCAN:          u16 = 26;
pub const BTREE_KIND_COUNT:                   u16 = 27;
const _: () = assert!(BTREE_KIND_RECONCILE_SCAN + 1 == BTREE_KIND_COUNT);

// §11.5 — snapshot-aware btrees. Synchronous SnapshotDelete cost is
// `1 + N` WAL entries where N is this count. Doc claim: "7 in the current
// format: the five in §11.2's table plus the two radix sidecars".
pub const SNAPSHOT_AWARE_BTREE_COUNT: usize = 7;
const _: () = {
    // Enumerate them so additions to BtreeKind force a review here.
    let snapshot_aware: [u16; SNAPSHOT_AWARE_BTREE_COUNT] = [
        BTREE_KIND_OBJECT_HISTORY,    // sidecar
        BTREE_KIND_LOCATION_HISTORY,  // sidecar
        BTREE_KIND_FORWARD,
        BTREE_KIND_TAG_DIRECTORY,
        BTREE_KIND_RANGE,
        BTREE_KIND_ONTOLOGY,
        BTREE_KIND_SUBSCRIPTIONS,
    ];
    let _ = snapshot_aware;
};

// BtreeNodeHeader.flags
pub const BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS: u8 = 1 << 0;

// SortedRunHeader.flags
pub const SORTED_RUN_FLAG_PACKED_KEYS: u32 = 1 << 0;
pub const SORTED_RUN_FLAG_ENCRYPTED:   u32 = 1 << 1;

// FieldFormat.flags
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

// LeafEntry.header (§7.1)
pub const LEAF_ENTRY_SPILL_FLAG: u16 = 1 << 15;
pub const LEAF_ENTRY_TOTAL_MASK: u16 = 0x7FFF;
const _: () = assert!(LEAF_ENTRY_SPILL_FLAG | LEAF_ENTRY_TOTAL_MASK == 0xFFFF);
const _: () = assert!(LEAF_ENTRY_SPILL_FLAG & LEAF_ENTRY_TOTAL_MASK == 0);

// SnapshotNode.flags
pub const SNAPSHOT_FLAG_LEAF:    u8 = 1 << 0;
pub const SNAPSHOT_FLAG_DELETED: u8 = 1 << 1;

// BucketAllocEntry.flags
pub const BUCKET_FLAG_NEEDS_DISCARD:      u8 = 1 << 0;
pub const BUCKET_FLAG_PINNED_BY_SNAPSHOT: u8 = 1 << 1;

// WorkItem.flags
pub const WORK_FLAG_RATELIMITED: u32 = 1 << 0;
pub const WORK_FLAG_PERSISTENT:  u32 = 1 << 1;

// =====================================================================
// §2.3 BlockRef — 16 B
// =====================================================================
#[repr(C, packed)]
pub struct BlockRef {
    pub disk_id: u16,
    pub _pad: u16,
    pub block_no: u32,
    pub generation: u64,
}
const _: () = assert!(size_of::<BlockRef>() == 16);

// =====================================================================
// §1.3 BlockPreamble — 8 B (shared by BlockHeader and BtreeNodeHeader)
// =====================================================================
#[repr(C, packed)]
pub struct BlockPreamble {
    pub magic: [u8; 4],          // "MIMR" (4 KiB block) or "MIMB" (256 KiB region)
    pub kind: u16,               // BlockKind or BtreeKind, scoped by magic
    pub format_version: u16,
}
const _: () = assert!(size_of::<BlockPreamble>() == 8);

// =====================================================================
// §1.3 BlockHeader — 32 B
// =====================================================================
#[repr(C, packed)]
pub struct BlockHeader {
    pub pre: BlockPreamble,
    pub payload_length: u32,
    pub generation: u64,
    pub lsn: u64,
    pub flags: u32,
}
const _: () = assert!(size_of::<BlockHeader>() == 32);

// =====================================================================
// §1.5.1 BtreeNodeHeader — 64 B
// =====================================================================
#[repr(C, packed)]
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
// §1.5.1 SortedRunHeader — 32 B
// (bcachefs source calls this `bset` — sorted run = bset = single sorted
// append-only commit unit within a btree node. On-disk magic still "BSET".)
// =====================================================================
#[repr(C, packed)]
pub struct SortedRunHeader {
    pub magic: u32,
    pub seq: u32,
    pub journal_seq: u64,
    pub entry_count: u32,
    pub payload_length: u32,
    pub flags: u32,
    pub crc: u32,
}
const _: () = assert!(size_of::<SortedRunHeader>() == 32);

// =====================================================================
// §1.5.6 SortedRunKeyFormat / FieldFormat
// =====================================================================
#[repr(C, packed)]
pub struct FieldFormat {
    pub bit_width: u8,
    pub flags: u8,
    pub _pad0: u16,
    pub base: u64,
    pub _pad1: u32,                          // tail pad to keep struct multiple-of-8 (§1.1)
}
const _: () = assert!(size_of::<FieldFormat>() == 16);

// (SortedRunKeyFormat is variable-length; spec says 8 + 16 × nr_fields.)

// =====================================================================
// §2.1 ZoneExtent — 24 B
// =====================================================================
#[repr(C, packed)]
pub struct ZoneExtent {
    pub offset: u64,
    pub length: u64,
    pub flags: u32,
    pub _pad: u32,
}
const _: () = assert!(size_of::<ZoneExtent>() == 24);

// =====================================================================
// §2.1 ZoneMap (4 KiB block) and ZoneMapEntry (32 B)
// ZoneMapEntry embeds the full 24 B ZoneExtent so each entry carries
// `flags` for future per-extent hints.
// =====================================================================
#[repr(C, packed)]
pub struct ZoneMapEntry {
    pub zone_kind: u8,                  //  [0..1]
    pub _pad: [u8; 7],                  //  [1..8]   align embedded extent to u64
    pub extent: ZoneExtent,             //  [8..32]  full 24 B form (offset, length, flags, _pad)
}
const _: () = assert!(size_of::<ZoneMapEntry>() == 32);

#[repr(C, packed)]
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
// §2.2 RootPointer — 408 B
// Anchors every persistent btree root atomically (one COW commit flips
// the active superblock root; every tree advances together).
// =====================================================================
#[repr(C, packed)]
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
    pub disks_overflow_root: BlockRef,           // §10.4 — populated iff disk_count > 12
    pub placement_rules_root: BlockRef,          // §10.4 — placement rules btree
    pub cluster_peers_root: BlockRef,            // §10.4 — cluster peers btree
    pub flags: u32,
    pub crc: u32,
}
const _: () = assert!(size_of::<RootPointer>() == 408);

// =====================================================================
// §2.1 Superblock — 4096 B
// Two alternating RootPointer slots (root_a, root_b) for atomic commit;
// the trailing _reserved region absorbs whatever space remains after
// the static layout fields.
// =====================================================================
#[repr(C, packed)]
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
    pub _reserved: [u8; 2980],                  // [1112..4092]
    pub trailing_crc: u32,                      // [4092..4096]
}
const _: () = assert!(size_of::<Superblock>() == 4096);

// =====================================================================
// §3.1 WalHeader — 4096 B (one block; A/B-alternated using
// BlockHeader.generation, no separate seq field)
// =====================================================================
#[repr(C, packed)]
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
// §3.2 WalEntryHeader — 40 B
// HybridTimestamp uses ns resolution (matches every other *_ns field on disk)
// and u16 node_id (matches Superblock.node_id). Sort order: physical_ns →
// logical → node_id (DESIGN §10.4).
// =====================================================================
#[repr(C, packed)]
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

#[repr(C, packed)]
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

// §3.2 / §14 — encrypted WAL entry framing.
// Plaintext entry overhead = WalEntryHeader (40) + framing CRC32C (4) = 44 B.
// Encrypted entry overhead = header (40) + GCM tag (16) + framing CRC32C (4) = 60 B.
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
// §5.1 ObjectRecord — 128 B
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
// §5.2 OverflowRecord per-attribute layout — 32 B (spill) or 112 B (inline)
// `OverflowAttr.flags` is the discriminator that tells a reader how many
// bytes to consume for the body (the spill arm is 16 B, the inline arm
// is 96 B; without the flag the variant is unrecoverable).
// =====================================================================
pub const OVERFLOW_ATTR_FLAG_SPILL: u8 = 1 << 0;

// Placeholder layout for BlobRef (§4.1 / §9.3 — 16 bytes; the doc fixes
// only its size and three logical fields, not the on-disk pad arrangement).
#[repr(C)]
pub struct BlobRef {
    pub disk_id: u16,
    pub _pad: u16,
    pub block_no: u32,
    pub length: u64,
}
const _: () = assert!(size_of::<BlobRef>() == 16);

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
// §6.1 ObjectLocation — 48 B, ReplicaRef — 8 B
// ReplicaRef is bucket-relative, mirroring BackpointerKey's layout
// (§6.2) so move/scrub/resilver paths share field-level conversions.
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
// §6.2 BackpointerKey — 8 B, BackpointerValue — 24 B
// =====================================================================
#[repr(C, packed)]
pub struct BackpointerKey {
    pub disk_id: u16,
    pub bucket_no: u32,
    pub sector_offset: u16,
}
const _: () = assert!(size_of::<BackpointerKey>() == 8);

#[repr(C, packed)]
pub struct BackpointerValue {
    pub owner_kind: u8,
    pub _pad: u8,
    pub length_sectors: u16,
    pub bucket_gen: u32,
    pub owner_key: [u8; 16],
}
const _: () = assert!(size_of::<BackpointerValue>() == 24);

// =====================================================================
// §7.1 PackedAssertion — 16 B
// =====================================================================
#[repr(C, packed)]
pub struct PackedAssertion {
    pub kind: u8,
    pub origin: u8,
    pub _pad: u16,
    pub a: u32,
    pub b: u64,
}
const _: () = assert!(size_of::<PackedAssertion>() == 16);

// =====================================================================
// §8.1 TagIndexLeafEntry — 48 B
// Snapshot-aware (§11.2): key is (tag_id, snapshot). Fields ordered so every
// multi-byte field lands at its natural alignment. The hot key+pointer set
// (store_root through store_kind) all fits within [0..48], a single 64-byte
// cache line.
// =====================================================================
#[repr(C)]
pub struct TagIndexLeafEntry {
    pub last_modify_lsn: u64,    //  8 @  0   (8-aligned)
    pub store_root: BlockRef,    // 16 @  8   (8-aligned; BlockRef contains a u64)
    pub tag_id: u32,             //  4 @ 24   sort key
    pub snapshot: u32,           //  4 @ 28   sort key (suffix); §11.2
    pub cardinality: u32,        //  4 @ 32
    pub generation: u32,         //  4 @ 36
    pub store_kind: u8,          //  1 @ 40
    pub _pad: [u8; 7],           //  7 @ 41
                                 // 48 total, struct alignment = 8
}
const _: () = assert!(size_of::<TagIndexLeafEntry>() == 48);
const _: () = assert!(std::mem::align_of::<TagIndexLeafEntry>() == 8);

// §8.1 leaf packing — verify the entries-per-run claim against the 2-key-field
// SortedRunKeyFormat overhead (8 + 2×16 = 40 B/run, vs 56 for 3-field).
// Per-key on-disk cost: ~3 B packed key (tag_id ~2.5 B + snapshot ~0 bits)
// + ~34 B value (40 B − ~6 B common-prefix elision) = ~37 B.
pub const TAG_DIR_RUN_KEY_FORMAT_2FIELD: usize = 8 + 2 * size_of::<FieldFormat>();
const _: () = assert!(TAG_DIR_RUN_KEY_FORMAT_2FIELD == 40);
pub const TAG_DIR_RUN_OVERHEAD: usize =
    size_of::<SortedRunHeader>() + TAG_DIR_RUN_KEY_FORMAT_2FIELD;
const _: () = assert!(TAG_DIR_RUN_OVERHEAD == 72);
pub const TAG_DIR_PAYLOAD_PER_RUN: usize =
    REGION - size_of::<BtreeNodeHeader>() - TAG_DIR_RUN_OVERHEAD;
const _: () = assert!(TAG_DIR_PAYLOAD_PER_RUN == 262_008);
pub const TAG_DIR_ENTRY_BYTES_PACKED: usize = 37;
pub const TAG_DIR_ENTRIES_PER_RUN: usize =
    TAG_DIR_PAYLOAD_PER_RUN / TAG_DIR_ENTRY_BYTES_PACKED;
const _: () = assert!(TAG_DIR_ENTRIES_PER_RUN == 7_081);

// §11.2 — TagIndexLeafEntry gained 4 bytes of `snapshot` field; with
// 8-byte struct alignment the on-disk size grew from 40 → 48 (4 B field
// + 4 B trailing alignment pad). The snapshot field is at offset 28
// (immediately after the tag_id sort key).
const _: () = assert!(std::mem::offset_of!(TagIndexLeafEntry, snapshot) == 28);
const _: () = assert!(std::mem::offset_of!(TagIndexLeafEntry, tag_id) == 24);

// =====================================================================
// §9.1 KvDirectory — 4096 B (was [BlockRef; 512] which overflowed)
// Capacity: 252 entries; supports global_depth ≤ 7 (128 active buckets).
// Larger pools spill via spillover_root (next-level §1.5 region).
// =====================================================================
#[repr(C, packed)]
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
// §9.1 KvBucket — 4096 B
// =====================================================================
#[repr(C, packed)]
pub struct KvBucketEntry {
    pub tag_id: u32,
    pub value_hash: u64,
    pub bitmap_ref: BlockRef,
}
const _: () = assert!(size_of::<KvBucketEntry>() == 28);

// 4096 - 32 (header) - 1 (local_depth) - 2 (entry_count) - 1 (_pad) - 4 (trailing CRC) = 4056
// 4056 / 28 = 144 entries
// Let's use 144 to maximise.
#[repr(C, packed)]
pub struct KvBucket {
    pub header: BlockHeader,                    //  32
    pub local_depth: u8,                        //   1
    pub entry_count: u16,                       //   2
    pub _pad: u8,                               //   1
    pub entries: [KvBucketEntry; 144],          // 144*28 = 4032
    pub _pad_tail: [u8; 24],                    //  24
    pub trailing_crc: u32,                      //   4
}
const _: () = assert!(size_of::<KvBucket>() == 4096);

// =====================================================================
// §8.3 SequencePage / RankedPage — 4096 B each
// Each page is a 4 KiB block under the standard §1.3 BlockHeader framing,
// with a trailing `next: BlockRef` slot (zero on the tail page) forming
// a singly-linked chain.
// =====================================================================
#[repr(C, packed)]
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

#[repr(C, packed)]
pub struct RankedEntry {                     // 16 B
    pub oid: u64,
    pub score: f32,
    pub _pad: u32,
}
const _: () = assert!(size_of::<RankedEntry>() == 16);

#[repr(C, packed)]
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
// §9.3 ChunkIndexLeafEntry — 56 B
// =====================================================================
#[repr(C, packed)]
pub struct ChunkIndexLeafEntry {
    pub chunk_hash: [u8; 32],
    pub ref_count: u32,
    pub length: u32,
    pub blob: BlockRef,
}
const _: () = assert!(size_of::<ChunkIndexLeafEntry>() == 56);

// =====================================================================
// §9.3 ChunkListEntry — 40 B (positional within ChunkList region)
// Region capacity: (262 144 − 64 − 16) / 40 = 6 551 entries before chaining
// through the trailing BlockRef slot.
// =====================================================================
#[repr(C, packed)]
pub struct ChunkListEntry {
    pub chunk_hash: [u8; 32],   // [0..32]   BLAKE3 of plaintext, indexes ChunkIndex
    pub length: u32,            // [32..36]  plaintext length of this chunk
    pub flags: u32,             // [36..40]  reserved
}
const _: () = assert!(size_of::<ChunkListEntry>() == 40);

// §10.3 — Path projections are encoded as ordinary tag/attribute assertions
// (Value::Scoped { context, inner }; §4.3). Nothing here to validate at the
// layout level; ontology validation handles the type rules.

// =====================================================================
// §10.4 DiskDescriptorOnDisk — 256 B (path: [u8; 192])
// path_len = number of valid UTF-8 bytes in path[]; remainder zero-padded.
// 192 B path covers full /dev/disk/by-id/... names, NVMe-oF discovery URLs,
// long S3/HTTP endpoints, etc.
// =====================================================================
#[repr(C, packed)]
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
// §10.4 PoolStateRoot — 4096 B
// Hybrid disk storage:
//   disk_count ≤ 12: all disks live in inline_disks[0..disk_count];
//                    RootPointer.disks_overflow_root is zero.
//   disk_count > 12: all disks live in the disks-overflow B+ tree
//                    (BtreeKind::DiskDescriptors) keyed by disk_id;
//                    inline_disks[] is unused. Crossing the threshold
//                    migrates the inline contents into the tree.
// =====================================================================
#[repr(C, packed)]
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
// §11.1 SnapshotNode — 64 B
// First-child / next-sibling topology supports arbitrary fan-out at no
// extra per-node cost vs. the previous fixed [u32; 2] children array.
// `ancestor_bitmap: u128` is placed at offset 16 so the u128 hits its
// natural alignment on the hot ancestry-check path.
// =====================================================================
#[repr(C, packed)]
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
// §12.2 BucketAllocEntry — 16 B
// =====================================================================
#[repr(C, packed)]
pub struct BucketAllocEntry {
    pub generation: u32,
    pub data_type: u8,
    pub flags: u8,
    pub dirty_sectors: u16,
    pub last_modify_lsn: u64,
}
const _: () = assert!(size_of::<BucketAllocEntry>() == 16);

// =====================================================================
// §17.2 WorkItem — 48 B
// =====================================================================
#[repr(C, packed)]
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
// §1.5 / §5 / §6.1 Large-node region capacity calculations
// =====================================================================
pub const REGION: usize = 256 * 1024; // 262 144

// §5 depth convention: *depth* counts inner levels above the leaves.
// MAX_LEVELS bounds inner depth (deepest tree = depth 3 = 4 tiers).
pub const MAX_LEVELS: usize = 3;

// Per-flush cost at depth D = (D + 1) × REGION (one rewrite per tier,
// leaf included). Locks down the §5 "COW write path" formula.
pub const fn flush_cost_bytes(depth: usize) -> usize { (depth + 1) * REGION }
const _: () = assert!(flush_cost_bytes(0) == REGION);                 // leaf-only
const _: () = assert!(flush_cost_bytes(1) == 2 * REGION);              // §5 "2 node rewrites" at 10 M
const _: () = assert!(flush_cost_bytes(MAX_LEVELS) == 4 * REGION);     // 48-bit cap

// --- Inner radix node (level ≥ 1): positional BlockRef array, no bitmap.
// Empty slots use BlockRef.generation == 0 as the sentinel.
pub const INNER_PAYLOAD: usize = REGION - size_of::<BtreeNodeHeader>();
pub const INNER_ENTRIES: usize = INNER_PAYLOAD / size_of::<BlockRef>();
const _: () = assert!(INNER_ENTRIES == 16380);
const _: () = assert!(INNER_ENTRIES * size_of::<BlockRef>() + size_of::<BtreeNodeHeader>() == REGION);

// --- Object Table leaf (level 0): N × 128 B records + occupancy bitmap + 32 B trailer.
// Solve for max N such that 64 + ceil(N/8) + 32 + 128*N ≤ 262144 → N ≤ 2045.
// Use N = 2044 with 256-byte bitmap (2048 bits, headroom).
pub const OBJECT_TABLE_LEAF_RECORDS: usize = 2044;
pub const OBJECT_TABLE_BITMAP_BYTES: usize = 256;     // ≥ ceil(2044/8) = 256
pub const OBJECT_TABLE_TRAILER: usize = 32;            // generation, version, reserved
pub const OBJECT_TABLE_USED: usize =
    size_of::<BtreeNodeHeader>()
        + OBJECT_TABLE_BITMAP_BYTES
        + OBJECT_TABLE_TRAILER
        + OBJECT_TABLE_LEAF_RECORDS * size_of::<ObjectRecord>();
const _: () = assert!(OBJECT_TABLE_USED <= REGION);
const _: () = assert!(OBJECT_TABLE_USED == 64 + 256 + 32 + 2044 * 128);
// Trailing pad
pub const OBJECT_TABLE_PAD: usize = REGION - OBJECT_TABLE_USED;
const _: () = assert!(OBJECT_TABLE_PAD == 160);

// --- Location Table leaf (level 0): N × 48 B + bitmap + 32 B trailer.
// 64 + ceil(N/8) + 32 + 48*N ≤ 262144 → N ≤ 5444.
// Use N = 5440 (multiple of 8 → bitmap exactly 680 B, no bit wastage).
pub const LOCATION_TABLE_LEAF_RECORDS: usize = 5440;
pub const LOCATION_TABLE_BITMAP_BYTES: usize = 680;     // 5440/8
pub const LOCATION_TABLE_TRAILER: usize = 32;
pub const LOCATION_TABLE_USED: usize =
    size_of::<BtreeNodeHeader>()
        + LOCATION_TABLE_BITMAP_BYTES
        + LOCATION_TABLE_TRAILER
        + LOCATION_TABLE_LEAF_RECORDS * size_of::<ObjectLocation>();
const _: () = assert!(LOCATION_TABLE_USED <= REGION);
const _: () = assert!(LOCATION_TABLE_USED == 64 + 680 + 32 + 5440 * 48);
pub const LOCATION_TABLE_PAD: usize = REGION - LOCATION_TABLE_USED;
const _: () = assert!(LOCATION_TABLE_PAD == 248);

// =====================================================================
// Capacity sanity checks (depth-vs-objects)
// =====================================================================
const _: () = assert!(OBJECT_TABLE_LEAF_RECORDS * INNER_ENTRIES > 33_000_000); // ≥33 M at depth 2
const _: () = assert!(LOCATION_TABLE_LEAF_RECORDS * INNER_ENTRIES > 89_000_000); // ≥89 M at depth 2

// =====================================================================
// §7.1 Forward-index leaf packing — validate the entries-per-run and
// entries-per-leaf claims against region budget arithmetic.
//
// Per-region budget (§1.5.1, §1.5.6):
//   region:                      256 KiB   = 262 144 B
//   - BtreeNodeHeader:                 64 B
//   - per-run SortedRunHeader:         32 B
//   - per-run SortedRunKeyFormat (2 fields after §11.2: `(oid, snapshot)`,
//                                 §1.5.6: 8 + 2 × FieldFormat):
//                                      8 + 2 × 12 = 32 B
//
// Per-LeafEntry cost (inline body, no spill; §7.1 prose):
//   - packed (oid, snapshot) key: ~2.5 B average (snapshot ~0 bits when one
//     snapshot dominates a sorted run); 3 B integer ceiling for const math
//   - LeafEntry.header:                 2 B
//   - body: assertions × PackedAssertion (16 B each)
// =====================================================================
pub const FWD_RUN_KEY_FORMAT_2FIELD: usize = 8 + 2 * size_of::<FieldFormat>();
const _: () = assert!(FWD_RUN_KEY_FORMAT_2FIELD == 40);

pub const FWD_RUN_OVERHEAD: usize =
    size_of::<SortedRunHeader>() + FWD_RUN_KEY_FORMAT_2FIELD;
const _: () = assert!(FWD_RUN_OVERHEAD == 72);

pub const FWD_ENTRY_KEY_BYTES_CEIL: usize = 3; // ⌈2.5⌉
pub const FWD_ENTRY_HEADER_BYTES:  usize = 2;

pub const fn fwd_entry_bytes(assertions: usize) -> usize {
    FWD_ENTRY_KEY_BYTES_CEIL
        + FWD_ENTRY_HEADER_BYTES
        + assertions * size_of::<PackedAssertion>()
}
// At 8 assertions: 3 + 2 + 128 = 133 B (matches the §7.1 prose "~133 B").
const _: () = assert!(fwd_entry_bytes(8) == 133);

pub const fn fwd_payload_n_runs(runs: usize) -> usize {
    REGION - size_of::<BtreeNodeHeader>() - runs * FWD_RUN_OVERHEAD
}
// One sorted run alone has 262 008 B of entry payload available.
const _: () = assert!(fwd_payload_n_runs(1) == 262_008);
// Four sorted runs share 261 792 B (the §1.5.4 trigger is sorted_run_count > 4).
const _: () = assert!(fwd_payload_n_runs(4) == 261_792);

// Sorted runs are appended into the *same* region (§1.5.2) — they share its
// payload bytes. Adding a run does not multiply capacity; it slightly reduces
// it (by one run's 32 B header + 40 B key-format descriptor = 72 B). The
// total entry count a leaf can hold is therefore bounded by (region − header
// − n×overhead) divided by entry size, not by per-run × n.
pub const fn fwd_total_entries(runs: usize, assertions: usize) -> usize {
    fwd_payload_n_runs(runs) / fwd_entry_bytes(assertions)
}

// At "8 assertions per object" (the §7.2 inline-spill threshold and the
// §7.1 calculation basis), a single-run leaf holds ~1 969 entries
// (the doc prose rounds to "~1 970").
const _: () = assert!(fwd_total_entries(1, 8) == 1_969);

// At the §1.5.4 compaction trigger (sorted_run_count > 4) the leaf still
// holds ~1 968 entries TOTAL — sorted runs share the region's payload
// bytes (§1.5.2), so adding runs costs overhead without adding capacity.
const _: () = assert!(fwd_total_entries(4, 8) == 1_968);

// Lower-assertion regimes (smaller objects pack more densely; monotonic):
const _: () = assert!(fwd_total_entries(1, 4) == 3_797);
const _: () = assert!(fwd_total_entries(4, 4) == 3_794);
const _: () = assert!(fwd_total_entries(1, 4) > fwd_total_entries(1, 8));

// Forward-index footprint at 10 M objects, basis = 8 assertions/entry
// (the §7.2 inline-spill threshold, used as the §7.1 calculation basis).
//
// Tree shape at this scale: depth 2 (root inner + leaves).
//   leaves = ⌈10_000_000 / fwd_total_entries(4, 8)⌉
//   inner  = ⌈leaves / INNER_ENTRIES⌉   (= 1, since leaves « 16 380)
//   total_bytes = (leaves + inner) × REGION
pub const FWD_OBJECTS_10M: usize = 10_000_000;
pub const FWD_LEAVES_10M_K8: usize =
    FWD_OBJECTS_10M.div_ceil(fwd_total_entries(4, 8));
pub const FWD_INNER_10M_K8: usize = FWD_LEAVES_10M_K8.div_ceil(INNER_ENTRIES);
const _: () = assert!(FWD_LEAVES_10M_K8 == 5_082);
const _: () = assert!(FWD_INNER_10M_K8 == 1);

pub const FWD_BYTES_10M_K8: usize = (FWD_LEAVES_10M_K8 + FWD_INNER_10M_K8) * REGION;
// 5 083 × 256 KiB = 1 332 617 152 B ≈ 1.241 GiB.
const _: () = assert!(FWD_BYTES_10M_K8 == 5_083 * 262_144);
// Within [1.24 GiB, 1.25 GiB].
const _: () = assert!(FWD_BYTES_10M_K8 > 1_330_000_000);
const _: () = assert!(FWD_BYTES_10M_K8 < 1_335_000_000);

// For comparison, the small-object regime (4 assertions/entry):
pub const FWD_LEAVES_10M_K4: usize =
    FWD_OBJECTS_10M.div_ceil(fwd_total_entries(4, 4));
pub const FWD_BYTES_10M_K4: usize = (FWD_LEAVES_10M_K4 + 1) * REGION;
const _: () = assert!(FWD_LEAVES_10M_K4 == 2_636);
// 2 637 × 256 KiB ≈ 659 MiB.
const _: () = assert!(FWD_BYTES_10M_K4 > 690_000_000);
const _: () = assert!(FWD_BYTES_10M_K4 < 695_000_000);

// =====================================================================
// §5 / §6.1 Radix-tree depth covers 48-bit local-id space.
// MAX_LEVELS = 3 (inner levels above the leaf); the table in each
// section claims depth 3 reaches ≥ 2^48 ≈ 281 T objects. Asserting
// the inequality nails the §6.1 prose ("48-bit space reachable at
// depth 3").
// =====================================================================
pub const LOCAL_ID_SPACE: u64 = 1u64 << 48;       // 2^48 ≈ 281.5 T
pub const OBJECT_TABLE_DEPTH3_CAP: u64 =
    OBJECT_TABLE_LEAF_RECORDS as u64
        * INNER_ENTRIES as u64
        * INNER_ENTRIES as u64
        * INNER_ENTRIES as u64;
pub const LOCATION_TABLE_DEPTH3_CAP: u64 =
    LOCATION_TABLE_LEAF_RECORDS as u64
        * INNER_ENTRIES as u64
        * INNER_ENTRIES as u64
        * INNER_ENTRIES as u64;
const _: () = assert!(OBJECT_TABLE_DEPTH3_CAP >= LOCAL_ID_SPACE);
const _: () = assert!(LOCATION_TABLE_DEPTH3_CAP >= LOCAL_ID_SPACE);
// Depth 2 alone is NOT enough for the location table (would have caught
// the §6.1 "depth 2" prose bug):
pub const LOCATION_TABLE_DEPTH2_CAP: u64 =
    LOCATION_TABLE_LEAF_RECORDS as u64 * INNER_ENTRIES as u64 * INNER_ENTRIES as u64;
const _: () = assert!(LOCATION_TABLE_DEPTH2_CAP < LOCAL_ID_SPACE);

// =====================================================================
// §16 footprint: 10 M-object leaf counts (positional radix tables).
// Pinning these against the doc's §16 figures ensures off-by-one in the
// leaf count is caught at build time.
// =====================================================================
pub const OBJECT_TABLE_LEAVES_10M: usize =
    FWD_OBJECTS_10M.div_ceil(OBJECT_TABLE_LEAF_RECORDS);
pub const LOCATION_TABLE_LEAVES_10M: usize =
    FWD_OBJECTS_10M.div_ceil(LOCATION_TABLE_LEAF_RECORDS);
const _: () = assert!(OBJECT_TABLE_LEAVES_10M == 4_893);    // §16: 4 893 leaves
const _: () = assert!(LOCATION_TABLE_LEAVES_10M == 1_839);  // §16: 1 839 leaves

// =====================================================================
// §3.2 hard cap: a single WAL op fits in one 4 KiB sector. No continuation
// framing — bulk data goes through the blob zone (§4.1) and is referenced
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

// §3.3 variable-length op-field caps that keep every op under WAL_OP_MAX_PAYLOAD
// even with CBOR overhead. Sized so the largest op (Checkpoint with a 424 B
// RootPointer, or SnapshotCreate with a 256 B label) stays comfortably below
// the sector bound. owner_key is naturally bounded by §6.2's 16 B BackpointerValue.
pub const WAL_LABEL_MAX_BYTES:      usize = 256;
pub const WAL_CURSOR_KEY_MAX_BYTES: usize = 256;
pub const WAL_OWNER_KEY_BYTES:      usize = 16;     // §6.2 BackpointerValue.owner_key
const _: () = assert!(WAL_LABEL_MAX_BYTES      < WAL_OP_MAX_PAYLOAD);
const _: () = assert!(WAL_CURSOR_KEY_MAX_BYTES < WAL_OP_MAX_PAYLOAD);

// =====================================================================
// §7.2 ForwardOverflow region: positional PackedAssertion array minus
// trailing-chain BlockRef. Region capacity claim: 16 379 entries.
// =====================================================================
pub const FORWARD_OVERFLOW_PAYLOAD: usize =
    REGION - size_of::<BtreeNodeHeader>() - size_of::<BlockRef>();
pub const FORWARD_OVERFLOW_ENTRIES: usize =
    FORWARD_OVERFLOW_PAYLOAD / size_of::<PackedAssertion>();
const _: () = assert!(FORWARD_OVERFLOW_PAYLOAD == 262_064);
const _: () = assert!(FORWARD_OVERFLOW_ENTRIES == 16_379);

// =====================================================================
// §9.3 ChunkList region: positional ChunkListEntry array minus
// trailing-chain BlockRef. Region capacity claim: 6 551 entries.
// =====================================================================
pub const CHUNK_LIST_PAYLOAD: usize =
    REGION - size_of::<BtreeNodeHeader>() - size_of::<BlockRef>();
pub const CHUNK_LIST_ENTRIES: usize =
    CHUNK_LIST_PAYLOAD / size_of::<ChunkListEntry>();
const _: () = assert!(CHUNK_LIST_PAYLOAD == 262_064);
const _: () = assert!(CHUNK_LIST_ENTRIES == 6_551);

// =====================================================================
// §9.1 KvDirectory inline capacity: 252 entries supports global_depth ≤ 7.
// At global_depth ≥ 8 the directory spills into a §1.5 region holding
// up to INNER_ENTRIES (16 380) BlockRef entries, supporting global_depth ≤ 13.
// =====================================================================
pub const KV_INLINE_ENTRIES: usize = 252;
pub const KV_INLINE_GLOBAL_DEPTH_MAX: u32 = 7;
const _: () = assert!((1u32 << KV_INLINE_GLOBAL_DEPTH_MAX) <= KV_INLINE_ENTRIES as u32);
// One step further would overflow:
const _: () = assert!((1u32 << (KV_INLINE_GLOBAL_DEPTH_MAX + 1)) > KV_INLINE_ENTRIES as u32);
pub const KV_SPILLOVER_GLOBAL_DEPTH_MAX: u32 = 13;
const _: () = assert!((1usize << KV_SPILLOVER_GLOBAL_DEPTH_MAX) <= INNER_ENTRIES);
const _: () = assert!((1usize << (KV_SPILLOVER_GLOBAL_DEPTH_MAX + 1)) > INNER_ENTRIES);

// =====================================================================
// §7.1 Forward-index inner-node packing (sanity check the prose's
// "~13 000 children per run" claim).
//
// Inner entry: packed (oid, snapshot) key (~3 B) + BlockRef (16 B) ≈ 19 B.
// Per-run payload uses the same 2-field SortedRunKeyFormat as the leaf.
// =====================================================================
pub const FWD_INNER_ENTRY_BYTES: usize = FWD_ENTRY_KEY_BYTES_CEIL + size_of::<BlockRef>();
const _: () = assert!(FWD_INNER_ENTRY_BYTES == 19);
pub const FWD_INNER_CHILDREN_PER_RUN: usize =
    fwd_payload_n_runs(1) / FWD_INNER_ENTRY_BYTES;
// 262 008 / 19 = 13 789 — pin the doc's tightened "~13 789" figure.
const _: () = assert!(FWD_INNER_CHILDREN_PER_RUN == 13_789);

// =====================================================================
// §9.2 Range-index leaf packing (4-field key: attr_id, value, oid, snapshot).
// Per-run overhead = SortedRunHeader (32) + SortedRunKeyFormat (8 + 4×16 = 72) = 104 B.
// Per-entry on-disk cost = 8–12 B packed key + 16 B BlockRef value = 24–28 B.
// =====================================================================
pub const RANGE_RUN_KEY_FORMAT_4FIELD: usize = 8 + 4 * size_of::<FieldFormat>();
const _: () = assert!(RANGE_RUN_KEY_FORMAT_4FIELD == 72);
pub const RANGE_RUN_OVERHEAD: usize =
    size_of::<SortedRunHeader>() + RANGE_RUN_KEY_FORMAT_4FIELD;
const _: () = assert!(RANGE_RUN_OVERHEAD == 104);
pub const RANGE_PAYLOAD_PER_RUN: usize =
    REGION - size_of::<BtreeNodeHeader>() - RANGE_RUN_OVERHEAD;
const _: () = assert!(RANGE_PAYLOAD_PER_RUN == 261_976);
// Best/worst entries-per-run bounds (24–28 B/entry):
pub const RANGE_ENTRIES_PER_RUN_BEST: usize = RANGE_PAYLOAD_PER_RUN / 24;
pub const RANGE_ENTRIES_PER_RUN_WORST: usize = RANGE_PAYLOAD_PER_RUN / 28;
// Pin the prose's "~9 350–10 900 entries" range.
const _: () = assert!(RANGE_ENTRIES_PER_RUN_BEST  >= 10_900 && RANGE_ENTRIES_PER_RUN_BEST  < 11_000);
const _: () = assert!(RANGE_ENTRIES_PER_RUN_WORST >=  9_350 && RANGE_ENTRIES_PER_RUN_WORST <  9_500);

// =====================================================================
// §12.4 Freespace LRU fragmentation_band: ⌈255 × dirty / sectors⌉
// Endpoints:
//   band == 0   iff dirty == 0 (empty bucket — full free)
//   band == 255 when dirty is full *or near-full* (the formula saturates
//               for the last few sectors because ⌈255·d/s⌉ rounds up)
// Intermediate dirty values map into 1..=254. Reorder cost is bounded by
// allocation pressure, not write volume: bands are recomputed lazily when
// dirty_sectors crosses an 8-sector boundary (§12.4 prose).
// =====================================================================
pub const fn fragmentation_band(dirty: u64, sectors_per_bucket: u64) -> u8 {
    let band = (255u64 * dirty).div_ceil(sectors_per_bucket);
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
// §1.5.5 depth table (10 M objects, 5000 tags, 100 snapshots) — pin the
// concrete tier counts cited in the prose. Forward leaves are computed
// at the §7.2 inline-spill threshold (8 assertions/object).
// =====================================================================
const _: () = assert!(OBJECT_TABLE_LEAVES_10M   == 4_893);    // depth 1, fits under one inner
const _: () = assert!(OBJECT_TABLE_LEAVES_10M   <= INNER_ENTRIES);
const _: () = assert!(LOCATION_TABLE_LEAVES_10M == 1_839);
const _: () = assert!(LOCATION_TABLE_LEAVES_10M <= INNER_ENTRIES);
const _: () = assert!(FWD_LEAVES_10M_K8         == 5_082);
const _: () = assert!(FWD_LEAVES_10M_K8         <= INNER_ENTRIES);
// Tag directory at 5000 tags × 100 snapshots (no divergence): single leaf.
const _: () = assert!(5_000 <= TAG_DIR_ENTRIES_PER_RUN);
