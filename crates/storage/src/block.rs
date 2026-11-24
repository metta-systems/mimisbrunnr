//! Per-block headers and kind enumerations.
//!
//! Implements FORMAT-DESIGN§1.3.

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
};

use crate::error::StorageError;

/// Logical block size — 4 KiB.
///
/// FORMAT-DESIGN§1.2.
pub const BLOCK_SIZE: usize = 4096;

/// `log2(BLOCK_SIZE)`.
pub const BLOCK_SIZE_LOG2: u8 = 12;

/// Magic for 4 KiB block headers (`MIMR`).
pub const BLOCK_PREAMBLE_MAGIC_BLOCK: [u8; 4] = *b"MIMR";

/// Magic for 256 KiB B+ tree / radix region headers (`MIMB`).
pub const BLOCK_PREAMBLE_MAGIC_BTREE: [u8; 4] = *b"MIMB";

// ---------- BlockPreamble ----------

/// 8-byte common preamble shared by [`BlockHeader`] (4 KiB blocks) and
/// `BtreeNodeHeader` (256 KiB regions). FORMAT-DESIGN§1.3.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct BlockPreamble {
    /// `"MIMR"` for 4 KiB blocks, `"MIMB"` for 256 KiB regions.
    pub magic: [u8; 4], // [0..4]
    /// `BlockKind` (when `magic == "MIMR"`) or `BtreeKind` (when `"MIMB"`).
    pub kind: u16, // [4..6]
    /// Per-kind structural format version.
    pub format_version: u16, // [6..8]
}

const_assert_eq!(core::mem::size_of::<BlockPreamble>(), 8);

// ---------- BlockHeader ----------

/// Flag: payload encrypted (XTS-AES-256). Cipher integration is a later phase.
pub const BLOCK_FLAG_ENCRYPTED: u32 = 1 << 0;
/// Flag: continuation of a chained record.
pub const BLOCK_FLAG_CONTINUATION: u32 = 1 << 1;

/// 32-byte header that prefixes every persistent 4 KiB block.
///
/// TODO(ENGINE4): `BlockHeader`, `BlockKind` and `BlockPreamble`
/// go with their last users; every header begins with the aligned
/// [`crate::Preamble`] and ends with a 64-bit checksum slot (FORMAT-DESIGN§1.3–1.4).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct BlockHeader {
    /// 8-byte common preamble.
    pub pre: BlockPreamble, // [0..8]
    /// Bytes following the header (excl. trailing CRC). For 4 KiB blocks, < 4096.
    pub payload_length: u32, // [8..12]
    /// Monotonic per-block generation, for COW; little-endian `u64` at a
    /// 4-aligned offset.
    pub generation: [u8; 8], // [12..20]
    /// WAL LSN that produced this block; little-endian `u64`.
    pub lsn: [u8; 8], // [20..28]
    /// `BLOCK_FLAG_*` bits.
    pub flags: u32, // [28..32]
}

const_assert_eq!(core::mem::size_of::<BlockHeader>(), 32);

impl BlockHeader {
    /// Build a fresh `BlockHeader` for a block of `kind` with `payload_length`
    /// payload bytes.
    pub fn new(kind: BlockKind, format_version: u16, payload_length: u32) -> Self {
        Self {
            pre: BlockPreamble {
                magic: BLOCK_PREAMBLE_MAGIC_BLOCK,
                kind: kind as u16,
                format_version,
            },
            payload_length,
            generation: [0; 8],
            lsn: [0; 8],
            flags: 0,
        }
    }

    /// The LSN that produced this block.
    pub fn lsn(&self) -> u64 {
        u64::from_le_bytes(self.lsn)
    }

    /// The block's copy-on-write generation.
    pub fn generation(&self) -> u64 {
        u64::from_le_bytes(self.generation)
    }

    pub fn set_generation(&mut self, generation: u64) {
        self.generation = generation.to_le_bytes();
    }
}

// ---------- BlockKind ----------

/// `BlockKind` enumerates the small (4 KiB) block types. **Discriminants are
/// pinned by the spec — do not renumber.** FORMAT-DESIGN§1.3.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockKind {
    Superblock = 0,
    ZoneMap = 1,
    PoolStateRoot = 2,
    WalSegment = 3,
    Checkpoint = 4,
    TagBitmapPage = 5,
    SequencePage = 6,
    RankedPage = 7,
    KvHashDirectory = 8,
    KvHashBucket = 9,
    OverflowRecord = 10,
}

impl BlockKind {
    /// Decode a raw u16 into a `BlockKind`. Returns
    /// [`StorageError::InvalidBlockKind`] on unknown discriminants.
    pub fn from_u16(value: u16) -> Result<Self, StorageError> {
        Ok(match value {
            0 => Self::Superblock,
            1 => Self::ZoneMap,
            2 => Self::PoolStateRoot,
            3 => Self::WalSegment,
            4 => Self::Checkpoint,
            5 => Self::TagBitmapPage,
            6 => Self::SequencePage,
            7 => Self::RankedPage,
            8 => Self::KvHashDirectory,
            9 => Self::KvHashBucket,
            10 => Self::OverflowRecord,
            other => return Err(StorageError::InvalidBlockKind(other)),
        })
    }
}

