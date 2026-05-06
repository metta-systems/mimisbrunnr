//! WAL header (4 KiB block stored at `Superblock.wal_offset` in two adjacent
//! A/B copies). IMPL §3.1.

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{BLOCK_SIZE, BlockHeader, BlockKind, block_crc},
    static_assertions::const_assert_eq,
};

use crate::error::WalError;

/// `WalHeader.format_version`.
pub const WAL_HEADER_FORMAT_VERSION: u16 = 1;

/// Reserved-region length so the header totals exactly 4096 B and the trailing
/// 4-byte CRC slot fits inside `BlockHeader`'s frame.
const WAL_HEADER_RESERVED_LEN: usize = 3996;

/// 4 KiB on-disk WAL header. IMPL §3.1.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct WalHeader {
    pub header: BlockHeader,                       // [0..32]
    pub next_lsn: u64,                             // [32..40]
    pub write_cursor: u64,                         // [40..48]   byte offset within ring
    pub read_cursor: u64,                          // [48..56]   oldest entry not yet checkpointed
    pub used_bytes: u64,                           // [56..64]
    pub last_checkpoint_lsn: u64,                  // [64..72]
    pub segment_size: u32,                         // [72..76]   typically 1 MiB
    pub _pad: u32,                                 // [76..80]
    pub encryption_keyid: [u8; 16],                // [80..96]
    pub _reserved: [u8; WAL_HEADER_RESERVED_LEN],  // [96..4092]
    pub crc: u32,                                  // [4092..4096]   CRC32C of [0..4092]
}

const_assert_eq!(core::mem::size_of::<WalHeader>(), BLOCK_SIZE);

impl Default for WalHeader {
    fn default() -> Self {
        Self {
            header: BlockHeader::new(BlockKind::WalSegment, WAL_HEADER_FORMAT_VERSION, 0),
            next_lsn: 1,
            write_cursor: 0,
            read_cursor: 0,
            used_bytes: 0,
            last_checkpoint_lsn: 0,
            segment_size: 1024 * 1024,
            _pad: 0,
            encryption_keyid: [0; 16],
            _reserved: [0; WAL_HEADER_RESERVED_LEN],
            crc: 0,
        }
    }
}

impl WalHeader {
    /// CRC slot lives at the very tail of the 4 KiB block.
    pub const CRC_OFFSET: usize = BLOCK_SIZE - 4;

    /// Recompute and store the CRC over bytes `[0..4092]`.
    pub fn recompute_crc(&mut self) {
        self.crc = 0;
        let bytes = bytemuck::bytes_of(self);
        let crc = block_crc(&bytes[..Self::CRC_OFFSET]);
        self.crc = crc;
    }

    /// Validate the stored CRC. Returns `Err(WalError::CrcMismatch)` on
    /// mismatch.
    pub fn verify_crc(&self) -> Result<(), WalError> {
        let expected = { self.crc };
        let mut copy = *self;
        copy.crc = 0;
        let bytes = bytemuck::bytes_of(&copy);
        let actual = block_crc(&bytes[..Self::CRC_OFFSET]);
        if expected == actual {
            Ok(())
        } else {
            Err(WalError::CrcMismatch { expected, actual })
        }
    }

    /// Verify that the inner [`BlockHeader`] declares `BlockKind::WalSegment`
    /// and a supported format version.
    pub fn verify_kind(&self) -> Result<(), WalError> {
        let kind_raw = { self.header.pre.kind };
        let kind = mimisbrunnr_storage::BlockKind::from_u16(kind_raw)?;
        if kind != BlockKind::WalSegment {
            return Err(WalError::InvalidMagic {
                expected: *b"WALR",
                actual: [0; 4],
            });
        }
        let version = { self.header.pre.format_version };
        if version != WAL_HEADER_FORMAT_VERSION {
            return Err(WalError::UnsupportedFormatVersion(version));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_size_is_4096() {
        assert_eq!(core::mem::size_of::<WalHeader>(), 4096);
    }

    #[test]
    fn crc_round_trip() {
        let mut h = WalHeader {
            next_lsn: 5,
            ..Default::default()
        };
        h.recompute_crc();
        h.verify_crc().unwrap();
    }

    #[test]
    fn crc_mismatch_after_mutation() {
        let mut h = WalHeader::default();
        h.recompute_crc();
        h.next_lsn = 99;
        let err = h.verify_crc().unwrap_err();
        assert!(matches!(err, WalError::CrcMismatch { .. }));
    }
}
