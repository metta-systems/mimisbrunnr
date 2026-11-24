//! `RootPointer`, `BlockRef`, `BlobRef`. ENGINE-DESIGN§1.2.

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
};

use crate::{
    addressing::{bucket_relative_byte_offset, split_byte_offset},
    error::StorageError,
};

// ---------- BlockRef ----------

/// 16-byte bucket-relative, generation-checked reference to a 4 KiB sector:
/// a tree node or region. ENGINE-DESIGN§1.2, ALLOCATION-DESIGN§1.3.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Eq, PartialEq, Hash, Default)]
pub struct BlockRef {
    pub disk_id: u16,    // [0..2]
    pub sector: u16,     // [2..4]   4 KiB sector within the bucket
    pub bucket_no: u32,  // [4..8]
    pub generation: u32, // [8..12]  bucket generation at write time
    pub _reserved: u32,  // [12..16] zero
}

const_assert_eq!(core::mem::size_of::<BlockRef>(), 16);
const_assert_eq!(core::mem::align_of::<BlockRef>(), 4);
const_assert_eq!(core::mem::offset_of!(BlockRef, disk_id), 0);
const_assert_eq!(core::mem::offset_of!(BlockRef, sector), 2);
const_assert_eq!(core::mem::offset_of!(BlockRef, bucket_no), 4);
const_assert_eq!(core::mem::offset_of!(BlockRef, generation), 8);
const_assert_eq!(core::mem::offset_of!(BlockRef, _reserved), 12);

// ---------- BlobRef ----------

/// 16-byte bucket-relative reference to a payload extent: a length in place
/// of a generation. ENGINE-DESIGN§1.2.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Eq, PartialEq, Hash, Default)]
pub struct BlobRef {
    pub disk_id: u16,   // [0..2]
    pub sector: u16,    // [2..4]
    pub bucket_no: u32, // [4..8]
    pub length: u64,    // [8..16]  bytes
}

const_assert_eq!(core::mem::size_of::<BlobRef>(), 16);
const_assert_eq!(core::mem::align_of::<BlobRef>(), 8);
const_assert_eq!(core::mem::offset_of!(BlobRef, disk_id), 0);
const_assert_eq!(core::mem::offset_of!(BlobRef, sector), 2);
const_assert_eq!(core::mem::offset_of!(BlobRef, bucket_no), 4);
const_assert_eq!(core::mem::offset_of!(BlobRef, length), 8);

// ---------- RootPointer ----------

/// 408-byte root pointer. ENGINE-DESIGN§1.2.
///
/// Holds two scalars (`seq`, `lsn`), 24 `BlockRef` slots, a `flags` word and
/// a trailing checksum (low 32 bits of `checksum64`) over bytes `[0..404]`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, PartialEq, Eq)]
pub struct RootPointer {
    pub seq: u64, // [0..8]   monotonic; larger seq wins
    pub lsn: u64, // [8..16]  WAL LSN this root corresponds to

    // 24 logical/data btree roots (16 B each = 384 B). Offsets [16..400].
    pub object_table_root: BlockRef,            // [16..32]
    pub object_history_root: BlockRef,          // [32..48]
    pub location_table_root: BlockRef,          // [48..64]
    pub location_history_root: BlockRef,        // [64..80]
    pub forward_index_root: BlockRef,           // [80..96]
    pub tag_index_root: BlockRef,               // [96..112]
    pub kv_index_root: BlockRef,                // [112..128]
    pub range_index_root: BlockRef,             // [128..144]
    pub chunk_index_root: BlockRef,             // [144..160]
    pub value_spill_root: BlockRef,             // [160..176]
    pub backpointer_root: BlockRef,             // [176..192]
    pub ontology_root: BlockRef,                // [192..208]
    pub subscriptions_root: BlockRef,           // [208..224]
    pub pool_state_root: BlockRef,              // [224..240]
    pub snapshot_chain_root: BlockRef,          // [240..256]
    pub reconcile_work_root: BlockRef,          // [256..272]
    pub reconcile_high_prio_root: BlockRef,     // [272..288]
    pub reconcile_work_phys_root: BlockRef,     // [288..304]
    pub reconcile_high_prio_phys_root: BlockRef,// [304..320]
    pub reconcile_pending_root: BlockRef,       // [320..336]
    pub reconcile_scan_root: BlockRef,          // [336..352]
    pub disks_overflow_root: BlockRef,          // [352..368]
    pub placement_rules_root: BlockRef,         // [368..384]
    pub cluster_peers_root: BlockRef,           // [384..400]

