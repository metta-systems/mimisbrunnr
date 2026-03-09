use crate::{BLOCK_SIZE, BlockDevice, StorageError};

/// Block allocation bitmap for extent management.
///
/// Each bit represents one block. A set bit means the block is allocated.
/// The bitmap is persisted in the allocation bitmap zone on disk.
pub struct AllocBitmap {
    /// In-memory bitmap: bit N = 1 means block N is allocated.
    bits: Vec<u8>,
    /// Total number of blocks tracked.
    total_blocks: u64,
    /// Offset on disk where this bitmap is stored.
    disk_offset: u64,
}

impl AllocBitmap {
    /// Create a new empty allocation bitmap for `total_blocks` blocks.
    pub fn new(total_blocks: u64, disk_offset: u64) -> Self {
        let byte_count = total_blocks.div_ceil(8) as usize;
        Self {
            bits: vec![0u8; byte_count],
            total_blocks,
            disk_offset,
        }
    }

    /// Load the bitmap from disk.
    pub fn load(
        dev: &dyn BlockDevice,
        disk_offset: u64,
        total_blocks: u64,
    ) -> Result<Self, StorageError> {
        let byte_count = total_blocks.div_ceil(8) as usize;
        let mut bits = vec![0u8; byte_count];
        dev.read_at(disk_offset, &mut bits)?;
        Ok(Self {
            bits,
            total_blocks,
            disk_offset,
        })
    }

    /// Flush the bitmap to disk.
    pub fn flush(&self, dev: &dyn BlockDevice) -> Result<(), StorageError> {
        dev.write_at(self.disk_offset, &self.bits)?;
        Ok(())
    }

    /// Check if a block is allocated.
    pub fn is_allocated(&self, block: u64) -> bool {
        if block >= self.total_blocks {
            return false;
        }
        let byte_idx = (block / 8) as usize;
        let bit_idx = (block % 8) as u8;
        (self.bits[byte_idx] >> bit_idx) & 1 == 1
    }

    /// Mark a block as allocated.
    pub fn set(&mut self, block: u64) {
        debug_assert!(block < self.total_blocks);
        let byte_idx = (block / 8) as usize;
        let bit_idx = (block % 8) as u8;
        self.bits[byte_idx] |= 1 << bit_idx;
    }

    /// Mark a block as free.
    pub fn clear(&mut self, block: u64) {
        debug_assert!(block < self.total_blocks);
        let byte_idx = (block / 8) as usize;
        let bit_idx = (block % 8) as u8;
        self.bits[byte_idx] &= !(1 << bit_idx);
    }

    /// Allocate a contiguous run of `count` blocks. Returns the start block index.
    pub fn alloc(&mut self, count: u64) -> Result<u64, StorageError> {
        if count == 0 {
            return Err(StorageError::NoFreeSpace { requested: 0 });
        }

        let mut run_start = 0u64;
        let mut run_len = 0u64;

        for block in 0..self.total_blocks {
            if self.is_allocated(block) {
                run_start = block + 1;
                run_len = 0;
            } else {
                run_len += 1;
                if run_len == count {
                    // Found a run — mark all allocated
                    for b in run_start..run_start + count {
                        self.set(b);
                    }
                    return Ok(run_start);
                }
            }
        }

        Err(StorageError::NoFreeSpace { requested: count })
    }

    /// Free a contiguous run of `count` blocks starting at `start`.
    pub fn free(&mut self, start: u64, count: u64) {
        for b in start..start + count {
            self.clear(b);
        }
    }

    /// Number of free blocks.
    pub fn free_count(&self) -> u64 {
        self.total_blocks - self.allocated_count()
    }

    /// Number of allocated blocks.
    pub fn allocated_count(&self) -> u64 {
        self.bits.iter().map(|b| b.count_ones() as u64).sum()
    }

    /// Total blocks tracked.
    pub fn total_blocks(&self) -> u64 {
        self.total_blocks
    }

    /// Convert a block index to a byte offset (relative to the zone start).
    pub fn block_to_offset(block: u64) -> u64 {
        block * BLOCK_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_bitmap_is_empty() {
        let bm = AllocBitmap::new(1000, 0);
        assert_eq!(bm.allocated_count(), 0);
        assert_eq!(bm.free_count(), 1000);
    }

    #[test]
    fn set_and_check() {
        let mut bm = AllocBitmap::new(100, 0);
        assert!(!bm.is_allocated(5));
        bm.set(5);
        assert!(bm.is_allocated(5));
        assert!(!bm.is_allocated(4));
        assert!(!bm.is_allocated(6));
    }

    #[test]
    fn clear_block() {
        let mut bm = AllocBitmap::new(100, 0);
        bm.set(10);
        assert!(bm.is_allocated(10));
        bm.clear(10);
        assert!(!bm.is_allocated(10));
    }

    #[test]
    fn alloc_first_fit() {
        let mut bm = AllocBitmap::new(100, 0);
        let start = bm.alloc(5).unwrap();
        assert_eq!(start, 0);
        assert_eq!(bm.allocated_count(), 5);

        for b in 0..5 {
            assert!(bm.is_allocated(b));
        }
        assert!(!bm.is_allocated(5));
    }

    #[test]
    fn alloc_skips_allocated_blocks() {
        let mut bm = AllocBitmap::new(100, 0);
        // Allocate blocks 0-4
        bm.alloc(5).unwrap();
        // Next allocation should start at 5
        let start = bm.alloc(3).unwrap();
        assert_eq!(start, 5);
    }

    #[test]
    fn alloc_finds_gap() {
        let mut bm = AllocBitmap::new(100, 0);
        bm.alloc(5).unwrap(); // 0-4
        bm.alloc(5).unwrap(); // 5-9
        bm.free(2, 3); // free 2-4

        // Need 2 blocks — should find gap at 2
        let start = bm.alloc(2).unwrap();
        assert_eq!(start, 2);
    }

    #[test]
    fn alloc_no_space() {
        let mut bm = AllocBitmap::new(10, 0);
        bm.alloc(10).unwrap();
        assert!(matches!(bm.alloc(1), Err(StorageError::NoFreeSpace { .. })));
    }

    #[test]
    fn free_blocks() {
        let mut bm = AllocBitmap::new(100, 0);
        bm.alloc(10).unwrap();
        assert_eq!(bm.allocated_count(), 10);
        bm.free(0, 10);
        assert_eq!(bm.allocated_count(), 0);
    }

    #[test]
    fn block_to_offset_conversion() {
        assert_eq!(AllocBitmap::block_to_offset(0), 0);
        assert_eq!(AllocBitmap::block_to_offset(1), BLOCK_SIZE);
        assert_eq!(AllocBitmap::block_to_offset(10), 10 * BLOCK_SIZE);
    }

    #[test]
    fn persist_and_reload() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let dev = crate::FileBlockDevice::open(tmp.path(), 1024 * 1024).unwrap();

        let mut bm = AllocBitmap::new(1000, 0);
        bm.alloc(42).unwrap();
        bm.set(500);
        bm.flush(&dev).unwrap();

        let bm2 = AllocBitmap::load(&dev, 0, 1000).unwrap();
        assert_eq!(bm2.allocated_count(), 43); // 42 + 1
        assert!(bm2.is_allocated(0));
        assert!(bm2.is_allocated(41));
        assert!(!bm2.is_allocated(42));
        assert!(bm2.is_allocated(500));
    }
}
