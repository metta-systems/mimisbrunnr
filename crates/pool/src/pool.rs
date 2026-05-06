//! `PoolStateRoot` — pool-wide on-disk state (IMPL §10.4).
//!
//! A single 4 KiB block holding pool-wide scalars and the inline
//! disk-descriptor array. For pools with ≤12 disks the entire descriptor
//! list lives inline; for >12 the spec calls for spillover into the
//! `BtreeKind::DiskDescriptors` B+ tree (rooted at
//! `RootPointer.disks_overflow_root`). The B+ tree spillover is *not yet
//! implemented* in this phase — see TODO below.

use {
    bytemuck::{Pod, Zeroable},
    log::trace,
    mimisbrunnr_storage::{
        BLOCK_PREAMBLE_MAGIC_BLOCK, BLOCK_SIZE, BlockDevice, BlockHeader, BlockKind, block_crc,
    },
    static_assertions::const_assert_eq,
};

use crate::{
    disk::{DISK_DESCRIPTOR_ON_DISK_SIZE, DiskDescriptorOnDisk},
    error::PoolError,
};

/// Number of inline `DiskDescriptorOnDisk` slots.
pub const POOL_STATE_ROOT_INLINE_DISKS: usize = 12;

/// Total byte size of [`PoolStateRoot`] (= 4 KiB block).
pub const POOL_STATE_ROOT_SIZE: usize = BLOCK_SIZE;

// Reserved trailer width: chosen so the block totals 4096 bytes exactly.
//   header (32) + disk_count (4) + cluster_node_count (4)
//   + inline_disks (12 × 256 = 3072) + reserved (980) + crc (4) = 4096.
const POOL_STATE_ROOT_RESERVED: usize = 980;

const POOL_STATE_ROOT_CRC_OFFSET: usize = POOL_STATE_ROOT_SIZE - 4; // 4092

/// Format-version slot for [`PoolStateRoot`]'s `BlockHeader`.
const POOL_STATE_ROOT_FORMAT_VERSION: u16 = 1;

/// 4 KiB on-disk pool state block. IMPL §10.4 lines 2125–2132.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct PoolStateRoot {
    pub header: BlockHeader,        // [0..32]    kind = PoolStateRoot
    pub disk_count: u32,            // [32..36]
    pub cluster_node_count: u32,    // [36..40]
    pub inline_disks: [DiskDescriptorOnDisk; POOL_STATE_ROOT_INLINE_DISKS], // [40..3112]
    pub _reserved: [u8; POOL_STATE_ROOT_RESERVED], // [3112..4092]
    pub crc: u32,                   // [4092..4096]   CRC32C of bytes [0..4092]
}

const_assert_eq!(core::mem::size_of::<PoolStateRoot>(), POOL_STATE_ROOT_SIZE);
const_assert_eq!(
    core::mem::size_of::<DiskDescriptorOnDisk>() * POOL_STATE_ROOT_INLINE_DISKS,
    DISK_DESCRIPTOR_ON_DISK_SIZE * POOL_STATE_ROOT_INLINE_DISKS
);

impl PoolStateRoot {
    /// Build a zero-initialised `PoolStateRoot` block with header populated.
    /// The CRC is **not** computed by this constructor — call
    /// [`Self::recompute_crc`] before persisting.
    pub fn new_blank() -> Self {
        let mut out: Self = bytemuck::Zeroable::zeroed();
        out.header = BlockHeader::new(
            BlockKind::PoolStateRoot,
            POOL_STATE_ROOT_FORMAT_VERSION,
            (POOL_STATE_ROOT_SIZE - core::mem::size_of::<BlockHeader>() - 4) as u32,
        );
        out
    }

    /// Recompute and store the trailing CRC32C over bytes `[0..4092]`.
    pub fn recompute_crc(&mut self) {
        self.crc = 0;
        let crc = block_crc(&bytemuck::bytes_of(self)[..POOL_STATE_ROOT_CRC_OFFSET]);
        self.crc = crc;
    }

    /// Validate the trailing CRC.
    pub fn verify_crc(&self) -> Result<(), PoolError> {
        let expected = { self.crc };
        let mut copy = *self;
        copy.crc = 0;
        let actual = block_crc(&bytemuck::bytes_of(&copy)[..POOL_STATE_ROOT_CRC_OFFSET]);
        if expected == actual {
            Ok(())
        } else {
            Err(PoolError::CrcMismatch { expected, actual })
        }
    }

