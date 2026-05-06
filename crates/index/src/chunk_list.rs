//! `ChunkList` per-object recipe structures (IMPL §9.3).
//!
//! Two on-disk wire structs:
//!
//! - [`ChunkParamsRecord`] (16 B) — sits immediately after the
//!   `BtreeNodeHeader` of the chain's head region (flag
//!   `BTREE_NODE_FLAG_HEAD_OF_CHAIN`).
//! - [`ChunkListEntry`] (40 B) — one per FastCDC chunk of the object.
//!
//! Note: the storage crate currently exports a 16-byte
//! `ChunkParamsRecord` *placeholder* that holds opaque bytes; the version in
//! this crate is the spec-shape with named fields. They occupy the same
//! 16-byte footprint, so the engine can `bytemuck::cast` between them when
//! the time comes.

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
};

use crate::error::IndexError;

// ---------- ChunkParamsRecord ----------

/// Size in bytes of [`ChunkParamsRecord`] (16). IMPL §9.3 lines 1974–1981.
pub const CHUNK_PARAMS_RECORD_SIZE: usize = 16;

/// On-disk chunk-params record. IMPL §9.3.
///
/// Layout:
///
/// ```text
/// [0..1]   algo      u8     (ChunkingAlgo discriminant)
/// [1..2]   flags     u8     (reserved)
/// [2..4]   _pad      [u8; 2]
/// [4..8]   min_size  u32    (plaintext bytes; ignored for algo == None)
/// [8..12]  avg_size  u32
/// [12..16] max_size  u32
/// ```
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct ChunkParamsRecord {
    pub algo: u8,       // [0..1]   ChunkingAlgo discriminant
    pub flags: u8,      // [1..2]   reserved
    pub _pad: [u8; 2],  // [2..4]
    pub min_size: u32,  // [4..8]
    pub avg_size: u32,  // [8..12]
    pub max_size: u32,  // [12..16]
}

const_assert_eq!(
    core::mem::size_of::<ChunkParamsRecord>(),
    CHUNK_PARAMS_RECORD_SIZE
);
// computed: 1 (algo) + 1 (flags) + 2 (_pad) + 4 (min_size) + 4 (avg_size)
// + 4 (max_size) = 16

/// `ChunkingAlgo` discriminant — `0 = None`. IMPL §9.3.
pub const CHUNKING_ALGO_NONE: u8 = 0;
/// `ChunkingAlgo` discriminant — `1 = FixedSize`.
pub const CHUNKING_ALGO_FIXED_SIZE: u8 = 1;
/// `ChunkingAlgo` discriminant — `2 = FastCDC`.
pub const CHUNKING_ALGO_FAST_CDC: u8 = 2;

impl ChunkParamsRecord {
    /// Build a `None` record (no chunking).
    pub fn none() -> Self {
        Self {
            algo: CHUNKING_ALGO_NONE,
            flags: 0,
            _pad: [0; 2],
            min_size: 0,
            avg_size: 0,
            max_size: 0,
        }
    }

    /// Build a `FastCDC` record with the given size triple.
    pub fn fast_cdc(min_size: u32, avg_size: u32, max_size: u32) -> Self {
        Self {
            algo: CHUNKING_ALGO_FAST_CDC,
            flags: 0,
            _pad: [0; 2],
            min_size,
            avg_size,
            max_size,
        }
    }

    /// Build a `FixedSize` record.
    pub fn fixed_size(size: u32) -> Self {
        Self {
            algo: CHUNKING_ALGO_FIXED_SIZE,
            flags: 0,
            _pad: [0; 2],
            min_size: size,
            avg_size: size,
            max_size: size,
        }
    }

    /// Borrow as a 16-byte slice.
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }

    /// Parse from a 16-byte slice.
    pub fn parse(bytes: &[u8]) -> Result<Self, IndexError> {
        if bytes.len() < CHUNK_PARAMS_RECORD_SIZE {
            return Err(IndexError::BufferTooSmall {
                need: CHUNK_PARAMS_RECORD_SIZE,
                have: bytes.len(),
            });
        }
        Ok(*bytemuck::from_bytes(&bytes[..CHUNK_PARAMS_RECORD_SIZE]))
    }
}

// ---------- ChunkListEntry ----------

/// Size in bytes of [`ChunkListEntry`] (40). IMPL §9.3 lines 1991–1995.
pub const CHUNK_LIST_ENTRY_SIZE: usize = 40;

/// On-disk per-chunk recipe entry. IMPL §9.3.
///
/// Layout:
///
/// ```text
/// [0..32]  chunk_hash  [u8; 32]   (BLAKE3 of plaintext, indexes ChunkIndex)
/// [32..36] length      u32        (plaintext length of this chunk)
/// [36..40] flags       u32        (reserved — e.g. inline-tiny-chunk hint)
/// ```
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct ChunkListEntry {
    pub chunk_hash: [u8; 32], // [0..32]
    pub length: u32,          // [32..36]
    pub flags: u32,           // [36..40]
}

const_assert_eq!(
    core::mem::size_of::<ChunkListEntry>(),
    CHUNK_LIST_ENTRY_SIZE
);
// computed: 32 (chunk_hash) + 4 (length) + 4 (flags) = 40

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_params_record_size_is_16() {
        // computed: 1 + 1 + 2 + 4 + 4 + 4 = 16
        assert_eq!(core::mem::size_of::<ChunkParamsRecord>(), 16);
    }

    #[test]
    fn chunk_list_entry_size_is_40() {
        // computed: 32 + 4 + 4 = 40
        assert_eq!(core::mem::size_of::<ChunkListEntry>(), 40);
    }

    #[test]
    fn chunking_algo_discriminants_pinned() {
        assert_eq!(CHUNKING_ALGO_NONE, 0);
        assert_eq!(CHUNKING_ALGO_FIXED_SIZE, 1);
        assert_eq!(CHUNKING_ALGO_FAST_CDC, 2);
    }

    #[test]
    fn fast_cdc_record_round_trip() {
        let r = ChunkParamsRecord::fast_cdc(1024, 4096, 16384);
        let bytes = r.as_bytes();
        let parsed = ChunkParamsRecord::parse(bytes).unwrap();
        assert_eq!({ parsed.algo }, CHUNKING_ALGO_FAST_CDC);
        assert_eq!({ parsed.min_size }, 1024);
        assert_eq!({ parsed.avg_size }, 4096);
        assert_eq!({ parsed.max_size }, 16384);
    }
}