// ---------- BtreeKind ----------

/// The tree a region belongs to, carried in its preamble's `kind`.
/// **Discriminants 0–26 are pinned by FORMAT-DESIGN§1.3.**
///
/// The four kinds from `0x8000` up are not in the format: their trees are
/// replaced by the spec's own, and each kind goes with its tree.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BtreeKind {
    // Object metadata
    ObjectRoots = 0,
    ObjectTable = 1,
    ObjectHistory = 2,
    ContentMap = 3,
    // Term index
    Forward = 4,
    ForwardOverflow = 5,
    TermDictionary = 6,
    TermTable = 7,
    Postings = 8,
    Sequences = 9,
    // Deduplication
    ChunkIndex = 10,
    // Physical space
    Backpointer = 11,
    BucketAlloc = 12,
    FreespaceLru = 13,
    // Ontology and snapshots
    Ontology = 14,
    Snapshots = 15,
    // Subscriptions
    Subscriptions = 16,
    SubscriptionMembers = 17,
    SubscriptionEvents = 18,
    // Pool and cluster
    PoolState = 19,
    DiskDescriptors = 20,
    PlacementRules = 21,
    ClusterPeers = 22,
    Transfers = 23,
    // Reconcile
    ReconcileWork = 24,
    ReconcilePending = 25,
    ReconcileScan = 26,
    /// TODO(METADATA7): goes with the location table.
    LocationTable = 0x8000,
    /// TODO(INDEX13): goes with the tag index, replaced by `Postings`.
    TagDirectory = 0x8001,
    /// TODO(INDEX12): goes with the range index, replaced by `TermDictionary`.
    Range = 0x8002,
    /// TODO(INDEX12): goes with the kv index, replaced by `TermDictionary`.
    KvDirectory = 0x8003,
}

impl BtreeKind {
    /// Decode a raw u16 into a `BtreeKind`.
    pub fn from_u16(value: u16) -> Result<Self, StorageError> {
        Ok(match value {
            0 => Self::ObjectRoots,
            1 => Self::ObjectTable,
            2 => Self::ObjectHistory,
            3 => Self::ContentMap,
            4 => Self::Forward,
            5 => Self::ForwardOverflow,
            6 => Self::TermDictionary,
            7 => Self::TermTable,
            8 => Self::Postings,
            9 => Self::Sequences,
            10 => Self::ChunkIndex,
            11 => Self::Backpointer,
            12 => Self::BucketAlloc,
            13 => Self::FreespaceLru,
            14 => Self::Ontology,
            15 => Self::Snapshots,
            16 => Self::Subscriptions,
            17 => Self::SubscriptionMembers,
            18 => Self::SubscriptionEvents,
            19 => Self::PoolState,
            20 => Self::DiskDescriptors,
            21 => Self::PlacementRules,
            22 => Self::ClusterPeers,
            23 => Self::Transfers,
            24 => Self::ReconcileWork,
            25 => Self::ReconcilePending,
            26 => Self::ReconcileScan,
            0x8000 => Self::LocationTable,
            0x8001 => Self::TagDirectory,
            0x8002 => Self::Range,
            0x8003 => Self::KvDirectory,
            other => return Err(StorageError::InvalidBtreeKind(other)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_preamble_size_is_eight() {
        assert_eq!(core::mem::size_of::<BlockPreamble>(), 8);
    }

    #[test]
    fn block_header_size_is_thirty_two() {
        assert_eq!(core::mem::size_of::<BlockHeader>(), 32);
    }

    #[test]
    fn block_kind_round_trips() {
        for raw in 0u16..=10 {
            let k = BlockKind::from_u16(raw).unwrap();
            assert_eq!(k as u16, raw);
        }
        assert!(BlockKind::from_u16(99).is_err());
    }

    #[test]
    fn btree_kind_round_trips() {
        for raw in (0u16..=26).chain(0x8000..=0x8003) {
            let k = BtreeKind::from_u16(raw).unwrap();
            assert_eq!(k as u16, raw);
        }
        assert!(BtreeKind::from_u16(27).is_err());
        assert!(BtreeKind::from_u16(0x8004).is_err());
    }

    #[test]
    fn btree_kind_spec_discriminants_pinned() {
        // FORMAT-DESIGN§1.3, contiguous and grouped by subsystem.
        assert_eq!(BtreeKind::ObjectRoots as u16, 0);
        assert_eq!(BtreeKind::ContentMap as u16, 3);
        assert_eq!(BtreeKind::Forward as u16, 4);
        assert_eq!(BtreeKind::Postings as u16, 8);
        assert_eq!(BtreeKind::ChunkIndex as u16, 10);
        assert_eq!(BtreeKind::BucketAlloc as u16, 12);
        assert_eq!(BtreeKind::Ontology as u16, 14);
        assert_eq!(BtreeKind::PoolState as u16, 19);
        assert_eq!(BtreeKind::ClusterPeers as u16, 22);
        assert_eq!(BtreeKind::Transfers as u16, 23);
        assert_eq!(BtreeKind::ReconcileScan as u16, 26);
    }
}