    pub flags: u32, // [400..404]
    pub crc: u32,   // [404..408]  low 32 bits of checksum64 of bytes [0..404]
}

const_assert_eq!(core::mem::size_of::<RootPointer>(), 408);

impl RootPointer {
    /// CRC slot offset, used by [`Self::recompute_crc`] and verification.
    pub const CRC_OFFSET: usize = 404;

    /// Recompute and store the CRC over bytes `[0..404]`.
    pub fn recompute_crc(&mut self) {
        self.crc = 0;
        let crc = crate::checksum::checksum64(&bytemuck::bytes_of(self)[..Self::CRC_OFFSET]) as u32;
        self.crc = crc;
    }

    /// Validate the stored CRC. Returns the captured `(expected, actual)` on
    /// mismatch.
    pub fn verify_crc(&self) -> Result<(), StorageError> {
        let expected = self.crc;
        let mut copy = *self;
        copy.crc = 0;
        let actual = crate::checksum::checksum64(&bytemuck::bytes_of(&copy)[..Self::CRC_OFFSET]) as u32;
        if expected == actual {
            Ok(())
        } else {
            Err(StorageError::CrcMismatch { expected, actual })
        }
    }
}

impl BlockRef {
    /// Construct a new `BlockRef` to `sector` of bucket `bucket_no`.
    pub fn new(disk_id: u16, bucket_no: u32, sector: u16, generation: u32) -> Self {
        Self {
            disk_id,
            sector,
            bucket_no,
            generation,
            _reserved: 0,
        }
    }

    /// The reference to the 4 KiB-aligned byte `offset` of disk `disk_id`,
    /// whose buckets are `2^bucket_size_log2` bytes. Fails with
    /// [`StorageError::UnalignedBlockOffset`] or
    /// [`StorageError::BlockOffsetUnaddressable`].
    pub fn from_byte_offset(
        disk_id: u16,
        offset: u64,
        bucket_size_log2: u8,
        generation: u32,
    ) -> Result<Self, StorageError> {
        let (bucket_no, sector) = split_byte_offset(offset, bucket_size_log2)?;
        Ok(Self::new(disk_id, bucket_no, sector, generation))
    }

    /// Byte offset on its disk: `(bucket_no << bucket_size_log2) + sector × 4096`.
    pub fn byte_offset(&self, bucket_size_log2: u8) -> u64 {
        bucket_relative_byte_offset(self.bucket_no, self.sector, bucket_size_log2)
    }

    /// Whether this is the all-zero reference, used as "no block".
    pub fn is_null(self) -> bool {
        self == Self::zeroed()
    }

    /// Generation check: the pointer is fresh iff its `generation` matches
    /// the bucket's current generation. Returns `true` when fresh.
    /// ALLOCATION-DESIGN§1.3.
    pub fn dereference_check(&self, alloc: &crate::alloc::BucketAllocEntry) -> bool {
        alloc.generation == self.generation
    }
}

impl BlobRef {
    /// Construct a new `BlobRef` of `length` bytes from `sector` of bucket
    /// `bucket_no`.
    pub fn new(disk_id: u16, bucket_no: u32, sector: u16, length: u64) -> Self {
        Self {
            disk_id,
            sector,
            bucket_no,
            length,
        }
    }

    /// The extent of `length` bytes at the 4 KiB-aligned byte `offset` of
    /// disk `disk_id`. Fails as [`BlockRef::from_byte_offset`] does.
    pub fn from_byte_offset(
        disk_id: u16,
        offset: u64,
        bucket_size_log2: u8,
        length: u64,
    ) -> Result<Self, StorageError> {
        let (bucket_no, sector) = split_byte_offset(offset, bucket_size_log2)?;
        Ok(Self::new(disk_id, bucket_no, sector, length))
    }

    /// Byte offset of the extent's start on its disk.
    pub fn byte_offset(&self, bucket_size_log2: u8) -> u64 {
        bucket_relative_byte_offset(self.bucket_no, self.sector, bucket_size_log2)
    }

