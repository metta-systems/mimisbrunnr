//! Compile-time size assertions mirroring docs/IMPLEMENTATION.md.
//! Each `const _` assertion fails to compile if the size is wrong.

#![allow(dead_code)]

use std::mem::size_of;

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
    pub bset_count: u8,
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
// §1.5.1 BsetHeader — 32 B
// =====================================================================
#[repr(C, packed)]
pub struct BsetHeader {
    pub magic: u32,
    pub seq: u32,
    pub journal_seq: u64,
    pub entry_count: u32,
    pub payload_length: u32,
    pub flags: u32,
    pub crc: u32,
}
const _: () = assert!(size_of::<BsetHeader>() == 32);

// =====================================================================
// §1.5.6 BsetKeyFormat / FieldFormat
// =====================================================================
#[repr(C, packed)]
pub struct FieldFormat {
    pub bit_width: u8,
    pub flags: u8,
    pub _pad: u16,
    pub base: u64,
}
const _: () = assert!(size_of::<FieldFormat>() == 12);

// (BsetKeyFormat is variable-length; spec says 8 + 12 × nr_fields.)

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
// §2.1 ZoneMap (4 KiB block) and ZoneMapEntry (24 B)
// =====================================================================
#[repr(C, packed)]
pub struct ZoneMapEntry {
    pub zone_kind: u8,
    pub _pad: [u8; 7],
    pub extent_offset: u64,
    pub extent_length: u64,
}
const _: () = assert!(size_of::<ZoneMapEntry>() == 24);

#[repr(C, packed)]
pub struct ZoneMap {
    pub header: BlockHeader,            //   32
    pub extent_count: u16,              //    2
    pub _pad: [u8; 6],                  //    6
    pub extents: [ZoneMapEntry; 168],   // 4032
    pub _pad_tail: [u8; 20],            //   20
    pub trailing_crc: u32,              //    4
}
const _: () = assert!(size_of::<ZoneMap>() == 4096);

// =====================================================================
// §2.2 RootPointer — 376 B (revised)
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
    pub path_context_root: BlockRef,
    pub subscriptions_root: BlockRef,
    pub pool_state_root: BlockRef,
    pub snapshot_chain_root: BlockRef,
    pub reconcile_work_root: BlockRef,
    pub reconcile_hipri_root: BlockRef,
    pub reconcile_work_phys_root: BlockRef,
    pub reconcile_hipri_phys_root: BlockRef,
    pub reconcile_pending_root: BlockRef,
    pub reconcile_scan_root: BlockRef,
    pub flags: u32,
    pub crc: u32,
}
const _: () = assert!(size_of::<RootPointer>() == 376);

// =====================================================================
// §2.1 Superblock — 4096 B
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
    pub root_a: RootPointer,                    // [112..488]
    pub root_b: RootPointer,                    // [488..864]
    pub active_root: u8,                        // [864..865]
    pub _pad2: [u8; 7],                         // [865..872]
    pub wal_offset: u64,                        // [872..880]
    pub wal_size: u64,                          // [880..888]
    pub bucket_size_log2: u8,                   // [888..889]
    pub copygc_reserve_pct: u8,                 // [889..890]
    pub btree_node_size_log2: u8,               // [890..891]
    pub _pad3: [u8; 5],                         // [891..896]
    pub bootstrap_buckets: u32,                 // [896..900]
    pub _pad4: [u8; 4],                         // [900..904]
    pub zone_map_offset: u64,                   // [904..912]
    pub index_zone: ZoneExtent,                 // [912..936]
    pub metadata_zone: ZoneExtent,              // [936..960]
    pub blob_zone: ZoneExtent,                  // [960..984]
    pub encryption_keyid: [u8; 16],             // [984..1000]
    pub fs_format_version: u32,                 // [1000..1004]
    pub fs_min_on_disk: u32,                    // [1004..1008]
    pub compat_features: u64,                   // [1008..1016]
    pub ro_compat_features: u64,                // [1016..1024]
    pub incompat_features: u64,                 // [1024..1032]
    pub downgrade_log_ref: BlockRef,            // [1032..1048]
    pub _reserved: [u8; 3044],                  // [1048..4092]
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
    pub last_checkpoint_lsn: u64,       //    8
    pub last_checkpoint_offset: u64,    //    8
    pub segment_size: u32,              //    4
    pub _pad: u32,                      //    4
    pub encryption_keyid: [u8; 16],     //   16
    pub _reserved: [u8; 3988],          // 3988
    pub trailing_crc: u32,              //    4
}
const _: () = assert!(size_of::<WalHeader>() == 4096);

