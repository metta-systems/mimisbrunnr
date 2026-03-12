use {
    crate::{StorageError, layout::{ExtentLayout, ZoneExtent}},
    log::trace,
};

/// Magic bytes identifying a Mímisbrunnr superblock: "MIMIS\x01\x00\x00"
const MAGIC: [u8; 8] = *b"MIMIR\x01\0\0";

/// Current on-disk format version.
const FORMAT_VERSION: u32 = 1;

/// Superblock size in bytes (fits in one 4K block).
const SUPERBLOCK_BYTES: usize = 128;

/// The superblock anchors the filesystem. Written at three locations:
/// offset 0, offset 4K, and at end-4K (backup).
///
/// Binary layout (128 bytes, all little-endian):
/// ```text
///  [0..8]    magic
///  [8..12]   format_version
///  [12..14]  node_id
///  [14..16]  disk_id
///  [16..24]  device_capacity
///  [24..32]  index_zone_offset   (first extent)
///  [32..40]  index_zone_size     (first extent)
///  [40..48]  metadata_zone_offset (first extent)
///  [48..56]  metadata_zone_size   (first extent)
///  [56..64]  blob_zone_offset    (first extent)
///  [64..72]  blob_zone_size      (first extent)
///  [72..80]  wal_offset
///  [80..88]  alloc_bitmap_offset
///  [88..96]  alloc_bitmap_size
///  [96..104] creation_timestamp_ns
///  [104..112] last_checkpoint_lsn
///  [112..120] zone_map_offset (0 = single-extent zones, >0 = read ZoneMap for full extents)
///  [120..124] checksum (CRC32C of bytes [0..120])
///  [124..128] padding
/// ```
#[derive(Debug, Clone)]
pub struct Superblock {
    pub node_id: u16,
    pub disk_id: u16,
    pub layout: ExtentLayout,
    pub creation_timestamp_ns: i64,
    pub last_checkpoint_lsn: u64,
    /// Offset of the zone map block on disk. 0 means single-extent zones (inline in superblock).
    pub zone_map_offset: u64,
}

impl PartialEq for Superblock {
    fn eq(&self, other: &Self) -> bool {
        self.node_id == other.node_id
            && self.disk_id == other.disk_id
            && self.creation_timestamp_ns == other.creation_timestamp_ns
            && self.last_checkpoint_lsn == other.last_checkpoint_lsn
            && self.zone_map_offset == other.zone_map_offset
            // Compare layout fields that are serialized
            && self.layout.device_capacity == other.layout.device_capacity
            && self.layout.wal_offset == other.layout.wal_offset
            && self.layout.alloc_bitmap_offset == other.layout.alloc_bitmap_offset
            && self.layout.alloc_bitmap_size == other.layout.alloc_bitmap_size
            && self.layout.index_extents == other.layout.index_extents
            && self.layout.metadata_extents == other.layout.metadata_extents
            && self.layout.blob_extents == other.layout.blob_extents
    }
}

impl Eq for Superblock {}

impl Superblock {
    /// Create a new superblock for the given layout.
    pub fn new(node_id: u16, disk_id: u16, layout: ExtentLayout) -> Self {
        Self {
            node_id,
            disk_id,
            layout,
            creation_timestamp_ns: 0,
            last_checkpoint_lsn: 0,
            zone_map_offset: 0,
        }
    }

