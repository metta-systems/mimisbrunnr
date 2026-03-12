use crate::{BLOCK_SIZE, BlockDevice, StorageError};

/// Classification of a block's contents.
///
/// Stored as a 4-bit nibble in the BlockClassMap, allowing up to 16 classes.
/// Currently only 4 are defined; remaining values are reserved for future use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BlockClass {
    /// Block is free / unclassified.
    Free = 0,
    /// Index data (tag bitmaps, forward index, KV index).
    Index = 1,
    /// Metadata (object records).
    Metadata = 2,
    /// Blob data (file contents).
    Blob = 3,
}

impl BlockClass {
    pub fn from_nibble(n: u8) -> Self {
        match n & 0x0F {
            0 => Self::Free,
            1 => Self::Index,
            2 => Self::Metadata,
            3 => Self::Blob,
            _ => Self::Free, // reserved values treated as free
        }
    }

    pub fn as_nibble(self) -> u8 {
        self as u8
    }
}

/// Nibble-packed map of block classes. Two classes per byte (low nibble = even block,
/// high nibble = odd block).
///
/// This tracks what *kind* of data each block holds, independent of the allocation
/// bitmap. The block class map enables future migration to a unified block allocator
/// where zones are logical rather than physical.
pub struct BlockClassMap {
    /// Nibble-packed storage: byte i holds classes for blocks 2i (low) and 2i+1 (high).
    data: Vec<u8>,
    /// Number of blocks tracked.
    total_blocks: u64,
    /// Offset on disk where this map is stored.
    disk_offset: u64,
}

impl BlockClassMap {
    /// Create a new class map with all blocks set to `Free`.
    pub fn new(total_blocks: u64, disk_offset: u64) -> Self {
        let byte_count = total_blocks.div_ceil(2) as usize;
        Self {
            data: vec![0u8; byte_count],
            total_blocks,
            disk_offset,
        }
    }

    /// Get the class of a block.
    pub fn get(&self, block: u64) -> BlockClass {
        if block >= self.total_blocks {
            return BlockClass::Free;
        }
        let byte_idx = (block / 2) as usize;
        let nibble = if block.is_multiple_of(2) {
            self.data[byte_idx] & 0x0F
        } else {
            (self.data[byte_idx] >> 4) & 0x0F
        };
        BlockClass::from_nibble(nibble)
    }

    /// Set the class of a block.
    pub fn set(&mut self, block: u64, class: BlockClass) {
        debug_assert!(block < self.total_blocks);
        let byte_idx = (block / 2) as usize;
        let nibble = class.as_nibble();
        if block.is_multiple_of(2) {
            self.data[byte_idx] = (self.data[byte_idx] & 0xF0) | nibble;
        } else {
            self.data[byte_idx] = (self.data[byte_idx] & 0x0F) | (nibble << 4);
        }
    }

    /// Set the class for a contiguous range of blocks.
    pub fn set_range(&mut self, start: u64, count: u64, class: BlockClass) {
        for b in start..start + count {
            self.set(b, class);
        }
    }

    /// Count blocks of a given class.
    pub fn count(&self, class: BlockClass) -> u64 {
        let target = class.as_nibble();
        let mut n = 0u64;
        for block in 0..self.total_blocks {
            let byte_idx = (block / 2) as usize;
            let nibble = if block.is_multiple_of(2) {
                self.data[byte_idx] & 0x0F
            } else {
                (self.data[byte_idx] >> 4) & 0x0F
            };
            if nibble == target {
                n += 1;
            }
        }
        n
    }

    /// Total blocks tracked.
    pub fn total_blocks(&self) -> u64 {
        self.total_blocks
    }

    /// Size of the class map in bytes on disk (block-aligned).
    pub fn disk_size(total_blocks: u64) -> u64 {
        let raw = total_blocks.div_ceil(2);
        raw.div_ceil(BLOCK_SIZE) * BLOCK_SIZE
    }

    /// Load the class map from disk.
    pub fn load(
        dev: &dyn BlockDevice,
        disk_offset: u64,
        total_blocks: u64,
    ) -> Result<Self, StorageError> {
        let byte_count = total_blocks.div_ceil(2) as usize;
        let mut data = vec![0u8; byte_count];
        dev.read_at(disk_offset, &mut data)?;
        Ok(Self {
            data,
            total_blocks,
            disk_offset,
        })
    }