// =====================================================================
// §3.2 WalEntryHeader — 40 B (was claimed 32 B)
// =====================================================================
#[repr(C, packed)]
pub struct HybridTimestamp {
    pub physical_ns: i64,
    pub logical: u32,
    pub node_id: u32,
}
const _: () = assert!(size_of::<HybridTimestamp>() == 16);

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
    pub _pad0: u16,
    pub inline_tags: [u32; 4],
    pub overflow_offset: u64,
    pub stored_size: u64,
    pub last_modify_lsn: u64,
}
const _: () = assert!(size_of::<ObjectRecord>() == 128);

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
// §6.2 BackpointerKey — 12 B, BackpointerValue — 24 B
// =====================================================================
#[repr(C, packed)]
pub struct BackpointerKey {
    pub disk_id: u16,
    pub bucket_no: u32,
    pub sector_offset: u32,
    pub _pad: u16,
}
const _: () = assert!(size_of::<BackpointerKey>() == 12);

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
// §8.1 TagIndexLeafEntry — 40 B
// Fields reordered so every multi-byte field lands at its natural alignment.
// (tag_id, store_root) sit on the same 32-byte half-cache-line, so a single
// 64-byte fetch on tag-id lookup also brings cardinality and generation in.
// =====================================================================
#[repr(C)]
pub struct TagIndexLeafEntry {
    pub last_modify_lsn: u64,    //  8 @  0   (8-aligned)
    pub store_root: BlockRef,    // 16 @  8   (8-aligned; BlockRef contains a u64)
    pub tag_id: u32,             //  4 @ 24
    pub cardinality: u32,        //  4 @ 28
    pub generation: u32,         //  4 @ 32
    pub store_kind: u8,          //  1 @ 36
    pub _pad: [u8; 3],           //  3 @ 37
                                 // 40 total, struct alignment = 8
}
const _: () = assert!(size_of::<TagIndexLeafEntry>() == 40);
const _: () = assert!(std::mem::align_of::<TagIndexLeafEntry>() == 8);

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
// §10.3 PathContextHeader — 48 B
// =====================================================================
#[repr(C)]
pub struct PathContextHeader {
    pub name_offset: u32,
    pub name_len: u16,
    pub flags: u16,
    pub manifest_root: BlockRef,
    pub entry_count: u64,
    pub last_refresh_ns: i64,
    pub last_modify_lsn: u64,
}
const _: () = assert!(size_of::<PathContextHeader>() == 48);

// =====================================================================
// §10.4 DiskDescriptorOnDisk — 64 B
// =====================================================================
#[repr(C, packed)]
pub struct DiskDescriptorOnDisk {
    pub disk_id: u16,
    pub media_type: u8,
    pub tier: u8,
    pub state: u8,
    pub _pad: u8,
    pub path_offset: u16,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub bucket_count: u32,
    pub first_usable_bucket: u32,
    pub buckets_root: BlockRef,
    pub freespace_root: BlockRef,
}
const _: () = assert!(size_of::<DiskDescriptorOnDisk>() == 64);

// =====================================================================
// §11.1 SnapshotNode — 64 B
// First-child / next-sibling topology supports arbitrary fan-out at no
// extra per-node cost vs. the previous fixed [u32; 2] children array.
// =====================================================================
#[repr(C, packed)]
pub struct SnapshotNode {
    pub id: u32,
    pub parent: u32,
    pub first_child: u32,
    pub next_sibling: u32,
    pub depth: u16,
    pub flags: u8,
    pub _pad: u8,
    pub ancestor_bitmap: u128,
    pub skiplist: [u32; 3],
    pub created_ns: i64,
    pub label_offset: u32,
    pub _reserved: u32,
}
const _: () = assert!(size_of::<SnapshotNode>() == 64);

// =====================================================================
// §12.2 BucketAllocKey — 16 B
// =====================================================================
#[repr(C, packed)]
pub struct BucketAllocKey {
    pub generation: u32,
    pub data_type: u8,
    pub flags: u8,
    pub dirty_sectors: u16,
    pub last_modify_lsn: u64,
}
const _: () = assert!(size_of::<BucketAllocKey>() == 16);

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
