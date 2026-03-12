use crate::{BLOCK_SIZE, BlockDevice, StorageError, layout::{ZoneExtent, ExtentLayout}};

/// Magic bytes for the zone map block: "ZMAP\x01\x00\x00\x00"
const ZONE_MAP_MAGIC: [u8; 8] = *b"ZMAP\x01\0\0\0";

/// Maximum extents per zone in the on-disk format.
/// With 3 zones and 16 bytes per extent entry, 255 extents per zone uses
/// 3 * (2 + 255 * 16) = 12,246 bytes, comfortably within one 4K block.
/// We'll use a more conservative limit that fits in one block.
const MAX_EXTENTS_PER_ZONE: usize = 80;

/// On-disk zone map: a single 4K block describing all zone extents.
///
/// Binary layout:
/// ```text
///  [0..8]     magic ("ZMAP\x01\0\0\0")
///  [8..10]    index_extent_count (u16 LE)
///  [10..12]   metadata_extent_count (u16 LE)
///  [12..14]   blob_extent_count (u16 LE)
///  [14..16]   reserved
///  [16..]     extent entries: each 16 bytes (u64 offset + u64 size)
///             first: index extents, then metadata, then blob
///  [4088..4092] checksum (CRC32C of bytes [0..4088])
///  [4092..4096] padding
/// ```
pub struct ZoneMap;

const HEADER_SIZE: usize = 16;
const EXTENT_ENTRY_SIZE: usize = 16;
const CHECKSUM_OFFSET: usize = BLOCK_SIZE as usize - 8;
const CHECKSUM_SIZE: usize = 4;

impl ZoneMap {
    /// Serialize an ExtentLayout's zone extents into a 4K block.
    pub fn to_block(layout: &ExtentLayout) -> Result<[u8; BLOCK_SIZE as usize], StorageError> {
        let idx_count = layout.index_extents.len();
        let meta_count = layout.metadata_extents.len();
        let blob_count = layout.blob_extents.len();

        if idx_count > MAX_EXTENTS_PER_ZONE
            || meta_count > MAX_EXTENTS_PER_ZONE
            || blob_count > MAX_EXTENTS_PER_ZONE
        {
            return Err(StorageError::CapacityExceeded {
                message: format!(
                    "too many extents: {idx_count}/{meta_count}/{blob_count} (max {MAX_EXTENTS_PER_ZONE})"
                ),
            });
        }

        let total_extents = idx_count + meta_count + blob_count;
        let needed = HEADER_SIZE + total_extents * EXTENT_ENTRY_SIZE;
        if needed > CHECKSUM_OFFSET {
            return Err(StorageError::CapacityExceeded {
                message: format!("zone map data ({needed} bytes) exceeds block capacity"),
            });
        }

        let mut buf = [0u8; BLOCK_SIZE as usize];

        // Header
        buf[0..8].copy_from_slice(&ZONE_MAP_MAGIC);
        buf[8..10].copy_from_slice(&(idx_count as u16).to_le_bytes());
        buf[10..12].copy_from_slice(&(meta_count as u16).to_le_bytes());
        buf[12..14].copy_from_slice(&(blob_count as u16).to_le_bytes());

        // Extent entries
        let mut pos = HEADER_SIZE;
        for extent in layout.index_extents.iter()
            .chain(layout.metadata_extents.iter())
            .chain(layout.blob_extents.iter())
        {
            buf[pos..pos + 8].copy_from_slice(&extent.offset.to_le_bytes());
            buf[pos + 8..pos + 16].copy_from_slice(&extent.size.to_le_bytes());
            pos += EXTENT_ENTRY_SIZE;
        }

        // Checksum
        let crc = crc32fast::hash(&buf[0..CHECKSUM_OFFSET]);
        buf[CHECKSUM_OFFSET..CHECKSUM_OFFSET + CHECKSUM_SIZE].copy_from_slice(&crc.to_le_bytes());

        Ok(buf)
    }