    /// Active inline-disk count, clipped to `POOL_STATE_ROOT_INLINE_DISKS`.
    /// Disks beyond the inline cap live in the overflow B+ tree (not yet
    /// implemented in this phase).
    pub fn inline_disk_count(&self) -> usize {
        let raw = { self.disk_count } as usize;
        raw.min(POOL_STATE_ROOT_INLINE_DISKS)
    }

    /// Set the inline `disks` array, recomputing `disk_count`. Errors if
    /// more than [`POOL_STATE_ROOT_INLINE_DISKS`] disks are supplied — that
    /// case requires the overflow B+ tree which is not yet wired.
    pub fn set_inline_disks(&mut self, disks: &[DiskDescriptorOnDisk]) -> Result<(), PoolError> {
        if disks.len() > POOL_STATE_ROOT_INLINE_DISKS {
            return Err(PoolError::DiskOverflowUnsupported {
                count: disks.len() as u32,
                max: POOL_STATE_ROOT_INLINE_DISKS as u32,
            });
        }
        // Zero the inline array first so previously-occupied slots clear.
        self.inline_disks = [DiskDescriptorOnDisk::default(); POOL_STATE_ROOT_INLINE_DISKS];
        for (slot, disk) in self.inline_disks.iter_mut().zip(disks.iter()) {
            *slot = *disk;
        }
        self.disk_count = disks.len() as u32;
        Ok(())
    }

    /// Iterator over the live inline descriptors.
    pub fn inline_iter(&self) -> impl Iterator<Item = &DiskDescriptorOnDisk> {
        let count = self.inline_disk_count();
        self.inline_disks[..count].iter()
    }

    /// Validate the block header's magic and kind.
    fn validate_header(&self) -> Result<(), PoolError> {
        let magic = { self.header.pre.magic };
        if magic != BLOCK_PREAMBLE_MAGIC_BLOCK {
            return Err(PoolError::InvalidPayload(format!(
                "bad block preamble magic: {magic:?}"
            )));
        }
        let kind_raw = { self.header.pre.kind };
        if kind_raw != BlockKind::PoolStateRoot as u16 {
            return Err(PoolError::InvalidBlockKind(kind_raw));
        }
        Ok(())
    }

    /// Persist this block to `device` at byte `offset`. Recomputes the CRC.
    pub fn write(&mut self, device: &dyn BlockDevice, offset: u64) -> Result<(), PoolError> {
        trace!("PoolStateRoot::write offset={offset}");
        self.recompute_crc();
        device.write_at(offset, bytemuck::bytes_of(self))?;
        Ok(())
    }

    /// Read a `PoolStateRoot` from `device` at byte `offset`. Validates the
    /// block header magic, kind, and trailing CRC.
    pub fn read(device: &dyn BlockDevice, offset: u64) -> Result<Self, PoolError> {
        trace!("PoolStateRoot::read offset={offset}");
        let mut buf = vec![0u8; POOL_STATE_ROOT_SIZE];
        device.read_at(offset, &mut buf)?;
        let psr: &Self = bytemuck::try_from_bytes(&buf)
            .map_err(|e| PoolError::InvalidPayload(format!("pod cast failed: {e}")))?;
        let psr = *psr;
        psr.validate_header()?;
        psr.verify_crc()?;
        Ok(psr)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::disk::DiskDescriptorOnDisk,
        mimisbrunnr_storage::FileBlockDevice,
        mimisbrunnr_types::{DiskState, MediaType, StorageTier},
        tempfile::NamedTempFile,
    };

    fn make_disk(id: u16, path: &str) -> DiskDescriptorOnDisk {
        DiskDescriptorOnDisk::new(
            id,
            MediaType::Ssd,
            StorageTier::Warm,
            DiskState::Online,
            1024 * 1024 * 1024,
            path,
        )
        .unwrap()
    }

    #[test]
    fn pool_state_root_is_4096() {
        assert_eq!(core::mem::size_of::<PoolStateRoot>(), 4096);
    }

