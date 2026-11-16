//! Bucket-relative block addressing. ENGINE-DESIGN§1.3 and ALLOCATION-DESIGN§1.3.
//!
//! Within a device a 4 KiB sector is addressed as `(bucket_no, sector)`;
//! across the pool `(disk_id, bucket_no, sector)` identifies it. A byte
//! offset is `(bucket_no << bucket_size_log2) + sector × 4096`.

use crate::{
    block::{BLOCK_SIZE, BLOCK_SIZE_LOG2},
    error::StorageError,
    root_pointer::{BlobRef, BlockRef},
};

/// Largest bucket a disk may use: 256 MiB (ALLOCATION-DESIGN§1.1), the largest
/// whose sectors all fit the `u16` `sector` field.
pub const MAX_BUCKET_SIZE_LOG2: u8 = 28;

/// What turning a byte offset on one disk into a `BlockRef` / `BlobRef`, and
/// back, needs: the disk's id and its `Superblock.bucket_size_log2`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct DiskAddressing {
    pub disk_id: u16,
    pub bucket_size_log2: u8,
}

impl DiskAddressing {
    /// Addressing for disk `disk_id` with `2^bucket_size_log2`-byte buckets.
    pub fn new(disk_id: u16, bucket_size_log2: u8) -> Self {
        Self {
            disk_id,
            bucket_size_log2,
        }
    }

    /// The `BlockRef` to the 4 KiB-aligned byte `offset` on this disk.
    pub fn block_ref(&self, offset: u64, generation: u32) -> Result<BlockRef, StorageError> {
        BlockRef::from_byte_offset(self.disk_id, offset, self.bucket_size_log2, generation)
    }

    /// The `BlobRef` to `length` bytes at the 4 KiB-aligned byte `offset`
    /// on this disk.
    pub fn blob_ref(&self, offset: u64, length: u64) -> Result<BlobRef, StorageError> {
        BlobRef::from_byte_offset(self.disk_id, offset, self.bucket_size_log2, length)
    }

    /// Byte offset of `block_ref` on this disk; a reference to another disk
    /// fails with [`StorageError::CrossDiskBlockRef`].
    pub fn block_offset(&self, block_ref: &BlockRef) -> Result<u64, StorageError> {
        self.check_disk(block_ref.disk_id)?;
        Ok(block_ref.byte_offset(self.bucket_size_log2))
    }

    /// Byte offset of `blob_ref`'s start on this disk; a reference to
    /// another disk fails with [`StorageError::CrossDiskBlockRef`].
    pub fn blob_offset(&self, blob_ref: &BlobRef) -> Result<u64, StorageError> {
        self.check_disk(blob_ref.disk_id)?;
        Ok(blob_ref.byte_offset(self.bucket_size_log2))
    }

    fn check_disk(&self, disk_id: u16) -> Result<(), StorageError> {
        if disk_id == self.disk_id {
            Ok(())
        } else {
            Err(StorageError::CrossDiskBlockRef {
                expected_disk: self.disk_id,
                got_disk: disk_id,
            })
        }
    }
}

/// Byte offset of `sector` within bucket `bucket_no` on a disk with
/// `2^bucket_size_log2`-byte buckets. `bucket_size_log2` is a validated
/// superblock value (at most [`MAX_BUCKET_SIZE_LOG2`]), so the `u64` result
/// cannot overflow: at most `(2³² − 1) × 2²⁸ + 65 535 × 4096`.
pub fn bucket_relative_byte_offset(bucket_no: u32, sector: u16, bucket_size_log2: u8) -> u64 {
    (u64::from(bucket_no) << bucket_size_log2) + u64::from(sector) * BLOCK_SIZE as u64
}

