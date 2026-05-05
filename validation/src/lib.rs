//! Compile-time size assertions mirroring docs/IMPLEMENTATION.md.
//! Each `const _` assertion fails to compile if the size is wrong.

#![allow(dead_code)]

use std::mem::size_of;

// =====================================================================
// Symbolic flag constants — mirrors §1.3, §1.5.1, §1.5.6, §3.1, §3.2,
// §5.1, §6.1, §7.1, §10.3, §11.1, §12.2, §17.2.
// Asserted-by-existence: any code referring to these names compiles iff
// the constants stay defined here.
// =====================================================================

// BlockHeader.flags
pub const BLOCK_FLAG_ENCRYPTED:    u32 = 1 << 0;
pub const BLOCK_FLAG_CONTINUATION: u32 = 1 << 1;

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

// PathContextHeader.flags
pub const PATH_CONTEXT_FLAG_READ_ONLY: u16 = 1 << 0;
pub const PATH_CONTEXT_FLAG_EPHEMERAL: u16 = 1 << 1;

// SnapshotNode.flags
pub const SNAPSHOT_FLAG_LEAF:    u8 = 1 << 0;
pub const SNAPSHOT_FLAG_DELETED: u8 = 1 << 1;

// BucketAllocKey.flags
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
// §2.2 RootPointer — 424 B
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
    pub path_context_root: BlockRef,
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
const _: () = assert!(size_of::<RootPointer>() == 424);

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
    pub root_a: RootPointer,                    // [112..536]
    pub root_b: RootPointer,                    // [536..960]
    pub active_root: u8,                        // [960..961]
    pub _pad2: [u8; 7],                         // [961..968]
    pub wal_offset: u64,                        // [968..976]
    pub wal_size: u64,                          // [976..984]
    pub bucket_size_log2: u8,                   // [984..985]
    pub copygc_reserve_pct: u8,                 // [985..986]
    pub btree_node_size_log2: u8,               // [986..987]
    pub _pad3: [u8; 5],                         // [987..992]
    pub bootstrap_buckets: u32,                 // [992..996]
    pub _pad4: [u8; 4],                         // [996..1000]
    pub zone_map_offset: u64,                   // [1000..1008]
    pub index_zone: ZoneExtent,                 // [1008..1032]
    pub metadata_zone: ZoneExtent,              // [1032..1056]
    pub blob_zone: ZoneExtent,                  // [1056..1080]
    pub encryption_keyid: [u8; 16],             // [1080..1096]
    pub fs_format_version: u32,                 // [1096..1100]
    pub fs_min_on_disk: u32,                    // [1100..1104]
    pub compat_features: u64,                   // [1104..1112]
    pub ro_compat_features: u64,                // [1112..1120]
    pub incompat_features: u64,                 // [1120..1128]
    pub downgrade_log_ref: BlockRef,            // [1128..1144]
    pub _reserved: [u8; 2948],                  // [1144..4092]
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
// §3.2 WalEntryHeader — 40 B
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