    #[test]
    fn round_trip_pod_bytes() {
        let mut psr = PoolStateRoot::new_blank();
        psr.set_inline_disks(&[make_disk(1, "/disk1"), make_disk(2, "/disk2")])
            .unwrap();
        psr.recompute_crc();
        let bytes = bytemuck::bytes_of(&psr).to_vec();
        let back: &PoolStateRoot = bytemuck::from_bytes(&bytes);
        assert_eq!({ back.disk_count }, 2);
        back.verify_crc().unwrap();
    }

    #[test]
    fn read_write_through_block_device() {
        let tmp = NamedTempFile::new().unwrap();
        let dev = FileBlockDevice::open(tmp.path(), 1 << 20).unwrap();

        let mut psr = PoolStateRoot::new_blank();
        psr.cluster_node_count = 1;
        psr.set_inline_disks(&[make_disk(0, "/disk0"), make_disk(1, "/disk1")])
            .unwrap();
        psr.write(&dev, 0).unwrap();

        let back = PoolStateRoot::read(&dev, 0).unwrap();
        assert_eq!({ back.disk_count }, 2);
        assert_eq!({ back.cluster_node_count }, 1);
        let names: Vec<_> = back
            .inline_iter()
            .map(|d| d.path_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["/disk0", "/disk1"]);
    }

    #[test]
    fn corrupt_crc_rejected() {
        let tmp = NamedTempFile::new().unwrap();
        let dev = FileBlockDevice::open(tmp.path(), 1 << 20).unwrap();
        let mut psr = PoolStateRoot::new_blank();
        psr.write(&dev, 0).unwrap();

        // Flip a payload byte without recomputing the CRC.
        let mut buf = vec![0u8; POOL_STATE_ROOT_SIZE];
        dev.read_at(0, &mut buf).unwrap();
        buf[40] ^= 0xFF;
        dev.write_at(0, &buf).unwrap();

        let err = PoolStateRoot::read(&dev, 0).unwrap_err();
        assert!(matches!(err, PoolError::CrcMismatch { .. }));
    }

    #[test]
    fn wrong_block_kind_rejected() {
        let tmp = NamedTempFile::new().unwrap();
        let dev = FileBlockDevice::open(tmp.path(), 1 << 20).unwrap();
        let mut psr = PoolStateRoot::new_blank();
        psr.write(&dev, 0).unwrap();

        // Splice a different BlockKind discriminant into the preamble and fix the CRC.
        let mut buf = vec![0u8; POOL_STATE_ROOT_SIZE];
        dev.read_at(0, &mut buf).unwrap();
        buf[4] = BlockKind::ZoneMap as u8; // pre.kind low byte
        buf[5] = 0;
        // Recompute trailing CRC.
        buf[POOL_STATE_ROOT_CRC_OFFSET..POOL_STATE_ROOT_CRC_OFFSET + 4].copy_from_slice(&[0; 4]);
        let new_crc = block_crc(&buf[..POOL_STATE_ROOT_CRC_OFFSET]);
        buf[POOL_STATE_ROOT_CRC_OFFSET..POOL_STATE_ROOT_CRC_OFFSET + 4]
            .copy_from_slice(&new_crc.to_le_bytes());
        dev.write_at(0, &buf).unwrap();

        let err = PoolStateRoot::read(&dev, 0).unwrap_err();
        assert!(matches!(err, PoolError::InvalidBlockKind(_)));
    }

    #[test]
    fn overflow_inline_rejected() {
        let mut psr = PoolStateRoot::new_blank();
        let many: Vec<_> = (0..(POOL_STATE_ROOT_INLINE_DISKS as u16 + 1))
            .map(|i| make_disk(i, &format!("/d{i}")))
            .collect();
        let err = psr.set_inline_disks(&many).unwrap_err();
        assert!(matches!(err, PoolError::DiskOverflowUnsupported { .. }));
    }

    #[test]
    fn fits_exactly_twelve() {
        let mut psr = PoolStateRoot::new_blank();
        let twelve: Vec<_> = (0..POOL_STATE_ROOT_INLINE_DISKS as u16)
            .map(|i| make_disk(i, &format!("/d{i}")))
            .collect();
        psr.set_inline_disks(&twelve).unwrap();
        assert_eq!(psr.inline_disk_count(), POOL_STATE_ROOT_INLINE_DISKS);
    }
}
