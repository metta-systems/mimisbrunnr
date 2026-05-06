//! `KvDirectory` 4 KiB block — IMPL §9.1.
//!
//! ```text
//! [0..32]    BlockHeader  (kind = KvHashDirectory, magic = "MIMR")
//! [32..33]   global_depth   u8
//! [33..36]   _pad0          [u8; 3]
//! [36..40]   bucket_count   u32   (= 1 << global_depth)
//! [40..4072] entries        [BlockRef; 252]   (4 032 B; local-depth-tagged buckets)
//! [4072..4088] spillover_root BlockRef          (16 B; 0 unless global_depth ≥ 8)
//! [4088..4092] _pad_tail    [u8; 4]
//! [4092..4096] crc          u32     (CRC32C over bytes [0..4092] with crc=0)
//! ```
//!
//! Total = 4 096 B = 4 KiB.
//!
//! The inline `entries` array is sufficient for `global_depth ≤ 7`
//! (i.e. ≤ 128 hash buckets). At `global_depth ≥ 8`, `spillover_root`
//! points at a §1.5 large-node positional region
//! (`BtreeKind::KvDirectory`, level 0) holding the full
//! `2^global_depth`-sized `BlockRef[]` array.
//!
//! ## R1c-A1 scope
//!
//! - **Inline directory only.** `global_depth ≤ 7` enforced by writers;
//!   spillover is unimplemented (returns
//!   [`KvDirectoryError::SpilloverUnsupported`] when needed).
//! - **Cap on bucket count.** With the engine's current 256 KiB
//!   `kv_index_root` region serving as a slot pool for both directory,
//!   buckets, and bitmap pages, the practical `global_depth` cap is
//!   ~4 (16 buckets) until Tier 3 D3 lands sub-bucket allocation and
//!   moves bucket pages into the bucket allocator's pool.

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{
        BLOCK_PREAMBLE_MAGIC_BLOCK, BLOCK_SIZE, BlockDevice, BlockHeader, BlockKind, BlockRef,
        block_crc,
    },
    static_assertions::const_assert_eq,
};

/// 4 KiB.
pub const KV_DIRECTORY_BLOCK_SIZE: usize = BLOCK_SIZE;

/// Number of inline directory entries (252).
pub const KV_DIRECTORY_INLINE_ENTRIES: usize = 252;

/// Maximum `global_depth` representable inline (252 entries → ≤ 128
/// hash buckets, i.e. global_depth ≤ 7).
pub const KV_DIRECTORY_MAX_INLINE_DEPTH: u8 = 7;

/// Format-version slot for [`KvDirectoryBlock::header`].
pub const KV_DIRECTORY_FORMAT_VERSION: u16 = 1;

const ENTRIES_OFFSET: usize = 32 /* BlockHeader */ + 1 + 3 + 4 /* dir header */;
const ENTRIES_END: usize = ENTRIES_OFFSET + KV_DIRECTORY_INLINE_ENTRIES * 16;
const SPILLOVER_OFFSET: usize = ENTRIES_END;
const PAD_TAIL_OFFSET: usize = SPILLOVER_OFFSET + 16;
const CRC_OFFSET: usize = KV_DIRECTORY_BLOCK_SIZE - 4;

const_assert_eq!(ENTRIES_OFFSET, 40);
const_assert_eq!(ENTRIES_END, 4072);
const_assert_eq!(SPILLOVER_OFFSET, 4072);
const_assert_eq!(PAD_TAIL_OFFSET, 4088);
const_assert_eq!(CRC_OFFSET, 4092);

// ---------- KvDirectoryBlock ----------

/// 4 KiB on-disk KV directory block.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct KvDirectoryBlock {
    pub header: BlockHeader,                   // [0..32]
    pub global_depth: u8,                       // [32..33]
    pub _pad0: [u8; 3],                         // [33..36]
    pub bucket_count: u32,                      // [36..40]
    pub entries: [BlockRef; KV_DIRECTORY_INLINE_ENTRIES], // [40..4072]
    pub spillover_root: BlockRef,               // [4072..4088]
    pub _pad_tail: [u8; 4],                     // [4088..4092]
    pub crc: u32,                               // [4092..4096]
}

const_assert_eq!(core::mem::size_of::<KvDirectoryBlock>(), KV_DIRECTORY_BLOCK_SIZE);

#[derive(Debug, thiserror::Error)]
pub enum KvDirectoryError {
    #[error("KvDirectory CRC mismatch: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch { expected: u32, actual: u32 },

    #[error("invalid magic: expected MIMR, got {actual:?}")]
    InvalidMagic { actual: [u8; 4] },

