//! The legacy 64-byte header of the forward-overflow chain's regions.
//!
//! TODO(INDEX14): goes when that chain is a chain of positional page regions
//! ([`crate::page_region`], BTREE-DESIGN§1.7). Keyed trees use
//! [`crate::region`].

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
};

use crate::{
    block::{BLOCK_PREAMBLE_MAGIC_BTREE, BlockPreamble, BtreeKind},
    error::StorageError,
};

/// Flag: head of a chain.
pub const BTREE_NODE_FLAG_HEAD_OF_CHAIN: u8 = 1 << 1;

/// 64-byte header at offset 0 of a legacy positional region.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct BtreeNodeHeader {
    pub pre: BlockPreamble,             // [0..8]   magic = "MIMB", kind ∈ BtreeKind
    pub seq: u64,                       // [8..16]  monotonic per-region
    pub last_persisted_lsn: u64,        // [16..24] BlockHeader.lsn analogue
    pub region_size_log2: u8,           // [24..25] 18 = 256 KiB
    pub level: u8,                      // [25..26] 0 = leaf, ≥1 = inner
    pub sorted_run_count: u8,           // [26..27] unused by positional regions
    pub flags: u8,                      // [27..28] BTREE_NODE_FLAG_*
    pub payload_used: u32,              // [28..32]
    pub min_key: [u8; 16],              // [32..48]
    pub max_key: [u8; 16],              // [48..64]
}

const_assert_eq!(core::mem::size_of::<BtreeNodeHeader>(), 64);

impl BtreeNodeHeader {
    /// Build a fresh header for a region of the given `kind` and `level`.
    pub fn new(kind: BtreeKind, format_version: u16, level: u8, region_size_log2: u8) -> Self {
        Self {
            pre: BlockPreamble {
                magic: BLOCK_PREAMBLE_MAGIC_BTREE,
                kind: kind as u16,
                format_version,
            },
            seq: 0,
            last_persisted_lsn: 0,
            region_size_log2,
            level,
            sorted_run_count: 0,
            flags: 0,
            payload_used: 0,
            min_key: [0u8; 16],
            max_key: [0u8; 16],
        }
    }

    /// Parse a `BtreeNodeHeader` from a byte slice. Validates the magic.
    pub fn parse(bytes: &[u8]) -> Result<Self, StorageError> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return Err(StorageError::BufferTooSmall {
                need: core::mem::size_of::<Self>(),
                have: bytes.len(),
            });
        }
        let header_bytes = &bytes[..core::mem::size_of::<Self>()];
        let header: Self = *bytemuck::from_bytes(header_bytes);
        let magic = { header.pre.magic };
        if magic != BLOCK_PREAMBLE_MAGIC_BTREE {
            return Err(StorageError::InvalidMagic {
                expected: BLOCK_PREAMBLE_MAGIC_BTREE,
                actual: magic,
            });
        }
        Ok(header)
    }

    /// Serialise as bytes (zero-copy view).
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn btree_node_header_is_64_bytes() {
        assert_eq!(core::mem::size_of::<BtreeNodeHeader>(), 64);
    }

    #[test]
    fn parse_btree_node_header_validates_magic() {
        let h = BtreeNodeHeader::new(BtreeKind::Forward, 1, 0, 18);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(h.as_bytes());
        let parsed = BtreeNodeHeader::parse(&bytes).unwrap();
        assert_eq!({ parsed.region_size_log2 }, 18);

        // Corrupt the magic.
        bytes[0] = b'X';
        let err = BtreeNodeHeader::parse(&bytes).unwrap_err();
        assert!(matches!(err, StorageError::InvalidMagic { .. }));
    }
}