    /// Flush the class map to disk.
    pub fn flush(&self, dev: &dyn BlockDevice) -> Result<(), StorageError> {
        dev.write_at(self.disk_offset, &self.data)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_map_is_all_free() {
        let map = BlockClassMap::new(100, 0);
        for b in 0..100 {
            assert_eq!(map.get(b), BlockClass::Free);
        }
        assert_eq!(map.count(BlockClass::Free), 100);
    }

    #[test]
    fn set_and_get() {
        let mut map = BlockClassMap::new(100, 0);
        map.set(0, BlockClass::Index);
        map.set(1, BlockClass::Metadata);
        map.set(2, BlockClass::Blob);
        map.set(3, BlockClass::Free);

        assert_eq!(map.get(0), BlockClass::Index);
        assert_eq!(map.get(1), BlockClass::Metadata);
        assert_eq!(map.get(2), BlockClass::Blob);
        assert_eq!(map.get(3), BlockClass::Free);
    }

    #[test]
    fn nibble_packing() {
        let mut map = BlockClassMap::new(4, 0);
        // Even block in low nibble, odd block in high nibble
        map.set(0, BlockClass::Index);   // byte 0 low
        map.set(1, BlockClass::Blob);    // byte 0 high
        assert_eq!(map.data[0], 0x31); // Blob=3 in high, Index=1 in low

        map.set(2, BlockClass::Metadata); // byte 1 low
        map.set(3, BlockClass::Index);    // byte 1 high
        assert_eq!(map.data[1], 0x12); // Index=1 in high, Metadata=2 in low
    }

    #[test]
    fn set_range() {
        let mut map = BlockClassMap::new(20, 0);
        map.set_range(5, 10, BlockClass::Blob);
        assert_eq!(map.count(BlockClass::Blob), 10);
        assert_eq!(map.get(4), BlockClass::Free);
        assert_eq!(map.get(5), BlockClass::Blob);
        assert_eq!(map.get(14), BlockClass::Blob);
        assert_eq!(map.get(15), BlockClass::Free);
    }

    #[test]
    fn count_by_class() {
        let mut map = BlockClassMap::new(10, 0);
        map.set_range(0, 3, BlockClass::Index);
        map.set_range(3, 2, BlockClass::Metadata);
        map.set_range(5, 5, BlockClass::Blob);

        assert_eq!(map.count(BlockClass::Free), 0);
        assert_eq!(map.count(BlockClass::Index), 3);
        assert_eq!(map.count(BlockClass::Metadata), 2);
        assert_eq!(map.count(BlockClass::Blob), 5);
    }

    #[test]
    fn out_of_range_returns_free() {
        let map = BlockClassMap::new(10, 0);
        assert_eq!(map.get(999), BlockClass::Free);
    }

    #[test]
    fn disk_size_aligned() {
        // 100 blocks = 50 bytes → 1 block (4096)
        assert_eq!(BlockClassMap::disk_size(100), BLOCK_SIZE);
        // 8192 blocks = 4096 bytes = exactly 1 block
        assert_eq!(BlockClassMap::disk_size(8192), BLOCK_SIZE);
        // 8193 blocks = 4097 bytes → 2 blocks
        assert_eq!(BlockClassMap::disk_size(8193), 2 * BLOCK_SIZE);
    }

    #[test]
    fn persist_and_reload() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let dev = crate::FileBlockDevice::open(tmp.path(), 1024 * 1024).unwrap();

        let mut map = BlockClassMap::new(1000, 0);
        map.set(0, BlockClass::Index);
        map.set(500, BlockClass::Blob);
        map.set(999, BlockClass::Metadata);
        map.flush(&dev).unwrap();

        let map2 = BlockClassMap::load(&dev, 0, 1000).unwrap();
        assert_eq!(map2.get(0), BlockClass::Index);
        assert_eq!(map2.get(500), BlockClass::Blob);
        assert_eq!(map2.get(999), BlockClass::Metadata);
        assert_eq!(map2.get(1), BlockClass::Free);
    }

    #[test]
    fn from_nibble_roundtrip() {
        for class in [BlockClass::Free, BlockClass::Index, BlockClass::Metadata, BlockClass::Blob] {
            assert_eq!(BlockClass::from_nibble(class.as_nibble()), class);
        }
    }

    #[test]
    fn reserved_nibbles_are_free() {
        for n in 4..16 {
            assert_eq!(BlockClass::from_nibble(n), BlockClass::Free);
        }
    }
}
