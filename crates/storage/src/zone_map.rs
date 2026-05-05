//! Zone-extent descriptors and the optional `ZoneMap` continuation block.
//! IMPL §2.1.

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
};

use crate::block::{BLOCK_SIZE, BlockHeader, BlockKind};

/// 24-byte extent descriptor: `(offset, length, flags, _pad)`.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, Eq, PartialEq)]
pub struct ZoneExtent {
    pub offset: u64, // [0..8]   byte offset on the device
    pub length: u64, // [8..16]  length in bytes
    pub flags: u32,  // [16..20] reserved
    pub _pad: u32,   // [20..24]
}

const_assert_eq!(core::mem::size_of::<ZoneExtent>(), 24);

/// A single entry in the [`ZoneMap`] continuation block.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, Eq, PartialEq)]
pub struct ZoneMapEntry {
    pub zone_kind: u8,        // [0..1]   0 = index, 1 = metadata, 2 = blob
    pub _pad: [u8; 7],        // [1..8]
    pub extent: ZoneExtent,   // [8..32]
}

const_assert_eq!(core::mem::size_of::<ZoneMapEntry>(), 32);

/// Continuation block listing additional (non-contiguous) zone extents.
/// Written when [`super::Superblock::zone_map_offset`] is non-zero.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct ZoneMap {
    pub header: BlockHeader,                // [0..32]   kind = ZoneMap
    pub extent_count: u16,                  // [32..34]
    pub _pad: [u8; 6],                      // [34..40]
    pub extents: [ZoneMapEntry; 126],       // [40..4072]   126 × 32 B
    pub _pad_tail: [u8; 20],                // [4072..4092]
    pub crc: u32,                           // [4092..4096] trailing CRC32C
}

const_assert_eq!(core::mem::size_of::<ZoneMap>(), 4096);

impl ZoneMap {
    /// Build a zero-initialised `ZoneMap` block with header populated.
    pub fn new(format_version: u16) -> Self {
        // Safety: all fields are POD; zero-init is valid (`Zeroable`).
        let mut zm: Self = bytemuck::Zeroable::zeroed();
        zm.header = BlockHeader::new(BlockKind::ZoneMap, format_version, (BLOCK_SIZE - 32 - 4) as u32);
        zm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_spec() {
        assert_eq!(core::mem::size_of::<ZoneExtent>(), 24);
        assert_eq!(core::mem::size_of::<ZoneMapEntry>(), 32);
        assert_eq!(core::mem::size_of::<ZoneMap>(), 4096);
    }
}
