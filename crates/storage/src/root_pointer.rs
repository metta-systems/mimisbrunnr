//! `RootPointer`, `BlockRef`, `BlobRef`. IMPL §2.2.

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
};

use crate::error::StorageError;

// ---------- BlockRef ----------

/// 16-byte self-validating reference to a 4 KiB block.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Eq, PartialEq, Default)]
pub struct BlockRef {
    pub disk_id: u16,    // [0..2]
    pub _pad: u16,       // [2..4]
    pub block_no: u32,   // [4..8]
    pub generation: u64, // [8..16]
}

const_assert_eq!(core::mem::size_of::<BlockRef>(), 16);

// ---------- BlobRef ----------

/// 16-byte reference to a contiguous byte extent in the blob zone.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Eq, PartialEq, Default)]
pub struct BlobRef {
    pub disk_id: u16,  // [0..2]
    pub _pad: u16,     // [2..4]
    pub block_no: u32, // [4..8]
    pub length: u64,   // [8..16]   length in bytes
}

const_assert_eq!(core::mem::size_of::<BlobRef>(), 16);

// ---------- RootPointer ----------

/// 408-byte root pointer. IMPL §2.2.
///
/// Holds two scalars (`seq`, `lsn`), 24 `BlockRef` slots, a `flags` word and
/// a trailing CRC32C over bytes `[0..404]`.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
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
    pub crc: u32,   // [404..408]  CRC32C of bytes [0..404]
}

const_assert_eq!(core::mem::size_of::<RootPointer>(), 408);

impl RootPointer {
    /// CRC slot offset, used by [`Self::recompute_crc`] and verification.
    pub const CRC_OFFSET: usize = 404;

    /// Recompute and store the CRC over bytes `[0..404]`.
    pub fn recompute_crc(&mut self) {
        self.crc = 0;
        let crc = crc32c::crc32c(&bytemuck::bytes_of(self)[..Self::CRC_OFFSET]);
        self.crc = crc;
    }

    /// Validate the stored CRC. Returns the captured `(expected, actual)` on
    /// mismatch.
    pub fn verify_crc(&self) -> Result<(), StorageError> {
        let expected = { self.crc };
        let mut copy = *self;
        copy.crc = 0;
        let actual = crc32c::crc32c(&bytemuck::bytes_of(&copy)[..Self::CRC_OFFSET]);
        if expected == actual {
            Ok(())
        } else {
            Err(StorageError::CrcMismatch { expected, actual })
        }
    }
}

impl BlockRef {
    /// Construct a new `BlockRef`.
    pub fn new(disk_id: u16, block_no: u32, generation: u64) -> Self {
        Self {
            disk_id,
            _pad: 0,
            block_no,
            generation,
        }
    }

    /// Generation check: the pointer is fresh iff its `generation` matches
    /// the bucket's current generation. Returns `true` when fresh.
    /// IMPL §12.3.
    pub fn dereference_check(&self, alloc: &crate::alloc::BucketAllocEntry) -> bool {
        let bucket_gen = { alloc.generation } as u64;
        let ref_gen = { self.generation };
        bucket_gen == ref_gen
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
        use crate::alloc::{BucketAllocEntry, BucketDataType};
        let alloc = BucketAllocEntry {
            generation: 42,
            data_type: BucketDataType::Index as u8,
            flags: 0,
            dirty_sectors: 0,
            last_modify_lsn: 0,
        };
        let fresh = BlockRef::new(0, 1024, 42);
        assert!(fresh.dereference_check(&alloc));
        let stale = BlockRef::new(0, 1024, 41);
        assert!(!stale.dereference_check(&alloc));
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