    /// Whether this is the all-zero reference, used as "no extent".
    pub fn is_null(self) -> bool {
        self == Self::zeroed()
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn block_ref_is_16_bytes() {
        assert_eq!(core::mem::size_of::<BlockRef>(), 16);
    }

    #[test]
    fn blob_ref_is_16_bytes() {
        assert_eq!(core::mem::size_of::<BlobRef>(), 16);
    }

    #[test]
    fn root_pointer_is_408_bytes() {
        assert_eq!(core::mem::size_of::<RootPointer>(), 408);
    }

    #[test]
    fn root_pointer_crc_round_trip() {
        let mut rp = RootPointer::default();
        rp.seq = 3;
        rp.lsn = 7;
        rp.recompute_crc();
        rp.verify_crc().unwrap();
    }

    #[test]
    fn generation_check_matches_and_mismatches() {
        use crate::alloc::{BucketAllocEntry, BucketKind};
        let alloc = BucketAllocEntry::new(42, BucketKind::Node, 0, 0);
        let fresh = BlockRef::new(0, 4, 0, 42);
        assert!(fresh.dereference_check(&alloc));
        let stale = BlockRef::new(0, 4, 0, 41);
        assert!(!stale.dereference_check(&alloc));
    }

    /// 1 MiB buckets.
    const ONE_MIB_BUCKET_LOG2: u8 = 20;

    #[test]
    fn block_ref_into_last_bucket_of_one_mib_disk_round_trips() {
        let last_bucket = u32::MAX;
        for sector in [0u16, 1, 255] {
            let block_ref = BlockRef::new(7, last_bucket, sector, 0xdead_beef);
            let offset = block_ref.byte_offset(ONE_MIB_BUCKET_LOG2);
            assert_eq!(offset, (u64::from(last_bucket) << 20) + u64::from(sector) * 4096);
            let back = BlockRef::from_byte_offset(7, offset, ONE_MIB_BUCKET_LOG2, 0xdead_beef).unwrap();
            assert_eq!(back, block_ref);
        }
    }

    #[test]
    fn blob_ref_into_last_bucket_of_one_mib_disk_round_trips() {
        let blob_ref = BlobRef::new(2, u32::MAX, 255, 12_345);
        let offset = blob_ref.byte_offset(ONE_MIB_BUCKET_LOG2);
        assert_eq!(BlobRef::from_byte_offset(2, offset, ONE_MIB_BUCKET_LOG2, 12_345).unwrap(), blob_ref);
    }

    #[test]
    fn from_byte_offset_rejects_unaligned_and_unaddressable_offsets() {
        assert!(matches!(
            BlockRef::from_byte_offset(0, 4095, ONE_MIB_BUCKET_LOG2, 1),
            Err(StorageError::UnalignedBlockOffset { offset: 4095 })
        ));
        assert!(matches!(
            BlockRef::from_byte_offset(0, 1u64 << 52, ONE_MIB_BUCKET_LOG2, 1),
            Err(StorageError::BlockOffsetUnaddressable { .. })
        ));
        // A 512 MiB bucket has sectors beyond u16.
        assert!(matches!(
            BlobRef::from_byte_offset(0, 0, 29, 1),
            Err(StorageError::BlockOffsetUnaddressable { bucket_size_log2: 29, .. })
        ));
    }

    #[test]
    fn block_ref_encodes_to_fixed_bytes() {
        let block_ref = BlockRef::new(0x0102, 0x0708_090a, 0x0304, 0x1112_1314);
        assert_eq!(
            bytemuck::bytes_of(&block_ref),
            &[
                0x02, 0x01, // disk_id
                0x04, 0x03, // sector
                0x0a, 0x09, 0x08, 0x07, // bucket_no
                0x14, 0x13, 0x12, 0x11, // generation
                0x00, 0x00, 0x00, 0x00, // _reserved
            ]
        );
    }

    #[test]
    fn blob_ref_encodes_to_fixed_bytes() {
        let blob_ref = BlobRef::new(0x0102, 0x0708_090a, 0x0304, 0x1112_1314_1516_1718);
        assert_eq!(
            bytemuck::bytes_of(&blob_ref),
            &[
                0x02, 0x01, // disk_id
                0x04, 0x03, // sector
                0x0a, 0x09, 0x08, 0x07, // bucket_no
                0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, // length
            ]
        );
    }

    #[test]
    fn root_pointer_crc_mismatch() {
        let mut rp = RootPointer::default();
        rp.seq = 3;
        rp.recompute_crc();
        rp.lsn = 99; // mutate after CRC
        let err = rp.verify_crc().unwrap_err();
        assert!(matches!(err, StorageError::CrcMismatch { .. }));
    }
}