    /// Deserialize zone extents from a 4K block into the three extent lists.
    pub fn from_block(
        buf: &[u8; BLOCK_SIZE as usize],
    ) -> Result<(Vec<ZoneExtent>, Vec<ZoneExtent>, Vec<ZoneExtent>), StorageError> {
        // Magic check
        if buf[0..8] != ZONE_MAP_MAGIC {
            return Err(StorageError::InvalidMagic);
        }

        // Checksum
        let expected_crc = u32::from_le_bytes(buf[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].try_into().unwrap());
        let actual_crc = crc32fast::hash(&buf[0..CHECKSUM_OFFSET]);
        if expected_crc != actual_crc {
            return Err(StorageError::ChecksumMismatch {
                expected: expected_crc,
                actual: actual_crc,
            });
        }

        let idx_count = u16::from_le_bytes(buf[8..10].try_into().unwrap()) as usize;
        let meta_count = u16::from_le_bytes(buf[10..12].try_into().unwrap()) as usize;
        let blob_count = u16::from_le_bytes(buf[12..14].try_into().unwrap()) as usize;

        let mut pos = HEADER_SIZE;
        let mut read_extents = |count: usize| -> Vec<ZoneExtent> {
            let mut extents = Vec::with_capacity(count);
            for _ in 0..count {
                let offset = u64::from_le_bytes(buf[pos..pos + 8].try_into().unwrap());
                let size = u64::from_le_bytes(buf[pos + 8..pos + 16].try_into().unwrap());
                extents.push(ZoneExtent::new(offset, size));
                pos += EXTENT_ENTRY_SIZE;
            }
            extents
        };

        let index = read_extents(idx_count);
        let metadata = read_extents(meta_count);
        let blob = read_extents(blob_count);

        Ok((index, metadata, blob))
    }

    /// Write the zone map to disk at the given offset.
    pub fn write_to(
        layout: &ExtentLayout,
        dev: &dyn BlockDevice,
        offset: u64,
    ) -> Result<(), StorageError> {
        let block = Self::to_block(layout)?;
        dev.write_at(offset, &block)?;
        Ok(())
    }

    /// Read the zone map from disk at the given offset.
    pub fn read_from(
        dev: &dyn BlockDevice,
        offset: u64,
    ) -> Result<(Vec<ZoneExtent>, Vec<ZoneExtent>, Vec<ZoneExtent>), StorageError> {
        let mut block = [0u8; BLOCK_SIZE as usize];
        dev.read_at(offset, &mut block)?;
        Self::from_block(&block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::ZoneLayout;

    fn test_layout() -> ExtentLayout {
        ExtentLayout::from(&ZoneLayout::compute(256 * 1024 * 1024).unwrap())
    }

    #[test]
    fn roundtrip_single_extents() {
        let el = test_layout();
        let block = ZoneMap::to_block(&el).unwrap();
        let (idx, meta, blob) = ZoneMap::from_block(&block).unwrap();

        assert_eq!(idx.len(), 1);
        assert_eq!(meta.len(), 1);
        assert_eq!(blob.len(), 1);
        assert_eq!(idx[0], el.index_extents[0]);
        assert_eq!(meta[0], el.metadata_extents[0]);
        assert_eq!(blob[0], el.blob_extents[0]);
    }

    #[test]
    fn roundtrip_multi_extents() {
        let mut el = test_layout();
        el.index_extents.push(ZoneExtent::new(100 * BLOCK_SIZE, 10 * BLOCK_SIZE));
        el.metadata_extents.push(ZoneExtent::new(200 * BLOCK_SIZE, 5 * BLOCK_SIZE));

        let block = ZoneMap::to_block(&el).unwrap();
        let (idx, meta, blob) = ZoneMap::from_block(&block).unwrap();

        assert_eq!(idx.len(), 2);
        assert_eq!(meta.len(), 2);
        assert_eq!(blob.len(), 1);
        assert_eq!(idx[1], ZoneExtent::new(100 * BLOCK_SIZE, 10 * BLOCK_SIZE));
    }

    #[test]
    fn corrupted_magic_rejected() {
        let el = test_layout();
        let mut block = ZoneMap::to_block(&el).unwrap();
        block[0] = b'X';
        assert!(matches!(ZoneMap::from_block(&block), Err(StorageError::InvalidMagic)));
    }

    #[test]
    fn corrupted_checksum_rejected() {
        let el = test_layout();
        let mut block = ZoneMap::to_block(&el).unwrap();
        block[20] ^= 0xFF;
        assert!(matches!(
            ZoneMap::from_block(&block),
            Err(StorageError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn write_and_read_from_device() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let dev = crate::FileBlockDevice::open(tmp.path(), 1024 * 1024).unwrap();

        let el = test_layout();
        ZoneMap::write_to(&el, &dev, 0).unwrap();
        let (idx, meta, blob) = ZoneMap::read_from(&dev, 0).unwrap();

        assert_eq!(idx, el.index_extents);
        assert_eq!(meta, el.metadata_extents);
        assert_eq!(blob, el.blob_extents);
    }
}
