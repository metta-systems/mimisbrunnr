//! Per-block headers and kind enumerations.
//!
//! Implements IMPL §1.3.

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
};

use crate::error::StorageError;

/// Logical block size — 4 KiB.
///
/// IMPL §1.2.
pub const BLOCK_SIZE: usize = 4096;

/// `log2(BLOCK_SIZE)`.
pub const BLOCK_SIZE_LOG2: u8 = 12;

/// Magic for 4 KiB block headers (`MIMR`).
pub const BLOCK_PREAMBLE_MAGIC_BLOCK: [u8; 4] = *b"MIMR";

/// Magic for 256 KiB B+ tree / radix region headers (`MIMB`).
pub const BLOCK_PREAMBLE_MAGIC_BTREE: [u8; 4] = *b"MIMB";

// ---------- BlockPreamble ----------

/// 8-byte common preamble shared by [`BlockHeader`] (4 KiB blocks) and
/// `BtreeNodeHeader` (256 KiB regions). IMPL §1.3.
#[repr(C, packed)]
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

/// 32-byte header that prefixes every persistent 4 KiB block. IMPL §1.3.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct BlockHeader {
    /// 8-byte common preamble.
    pub pre: BlockPreamble, // [0..8]
    /// Bytes following the header (excl. trailing CRC). For 4 KiB blocks, < 4096.
    pub payload_length: u32, // [8..12]
    /// Monotonic per-block generation, for COW.
    pub generation: u64, // [12..20]
    /// WAL LSN that produced this block.
    pub lsn: u64, // [20..28]
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
            generation: 0,
            lsn: 0,
            flags: 0,
        }
    }
}

// ---------- BlockKind ----------

/// `BlockKind` enumerates the small (4 KiB) block types. **Discriminants are
/// pinned by the spec — do not renumber.** IMPL §1.3.
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

/// `BtreeKind` enumerates the large-region (256 KiB) types. **Discriminants
/// are pinned by the spec — do not renumber.** IMPL §1.3.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BtreeKind {
    ObjectTable = 0,
    ObjectHistory = 1,
    LocationTable = 2,
    LocationHistory = 3,
    Forward = 4,
    ForwardOverflow = 5,
    TagDirectory = 6,
    Range = 7,
    KvDirectory = 8,
    ValueSpill = 9,
    ChunkIndex = 10,
    ChunkList = 11,
    Backpointer = 12,
    Ontology = 13,
    Subscriptions = 14,
    Snapshots = 15,
    BucketAlloc = 16,
    FreespaceLru = 17,
    DiskDescriptors = 18,
    PlacementRules = 19,
    ClusterPeers = 20,
    ReconcileWork = 21,
    ReconcileHighPrio = 22,
    ReconcileWorkPhys = 23,
    ReconcileHighPrioPhys = 24,
    ReconcilePending = 25,
    ReconcileScan = 26,
}

impl BtreeKind {
    /// Decode a raw u16 into a `BtreeKind`.
    pub fn from_u16(value: u16) -> Result<Self, StorageError> {
        Ok(match value {
            0 => Self::ObjectTable,
            1 => Self::ObjectHistory,
            2 => Self::LocationTable,
            3 => Self::LocationHistory,
            4 => Self::Forward,
            5 => Self::ForwardOverflow,
            6 => Self::TagDirectory,
            7 => Self::Range,
            8 => Self::KvDirectory,
            9 => Self::ValueSpill,
            10 => Self::ChunkIndex,
            11 => Self::ChunkList,
            12 => Self::Backpointer,
            13 => Self::Ontology,
            14 => Self::Subscriptions,
            15 => Self::Snapshots,
            16 => Self::BucketAlloc,
            17 => Self::FreespaceLru,
            18 => Self::DiskDescriptors,
            19 => Self::PlacementRules,
            20 => Self::ClusterPeers,
            21 => Self::ReconcileWork,
            22 => Self::ReconcileHighPrio,
            23 => Self::ReconcileWorkPhys,
            24 => Self::ReconcileHighPrioPhys,
            25 => Self::ReconcilePending,
            26 => Self::ReconcileScan,
            other => return Err(StorageError::InvalidBtreeKind(other)),
        })
    }
}

// ---------- CRC helpers ----------

/// Compute CRC32C (Castagnoli) over a byte slice. The caller is responsible
/// for zeroing any in-band CRC slot before invoking this. IMPL §1.4.
pub fn block_crc(bytes_with_zeroed_crc_slot: &[u8]) -> u32 {
    crc32c::crc32c(bytes_with_zeroed_crc_slot)
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
        for raw in 0u16..=26 {
            let k = BtreeKind::from_u16(raw).unwrap();
            assert_eq!(k as u16, raw);
        }
        assert!(BtreeKind::from_u16(99).is_err());
    }

    #[test]
    fn crc32c_known_vector() {
        // crc32c of empty input is 0.
        assert_eq!(block_crc(&[]), 0);
        // Known vector: crc32c("123456789") == 0xE3069283.
        assert_eq!(block_crc(b"123456789"), 0xE306_9283);
    }
}