/// Split a 4 KiB-aligned byte `offset` into `(bucket_no, sector)` for a disk
/// with `2^bucket_size_log2`-byte buckets.
///
/// Fails with [`StorageError::UnalignedBlockOffset`] when `offset` is not a
/// multiple of 4 KiB, and with [`StorageError::BlockOffsetUnaddressable`] when
/// the bucket size is outside `4 KiB..=256 MiB` or the bucket number does not
/// fit `u32`.
pub fn split_byte_offset(offset: u64, bucket_size_log2: u8) -> Result<(u32, u16), StorageError> {
    if !offset.is_multiple_of(BLOCK_SIZE as u64) {
        return Err(StorageError::UnalignedBlockOffset { offset });
    }
    let unaddressable = StorageError::BlockOffsetUnaddressable {
        offset,
        bucket_size_log2,
    };
    if !(BLOCK_SIZE_LOG2..=MAX_BUCKET_SIZE_LOG2).contains(&bucket_size_log2) {
        return Err(unaddressable);
    }
    let Ok(bucket_no) = u32::try_from(offset >> bucket_size_log2) else {
        return Err(unaddressable);
    };
    let offset_in_bucket = offset & ((1u64 << bucket_size_log2) - 1);
    let Ok(sector) = u16::try_from(offset_in_bucket >> BLOCK_SIZE_LOG2) else {
        return Err(unaddressable);
    };
    Ok((bucket_no, sector))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_mib_buckets_split_into_bucket_and_sector() {
        // 1 MiB bucket: 256 sectors. Offset of bucket 0x12, sector 0x34.
        let offset = (0x12u64 << 20) + 0x34 * 4096;
        assert_eq!(split_byte_offset(offset, 20).unwrap(), (0x12, 0x34));
        assert_eq!(bucket_relative_byte_offset(0x12, 0x34, 20), offset);
    }

    #[test]
    fn offset_zero_is_bucket_zero_sector_zero() {
        assert_eq!(split_byte_offset(0, 20).unwrap(), (0, 0));
    }

    #[test]
    fn unaligned_offset_is_rejected() {
        assert!(matches!(
            split_byte_offset(4097, 20),
            Err(StorageError::UnalignedBlockOffset { offset: 4097 })
        ));
    }

    #[test]
    fn bucket_size_outside_range_is_rejected() {
        for bucket_size_log2 in [0, 11, 29, 63, 64, 255] {
            assert!(matches!(
                split_byte_offset(0, bucket_size_log2),
                Err(StorageError::BlockOffsetUnaddressable { .. })
            ));
        }
    }

    #[test]
    fn bucket_number_beyond_u32_is_rejected() {
        let offset = 1u64 << (32 + 20);
        assert!(matches!(
            split_byte_offset(offset, 20),
            Err(StorageError::BlockOffsetUnaddressable { offset: rejected, bucket_size_log2: 20 }) if rejected == offset
        ));
    }

    #[test]
    fn disk_addressing_round_trips_and_rejects_other_disks() {
        let addressing = DiskAddressing::new(3, 20);
        let offset = (5u64 << 20) + 7 * 4096;
        let block_ref = addressing.block_ref(offset, 9).unwrap();
        assert_eq!(block_ref, BlockRef::new(3, 5, 7, 9));
        assert_eq!(addressing.block_offset(&block_ref).unwrap(), offset);
        let blob_ref = addressing.blob_ref(offset, 100).unwrap();
        assert_eq!(addressing.blob_offset(&blob_ref).unwrap(), offset);
        assert!(matches!(
            DiskAddressing::new(4, 20).block_offset(&block_ref),
            Err(StorageError::CrossDiskBlockRef { expected_disk: 4, got_disk: 3 })
        ));
    }

    #[test]
    fn largest_bucket_keeps_every_sector_in_u16() {
        let last_sector_offset = (1u64 << MAX_BUCKET_SIZE_LOG2) - 4096;
        assert_eq!(split_byte_offset(last_sector_offset, MAX_BUCKET_SIZE_LOG2).unwrap(), (0, u16::MAX));
    }
}