    /// Serialize to a 128-byte buffer.
    ///
    /// Only the first extent of each zone is stored inline. If zones have
    /// multiple extents, the caller must also write a ZoneMap block and set
    /// `zone_map_offset` before calling this.
    pub fn to_bytes(&self) -> [u8; SUPERBLOCK_BYTES] {
        let mut buf = [0u8; SUPERBLOCK_BYTES];
        let l = &self.layout;

        buf[0..8].copy_from_slice(&MAGIC);
        buf[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf[12..14].copy_from_slice(&self.node_id.to_le_bytes());
        buf[14..16].copy_from_slice(&self.disk_id.to_le_bytes());
        buf[16..24].copy_from_slice(&l.device_capacity.to_le_bytes());
        // First extent of each zone (bootstrap info)
        buf[24..32].copy_from_slice(&l.index_zone_offset().to_le_bytes());
        buf[32..40].copy_from_slice(&l.index_extents[0].size.to_le_bytes());
        buf[40..48].copy_from_slice(&l.metadata_zone_offset().to_le_bytes());
        buf[48..56].copy_from_slice(&l.metadata_extents[0].size.to_le_bytes());
        buf[56..64].copy_from_slice(&l.blob_zone_offset().to_le_bytes());
        buf[64..72].copy_from_slice(&l.blob_extents[0].size.to_le_bytes());
        buf[72..80].copy_from_slice(&l.wal_offset.to_le_bytes());
        buf[80..88].copy_from_slice(&l.alloc_bitmap_offset.to_le_bytes());
        buf[88..96].copy_from_slice(&l.alloc_bitmap_size.to_le_bytes());
        buf[96..104].copy_from_slice(&self.creation_timestamp_ns.to_le_bytes());
        buf[104..112].copy_from_slice(&self.last_checkpoint_lsn.to_le_bytes());
        buf[112..120].copy_from_slice(&self.zone_map_offset.to_le_bytes());

        let crc = crc32fast::hash(&buf[0..120]);
        buf[120..124].copy_from_slice(&crc.to_le_bytes());

        buf
    }

    /// Deserialize from a 128-byte buffer.
    ///
    /// Reconstructs the layout with single extents from the inline data.
    /// If `zone_map_offset` is non-zero, the caller should read the ZoneMap
    /// block and replace the extent lists with the full data.
    pub fn from_bytes(buf: &[u8; SUPERBLOCK_BYTES]) -> Result<Self, StorageError> {
        if buf[0..8] != MAGIC {
            return Err(StorageError::InvalidMagic);
        }

        let version = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        if version != FORMAT_VERSION {
            return Err(StorageError::UnsupportedVersion(version));
        }

        let expected_crc = u32::from_le_bytes(buf[120..124].try_into().unwrap());
        let actual_crc = crc32fast::hash(&buf[0..120]);
        if expected_crc != actual_crc {
            return Err(StorageError::ChecksumMismatch {
                expected: expected_crc,
                actual: actual_crc,
            });
        }

        let node_id = u16::from_le_bytes(buf[12..14].try_into().unwrap());
        let disk_id = u16::from_le_bytes(buf[14..16].try_into().unwrap());
        let device_capacity = u64::from_le_bytes(buf[16..24].try_into().unwrap());
        let index_zone_offset = u64::from_le_bytes(buf[24..32].try_into().unwrap());
        let index_zone_size = u64::from_le_bytes(buf[32..40].try_into().unwrap());
        let metadata_zone_offset = u64::from_le_bytes(buf[40..48].try_into().unwrap());
        let metadata_zone_size = u64::from_le_bytes(buf[48..56].try_into().unwrap());
        let blob_zone_offset = u64::from_le_bytes(buf[56..64].try_into().unwrap());
        let blob_zone_size = u64::from_le_bytes(buf[64..72].try_into().unwrap());
        let wal_offset = u64::from_le_bytes(buf[72..80].try_into().unwrap());
        let alloc_bitmap_offset = u64::from_le_bytes(buf[80..88].try_into().unwrap());
        let alloc_bitmap_size = u64::from_le_bytes(buf[88..96].try_into().unwrap());
        let creation_timestamp_ns = i64::from_le_bytes(buf[96..104].try_into().unwrap());
        let last_checkpoint_lsn = u64::from_le_bytes(buf[104..112].try_into().unwrap());
        let zone_map_offset = u64::from_le_bytes(buf[112..120].try_into().unwrap());

        let layout = ExtentLayout {
            index_extents: vec![ZoneExtent::new(index_zone_offset, index_zone_size)],
            metadata_extents: vec![ZoneExtent::new(metadata_zone_offset, metadata_zone_size)],
            blob_extents: vec![ZoneExtent::new(blob_zone_offset, blob_zone_size)],
            device_capacity,
            superblock_primary: 0,
            superblock_copy: crate::SUPERBLOCK_SIZE,
            wal_offset,
            alloc_bitmap_offset,
            alloc_bitmap_size,
            block_class_map_offset: 0,
            block_class_map_size: 0,
            superblock_backup: device_capacity - crate::SUPERBLOCK_SIZE,
        };

        Ok(Self {
            node_id,
            disk_id,
            layout,
            creation_timestamp_ns,
            last_checkpoint_lsn,
            zone_map_offset,
        })
    }

    /// Write superblock to all three locations on the device.
    pub fn write_to(&self, dev: &dyn crate::BlockDevice) -> Result<(), StorageError> {
        trace!(
            "superblock::write_to primary={:#x} copy={:#x} backup={:#x}",
            self.layout.superblock_primary,
            self.layout.superblock_copy,
            self.layout.superblock_backup
        );
        let bytes = self.to_bytes();
        // Pad to full block
        let mut block = [0u8; crate::BLOCK_SIZE as usize];
        block[..SUPERBLOCK_BYTES].copy_from_slice(&bytes);

        dev.write_at(self.layout.superblock_primary, &block)?;
        dev.write_at(self.layout.superblock_copy, &block)?;
        dev.write_at(self.layout.superblock_backup, &block)?;
        dev.sync()?;
        Ok(())
    }

    /// Read and validate superblock from the primary location.
    /// Falls back to copy and backup on failure.
    ///
    /// If `zone_map_offset` is non-zero, the caller should subsequently read
    /// the ZoneMap block to get the full extent lists and update the layout.
    pub fn read_from(dev: &dyn crate::BlockDevice) -> Result<Self, StorageError> {
        let mut block = [0u8; crate::BLOCK_SIZE as usize];

        // Try primary
        trace!("superblock::read_from trying primary at offset 0");
        if dev.read_at(0, &mut block).is_ok() {
            let bytes: &[u8; SUPERBLOCK_BYTES] = block[..SUPERBLOCK_BYTES].try_into().unwrap();
            if let Ok(sb) = Self::from_bytes(bytes) {
                return Ok(sb);
            }
        }

        // Try copy at offset 4K
        trace!(
            "superblock::read_from trying copy at offset {:#x}",
            crate::SUPERBLOCK_SIZE
        );
        if dev.read_at(crate::SUPERBLOCK_SIZE, &mut block).is_ok() {
            let bytes: &[u8; SUPERBLOCK_BYTES] = block[..SUPERBLOCK_BYTES].try_into().unwrap();
            if let Ok(sb) = Self::from_bytes(bytes) {
                return Ok(sb);
            }
        }

        // Try backup at end
        trace!("superblock::read_from trying backup");
        // we need to know the device size
        let cap = dev.capacity();
        if cap >= crate::SUPERBLOCK_SIZE {
            let backup_offset = cap - crate::SUPERBLOCK_SIZE;
            if dev.read_at(backup_offset, &mut block).is_ok() {
                let bytes: &[u8; SUPERBLOCK_BYTES] = block[..SUPERBLOCK_BYTES].try_into().unwrap();
                if let Ok(sb) = Self::from_bytes(bytes) {
                    return Ok(sb);
                }
            }
        }

        Err(StorageError::InvalidMagic)
    }

    /// Read superblock and load full extent layout from ZoneMap if present.
    ///
    /// This is the preferred way to load a superblock — it handles the
    /// two-phase read (superblock → zone map) automatically.
    pub fn read_with_extents(dev: &dyn crate::BlockDevice) -> Result<Self, StorageError> {
        let mut sb = Self::read_from(dev)?;
        if sb.zone_map_offset != 0 {
            let extents = crate::ZoneMap::read_from(dev, sb.zone_map_offset)?;
            sb.layout.index_extents = extents.index;
            sb.layout.metadata_extents = extents.metadata;
            sb.layout.blob_extents = extents.blob;
        }
        Ok(sb)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{BlockDevice, layout::MIN_DEVICE_SIZE},
    };

    fn test_superblock() -> Superblock {
        let layout = ExtentLayout::compute(256 * 1024 * 1024).unwrap();
        let mut sb = Superblock::new(1, 0, layout);
        sb.creation_timestamp_ns = 1_700_000_000_000_000_000;
        sb.last_checkpoint_lsn = 42;
        sb
    }

    #[test]
    fn serialize_round_trip() {
        let sb = test_superblock();
        let bytes = sb.to_bytes();
        let sb2 = Superblock::from_bytes(&bytes).unwrap();
        assert_eq!(sb, sb2);
    }

    #[test]
    fn invalid_magic_rejected() {
        let sb = test_superblock();
        let mut bytes = sb.to_bytes();
        bytes[0] = b'X';
        assert!(matches!(
            Superblock::from_bytes(&bytes),
            Err(StorageError::InvalidMagic)
        ));
    }

    #[test]
    fn corrupted_checksum_rejected() {
        let sb = test_superblock();
        let mut bytes = sb.to_bytes();
        bytes[50] ^= 0xFF; // Flip a byte in the data
        assert!(matches!(
            Superblock::from_bytes(&bytes),
            Err(StorageError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn write_and_read_from_device() {
        use crate::FileBlockDevice;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let cap = 256 * 1024 * 1024u64;
        let dev = FileBlockDevice::open(tmp.path(), cap).unwrap();

        let layout = ExtentLayout::compute(cap).unwrap();
        let sb = Superblock::new(7, 0, layout);
        sb.write_to(&dev).unwrap();

        let sb2 = Superblock::read_from(&dev).unwrap();
        assert_eq!(sb, sb2);
    }

    #[test]
    fn fallback_to_copy_on_primary_corruption() {
        use crate::FileBlockDevice;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let cap = 256 * 1024 * 1024u64;
        let dev = FileBlockDevice::open(tmp.path(), cap).unwrap();

        let layout = ExtentLayout::compute(cap).unwrap();
        let sb = Superblock::new(3, 0, layout);
        sb.write_to(&dev).unwrap();

        // Corrupt primary superblock
        dev.write_at(0, &[0xFF; 128]).unwrap();

        let sb2 = Superblock::read_from(&dev).unwrap();
        assert_eq!(sb, sb2);
    }

    #[test]
    fn fallback_to_backup_on_both_corruption() {
        use crate::FileBlockDevice;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let cap = 256 * 1024 * 1024u64;
        let dev = FileBlockDevice::open(tmp.path(), cap).unwrap();

        let layout = ExtentLayout::compute(cap).unwrap();
        let sb = Superblock::new(5, 0, layout);
        sb.write_to(&dev).unwrap();

        // Corrupt primary and copy
        dev.write_at(0, &[0xFF; 128]).unwrap();
        dev.write_at(crate::SUPERBLOCK_SIZE, &[0xFF; 128]).unwrap();

        let sb2 = Superblock::read_from(&dev).unwrap();
        assert_eq!(sb, sb2);
    }

    #[test]
    fn min_device_size_works() {
        let layout = ExtentLayout::compute(MIN_DEVICE_SIZE).unwrap();
        let sb = Superblock::new(0, 0, layout);
        let bytes = sb.to_bytes();
        let sb2 = Superblock::from_bytes(&bytes).unwrap();
        assert_eq!(sb, sb2);
    }

    #[test]
    fn read_with_extents_loads_zone_map() {
        use crate::{FileBlockDevice, ZoneMap};

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let cap = 256 * 1024 * 1024u64;
        let dev = FileBlockDevice::open(tmp.path(), cap).unwrap();

        let mut layout = ExtentLayout::compute(cap).unwrap();
        // Add a second extent to the index zone
        let extra = crate::layout::ZoneExtent::new(100 * crate::BLOCK_SIZE, 10 * crate::BLOCK_SIZE);
        layout.index_extents.push(extra);

        // Write zone map at a known offset (use alloc_bitmap area for test simplicity)
        let zone_map_offset = layout.alloc_bitmap_offset;
        ZoneMap::write_to(&layout, &dev, zone_map_offset).unwrap();

        let mut sb = Superblock::new(1, 0, layout.clone());
        sb.zone_map_offset = zone_map_offset;
        sb.write_to(&dev).unwrap();

        // read_from only gets single inline extents
        let sb_basic = Superblock::read_from(&dev).unwrap();
        assert_eq!(sb_basic.layout.index_extents.len(), 1);

        // read_with_extents loads the full zone map
        let sb_full = Superblock::read_with_extents(&dev).unwrap();
        assert_eq!(sb_full.layout.index_extents.len(), 2);
        assert_eq!(sb_full.layout.index_extents[1], extra);
    }
}