    #[error("invalid block kind for KvDirectory: got {0}")]
    InvalidBlockKind(u16),

    #[error("invalid format version for KvDirectory: got {0}")]
    InvalidFormatVersion(u16),

    #[error(
        "global_depth {0} exceeds inline cap {KV_DIRECTORY_MAX_INLINE_DEPTH}; \
         spillover is not yet implemented (R1c-A1)"
    )]
    SpilloverUnsupported(u8),

    #[error("bucket_count {got} doesn't match 1 << global_depth = {expected}")]
    InconsistentBucketCount { got: u32, expected: u32 },
}

impl KvDirectoryBlock {
    /// Build a fresh empty directory at `global_depth`.
    ///
    /// Returns [`KvDirectoryError::SpilloverUnsupported`] if
    /// `global_depth > KV_DIRECTORY_MAX_INLINE_DEPTH`.
    pub fn new(global_depth: u8) -> Result<Self, KvDirectoryError> {
        if global_depth > KV_DIRECTORY_MAX_INLINE_DEPTH {
            return Err(KvDirectoryError::SpilloverUnsupported(global_depth));
        }
        let bucket_count = 1u32 << global_depth;
        let header = BlockHeader::new(
            BlockKind::KvHashDirectory,
            KV_DIRECTORY_FORMAT_VERSION,
            (KV_DIRECTORY_BLOCK_SIZE - core::mem::size_of::<BlockHeader>() - 4) as u32,
        );
        Ok(Self {
            header,
            global_depth,
            _pad0: [0; 3],
            bucket_count,
            entries: [BlockRef::default(); KV_DIRECTORY_INLINE_ENTRIES],
            spillover_root: BlockRef::default(),
            _pad_tail: [0; 4],
            crc: 0,
        })
    }

    /// Directory entry count (= `1 << global_depth`).
    pub fn count(&self) -> u32 {
        self.bucket_count
    }

    /// Set the directory entry at `idx` to `block_ref`. Caller is
    /// responsible for ensuring `idx < bucket_count`; out-of-bounds is
    /// silently no-op'd (caller bug).
    pub fn set_entry(&mut self, idx: u32, block_ref: BlockRef) {
        let count = { self.bucket_count } as usize;
        if (idx as usize) < count.min(KV_DIRECTORY_INLINE_ENTRIES) {
            self.entries[idx as usize] = block_ref;
        }
    }

    /// Read the directory entry at `idx`. Returns
    /// `BlockRef::default()` for out-of-bounds.
    pub fn get_entry(&self, idx: u32) -> BlockRef {
        let count = { self.bucket_count } as usize;
        if (idx as usize) < count.min(KV_DIRECTORY_INLINE_ENTRIES) {
            self.entries[idx as usize]
        } else {
            BlockRef::default()
        }
    }

    /// Recompute and store the trailing CRC32C.
    pub fn recompute_crc(&mut self) {
        self.crc = 0;
        let crc = block_crc(&bytemuck::bytes_of(self)[..CRC_OFFSET]);
        self.crc = crc;
    }

    /// Validate the trailing CRC.
    pub fn verify_crc(&self) -> Result<(), KvDirectoryError> {
        let expected = { self.crc };
        let mut copy = *self;
        copy.crc = 0;
        let actual = block_crc(&bytemuck::bytes_of(&copy)[..CRC_OFFSET]);
        if expected == actual {
            Ok(())
        } else {
            Err(KvDirectoryError::CrcMismatch { expected, actual })
        }
    }

    fn validate(&self) -> Result<(), KvDirectoryError> {
        let magic = { self.header.pre.magic };
        if magic != BLOCK_PREAMBLE_MAGIC_BLOCK {
            return Err(KvDirectoryError::InvalidMagic { actual: magic });
        }
        let kind_raw = { self.header.pre.kind };
        if kind_raw != BlockKind::KvHashDirectory as u16 {
            return Err(KvDirectoryError::InvalidBlockKind(kind_raw));
        }
        let version = { self.header.pre.format_version };
        if version != KV_DIRECTORY_FORMAT_VERSION {
            return Err(KvDirectoryError::InvalidFormatVersion(version));
        }
        let depth = self.global_depth;
        if depth > KV_DIRECTORY_MAX_INLINE_DEPTH {
            return Err(KvDirectoryError::SpilloverUnsupported(depth));
        }
        let count = { self.bucket_count };
        let expected_count = 1u32 << depth;
        if count != expected_count {
            return Err(KvDirectoryError::InconsistentBucketCount {
                got: count,
                expected: expected_count,
            });
        }
        Ok(())
    }

    /// Persist this block to `device` at byte `offset`. Recomputes the
    /// CRC.
    pub fn write<D: BlockDevice>(
        &mut self,
        device: &D,
        offset: u64,
    ) -> Result<(), KvDirectoryError> {
        self.recompute_crc();
        device
            .write_at(offset, bytemuck::bytes_of(self))
            .map_err(|_| KvDirectoryError::CrcMismatch {
                expected: 0,
                actual: 0,
            })?;
        Ok(())
    }

    /// Read a `KvDirectoryBlock` from `device` at byte `offset`.
    /// Validates magic + kind + version + CRC.
    pub fn read<D: BlockDevice>(device: &D, offset: u64) -> Result<Self, KvDirectoryError> {
        let mut buf = vec![0u8; KV_DIRECTORY_BLOCK_SIZE];
        device
            .read_at(offset, &mut buf)
            .map_err(|_| KvDirectoryError::CrcMismatch {
                expected: 0,
                actual: 0,
            })?;
        let block: &Self = bytemuck::from_bytes(&buf);
        let block = *block;
        block.validate()?;
        block.verify_crc()?;
        Ok(block)
    }
}

#[cfg(test)]
mod tests {
    use {super::*, mimisbrunnr_storage::FileBlockDevice, tempfile::TempDir};

    #[test]
    fn block_size_is_4096() {
        assert_eq!(core::mem::size_of::<KvDirectoryBlock>(), 4096);
    }

    #[test]
    fn new_at_depth_zero_has_one_bucket() {
        let dir = KvDirectoryBlock::new(0).unwrap();
        assert_eq!(dir.count(), 1);
        assert_eq!({ dir.global_depth }, 0);
    }

    #[test]
    fn new_at_depth_seven_has_128_buckets() {
        let dir = KvDirectoryBlock::new(7).unwrap();
        assert_eq!(dir.count(), 128);
    }

    #[test]
    fn depth_above_seven_rejected_under_r1c() {
        let err = KvDirectoryBlock::new(8).unwrap_err();
        assert!(matches!(err, KvDirectoryError::SpilloverUnsupported(8)));
    }

    #[test]
    fn round_trip_via_block_device() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("kv_dir.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();

        let mut block = KvDirectoryBlock::new(3).unwrap();
        let r = BlockRef {
            disk_id: 0,
            _pad: 0,
            block_no: 42,
            generation: 7,
        };
        block.set_entry(0, r);
        block.set_entry(7, r);
        block.write(&dev, 0).unwrap();

        let back = KvDirectoryBlock::read(&dev, 0).unwrap();
        assert_eq!(back.count(), 8);
        let e0 = back.get_entry(0);
        assert_eq!({ e0.block_no }, 42);
        assert_eq!({ e0.generation }, 7);
        let e7 = back.get_entry(7);
        assert_eq!({ e7.block_no }, 42);
        // Other slots are zero.
        let e1 = back.get_entry(1);
        assert_eq!({ e1.block_no }, 0);
    }

    #[test]
    fn corrupted_crc_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("kv_dir.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        let mut block = KvDirectoryBlock::new(2).unwrap();
        block.write(&dev, 0).unwrap();

        // Flip a payload byte without touching CRC.
        let mut buf = vec![0u8; KV_DIRECTORY_BLOCK_SIZE];
        dev.read_at(0, &mut buf).unwrap();
        buf[40] ^= 0xFF;
        dev.write_at(0, &buf).unwrap();

        let err = KvDirectoryBlock::read(&dev, 0).unwrap_err();
        assert!(matches!(err, KvDirectoryError::CrcMismatch { .. }));
    }

    #[test]
    fn wrong_block_kind_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("kv_dir.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        let mut block = KvDirectoryBlock::new(0).unwrap();
        block.write(&dev, 0).unwrap();

        let mut buf = vec![0u8; KV_DIRECTORY_BLOCK_SIZE];
        dev.read_at(0, &mut buf).unwrap();
        // BlockPreamble.kind sits at offset 4 (low byte) / 5 (high byte).
        buf[4] = BlockKind::KvHashBucket as u8;
        buf[5] = 0;
        // Recompute CRC over modified bytes.
        let mut zeroed = buf.clone();
        for b in &mut zeroed[CRC_OFFSET..CRC_OFFSET + 4] {
            *b = 0;
        }
        let new_crc = block_crc(&zeroed[..CRC_OFFSET]);
        buf[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&new_crc.to_le_bytes());
        dev.write_at(0, &buf).unwrap();

        let err = KvDirectoryBlock::read(&dev, 0).unwrap_err();
        assert!(matches!(err, KvDirectoryError::InvalidBlockKind(_)));
    }
}
