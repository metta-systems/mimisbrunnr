/// Block size in bytes (4 KiB).
pub const BLOCK_SIZE: u64 = 4096;

/// Superblock size: one block.
pub const SUPERBLOCK_SIZE: u64 = BLOCK_SIZE;

/// WAL size: 64 MiB circular buffer.
pub const WAL_SIZE: u64 = 64 * 1024 * 1024;

/// Minimum device size to hold a valid filesystem.
/// 2 superblocks + WAL + alloc bitmap (1 block min) + at least some zone space.
pub const MIN_DEVICE_SIZE: u64 = SUPERBLOCK_SIZE * 3 + WAL_SIZE + BLOCK_SIZE * 4;

/// Logical zone types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ZoneType {
    Index = 0,
    Metadata = 1,
    Blob = 2,
}

/// A contiguous extent belonging to a zone: (offset, size) in bytes, both block-aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoneExtent {
    pub offset: u64,
    pub size: u64,
}

impl ZoneExtent {
    pub fn new(offset: u64, size: u64) -> Self {
        debug_assert_eq!(offset % BLOCK_SIZE, 0);
        debug_assert_eq!(size % BLOCK_SIZE, 0);
        Self { offset, size }
    }

    /// Number of blocks in this extent.
    pub fn block_count(&self) -> u64 {
        self.size / BLOCK_SIZE
    }

    /// End offset (exclusive).
    pub fn end(&self) -> u64 {
        self.offset + self.size
    }

    /// Check if a byte offset falls within this extent.
    pub fn contains(&self, byte_offset: u64) -> bool {
        byte_offset >= self.offset && byte_offset < self.end()
    }
}

/// Runtime representation of the zone layout using extent lists.
///
/// Each logical zone (index, metadata, blob) is described by one or more extents.
/// Initially each zone has a single extent (equivalent to `ZoneLayout`), but zones
/// can grow by appending additional extents carved from free space.
#[derive(Debug, Clone)]
pub struct ExtentLayout {
    pub index_extents: Vec<ZoneExtent>,
    pub metadata_extents: Vec<ZoneExtent>,
    pub blob_extents: Vec<ZoneExtent>,
    pub device_capacity: u64,
    // Fixed layout fields (not per-zone)
    pub superblock_primary: u64,
    pub superblock_copy: u64,
    pub wal_offset: u64,
    pub alloc_bitmap_offset: u64,
    pub alloc_bitmap_size: u64,
    pub block_class_map_offset: u64,
    pub block_class_map_size: u64,
    pub superblock_backup: u64,
}

impl ExtentLayout {
    /// Total size of a zone across all its extents.
    pub fn zone_size(&self, zone: ZoneType) -> u64 {
        self.extents(zone).iter().map(|e| e.size).sum()
    }

    /// Total blocks in a zone across all its extents.
    pub fn zone_blocks(&self, zone: ZoneType) -> u64 {
        self.extents(zone).iter().map(|e| e.block_count()).sum()
    }

    /// Get the extents for a zone.
    pub fn extents(&self, zone: ZoneType) -> &[ZoneExtent] {
        match zone {
            ZoneType::Index => &self.index_extents,
            ZoneType::Metadata => &self.metadata_extents,
            ZoneType::Blob => &self.blob_extents,
        }
    }

    /// Get mutable extents for a zone.
    pub fn extents_mut(&mut self, zone: ZoneType) -> &mut Vec<ZoneExtent> {
        match zone {
            ZoneType::Index => &mut self.index_extents,
            ZoneType::Metadata => &mut self.metadata_extents,
            ZoneType::Blob => &mut self.blob_extents,
        }
    }

    /// Translate a logical byte offset within a zone to a physical device offset.
    ///
    /// Returns `None` if the logical offset is out of range.
    pub fn logical_to_physical(&self, zone: ZoneType, logical_offset: u64) -> Option<u64> {
        let mut remaining = logical_offset;
        for extent in self.extents(zone) {
            if remaining < extent.size {
                return Some(extent.offset + remaining);
            }
            remaining -= extent.size;
        }
        None
    }

    /// First extent's offset for a zone (convenience for single-extent zones).
    pub fn zone_offset(&self, zone: ZoneType) -> u64 {
        self.extents(zone).first().map_or(0, |e| e.offset)
    }

    /// Convenience accessors matching the old ZoneLayout field names.
    pub fn index_zone_offset(&self) -> u64 { self.zone_offset(ZoneType::Index) }
    pub fn index_zone_size(&self) -> u64 { self.zone_size(ZoneType::Index) }
    pub fn metadata_zone_offset(&self) -> u64 { self.zone_offset(ZoneType::Metadata) }
    pub fn metadata_zone_size(&self) -> u64 { self.zone_size(ZoneType::Metadata) }
    pub fn blob_zone_offset(&self) -> u64 { self.zone_offset(ZoneType::Blob) }
    pub fn blob_zone_size(&self) -> u64 { self.zone_size(ZoneType::Blob) }
}

impl From<&ZoneLayout> for ExtentLayout {
    fn from(zl: &ZoneLayout) -> Self {
        Self {
            index_extents: vec![ZoneExtent::new(zl.index_zone_offset, zl.index_zone_size)],
            metadata_extents: vec![ZoneExtent::new(zl.metadata_zone_offset, zl.metadata_zone_size)],
            blob_extents: vec![ZoneExtent::new(zl.blob_zone_offset, zl.blob_zone_size)],
            device_capacity: zl.device_capacity,
            superblock_primary: zl.superblock_primary,
            superblock_copy: zl.superblock_copy,
            wal_offset: zl.wal_offset,
            alloc_bitmap_offset: zl.alloc_bitmap_offset,
            alloc_bitmap_size: zl.alloc_bitmap_size,
            // v1 has no block class map — set to zero
            block_class_map_offset: 0,
            block_class_map_size: 0,
            superblock_backup: zl.superblock_backup,
        }
    }
}

/// Describes the byte offsets of each zone on a single disk.
///
/// Layout (from design doc §6.1):
/// ```text
///  0                    Superblock (4KB)
///  4K                   Superblock copy
///  8K                   Write-Ahead Log (64MB circular buffer)
///  8K+64M               Allocation bitmap
///  ...                  Index Zone (~1-5% of disk)
///  ...                  Metadata Zone (~1-2% of disk)
///  ...                  Blob Zone (~95% of disk)
///  end-4K               Superblock backup copy
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoneLayout {
    /// Offset of the primary superblock.
    pub superblock_primary: u64,
    /// Offset of the superblock copy (immediately after primary).
    pub superblock_copy: u64,
    /// Offset of the WAL region.
    pub wal_offset: u64,
    /// Offset of the allocation bitmap.
    pub alloc_bitmap_offset: u64,
    /// Size of the allocation bitmap in bytes.
    pub alloc_bitmap_size: u64,
    /// Offset of the index zone.
    pub index_zone_offset: u64,
    /// Size of the index zone in bytes.
    pub index_zone_size: u64,
    /// Offset of the metadata zone.
    pub metadata_zone_offset: u64,
    /// Size of the metadata zone in bytes.
    pub metadata_zone_size: u64,
    /// Offset of the blob zone.
    pub blob_zone_offset: u64,
    /// Size of the blob zone in bytes.
    pub blob_zone_size: u64,
    /// Offset of the backup superblock at end of disk.
    pub superblock_backup: u64,
    /// Total device capacity.
    pub device_capacity: u64,
}

impl ZoneLayout {
    /// Compute a zone layout for a device of the given capacity.
    ///
    /// Allocates:
    /// - Index zone: 3% of usable space
    /// - Metadata zone: 2% of usable space
    /// - Blob zone: remainder (~95%)
    pub fn compute(device_capacity: u64) -> Option<Self> {
        if device_capacity < MIN_DEVICE_SIZE {
            return None;
        }

        let superblock_primary = 0;
        let superblock_copy = SUPERBLOCK_SIZE;
        let wal_offset = SUPERBLOCK_SIZE * 2;
        let alloc_bitmap_offset = wal_offset + WAL_SIZE;

        // Reserve space for backup superblock at end
        let superblock_backup = device_capacity - SUPERBLOCK_SIZE;

        // Usable space: between alloc bitmap and backup superblock
        let total_usable = superblock_backup - alloc_bitmap_offset;

        // Allocation bitmap: 1 bit per block of usable space
        let usable_blocks = total_usable / BLOCK_SIZE;
        let alloc_bitmap_size = align_up(usable_blocks.div_ceil(8), BLOCK_SIZE);

        let data_start = alloc_bitmap_offset + alloc_bitmap_size;
        let data_size = superblock_backup - data_start;

        // Zone sizing: 3% index, 2% metadata, 95% blob
        let index_zone_size = align_up(data_size * 3 / 100, BLOCK_SIZE);
        let metadata_zone_size = align_up(data_size * 2 / 100, BLOCK_SIZE);
        let blob_zone_size = data_size - index_zone_size - metadata_zone_size;

        let index_zone_offset = data_start;
        let metadata_zone_offset = index_zone_offset + index_zone_size;
        let blob_zone_offset = metadata_zone_offset + metadata_zone_size;

        Some(Self {
            superblock_primary,
            superblock_copy,
            wal_offset,
            alloc_bitmap_offset,
            alloc_bitmap_size,
            index_zone_offset,
            index_zone_size,
            metadata_zone_offset,
            metadata_zone_size,
            blob_zone_offset,
            blob_zone_size,
            superblock_backup,
            device_capacity,
        })
    }
}

fn align_up(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment) * alignment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_small_returns_none() {
        assert!(ZoneLayout::compute(1024).is_none());
    }

    #[test]
    fn minimum_device() {
        let layout = ZoneLayout::compute(MIN_DEVICE_SIZE).unwrap();
        assert_eq!(layout.superblock_primary, 0);
        assert_eq!(layout.superblock_copy, BLOCK_SIZE);
        assert_eq!(layout.wal_offset, BLOCK_SIZE * 2);
    }

    #[test]
    fn one_gig_device() {
        let cap = 1024 * 1024 * 1024; // 1 GiB
        let layout = ZoneLayout::compute(cap).unwrap();

        // Zones should be within device
        assert!(layout.blob_zone_offset + layout.blob_zone_size <= layout.superblock_backup);
        assert_eq!(layout.superblock_backup, cap - SUPERBLOCK_SIZE);

        // Index zone ~3%, metadata ~2%, blob ~95%
        let data_size = layout.index_zone_size + layout.metadata_zone_size + layout.blob_zone_size;
        let index_pct = layout.index_zone_size as f64 / data_size as f64;
        let meta_pct = layout.metadata_zone_size as f64 / data_size as f64;

        assert!(
            (index_pct - 0.03).abs() < 0.01,
            "index zone ~3%, got {index_pct}"
        );
        assert!(
            (meta_pct - 0.02).abs() < 0.01,
            "metadata zone ~2%, got {meta_pct}"
        );
    }

    #[test]
    fn zones_are_block_aligned() {
        let cap = 256 * 1024 * 1024; // 256 MiB
        let layout = ZoneLayout::compute(cap).unwrap();

        assert_eq!(layout.index_zone_offset % BLOCK_SIZE, 0);
        assert_eq!(layout.index_zone_size % BLOCK_SIZE, 0);
        assert_eq!(layout.metadata_zone_offset % BLOCK_SIZE, 0);
        assert_eq!(layout.metadata_zone_size % BLOCK_SIZE, 0);
        assert_eq!(layout.blob_zone_offset % BLOCK_SIZE, 0);
        assert_eq!(layout.alloc_bitmap_size % BLOCK_SIZE, 0);
    }

    #[test]
    fn zones_dont_overlap() {
        let cap = 512 * 1024 * 1024;
        let layout = ZoneLayout::compute(cap).unwrap();

        // WAL ends before alloc bitmap
        assert!(layout.wal_offset + WAL_SIZE <= layout.alloc_bitmap_offset);
        // Alloc bitmap ends before index zone
        assert!(layout.alloc_bitmap_offset + layout.alloc_bitmap_size <= layout.index_zone_offset);
        // Index zone ends before metadata zone
        assert!(layout.index_zone_offset + layout.index_zone_size <= layout.metadata_zone_offset);
        // Metadata zone ends before blob zone
        assert!(layout.metadata_zone_offset + layout.metadata_zone_size <= layout.blob_zone_offset);
        // Blob zone ends before backup superblock
        assert!(layout.blob_zone_offset + layout.blob_zone_size <= layout.superblock_backup);
    }

    // --- ExtentLayout tests ---

    #[test]
    fn extent_layout_from_zone_layout() {
        let cap = 256 * 1024 * 1024;
        let zl = ZoneLayout::compute(cap).unwrap();
        let el = ExtentLayout::from(&zl);

        assert_eq!(el.index_extents.len(), 1);
        assert_eq!(el.metadata_extents.len(), 1);
        assert_eq!(el.blob_extents.len(), 1);
        assert_eq!(el.index_zone_offset(), zl.index_zone_offset);
        assert_eq!(el.index_zone_size(), zl.index_zone_size);
        assert_eq!(el.metadata_zone_offset(), zl.metadata_zone_offset);
        assert_eq!(el.metadata_zone_size(), zl.metadata_zone_size);
        assert_eq!(el.blob_zone_offset(), zl.blob_zone_offset);
        assert_eq!(el.blob_zone_size(), zl.blob_zone_size);
    }

    #[test]
    fn logical_to_physical_single_extent() {
        let cap = 256 * 1024 * 1024;
        let zl = ZoneLayout::compute(cap).unwrap();
        let el = ExtentLayout::from(&zl);

        // Offset 0 in index zone = physical index_zone_offset
        assert_eq!(
            el.logical_to_physical(ZoneType::Index, 0),
            Some(zl.index_zone_offset)
        );
        // One block in
        assert_eq!(
            el.logical_to_physical(ZoneType::Index, BLOCK_SIZE),
            Some(zl.index_zone_offset + BLOCK_SIZE)
        );
        // Past end
        assert_eq!(
            el.logical_to_physical(ZoneType::Index, zl.index_zone_size),
            None
        );
    }

    #[test]
    fn logical_to_physical_multi_extent() {
        let mut el = ExtentLayout::from(&ZoneLayout::compute(256 * 1024 * 1024).unwrap());
        // Simulate a second extent for the index zone
        let second = ZoneExtent::new(100 * BLOCK_SIZE, 10 * BLOCK_SIZE);
        let first_size = el.index_extents[0].size;
        el.index_extents.push(second);

        // Logical offset at start of second extent
        assert_eq!(
            el.logical_to_physical(ZoneType::Index, first_size),
            Some(100 * BLOCK_SIZE)
        );
        // One block into second extent
        assert_eq!(
            el.logical_to_physical(ZoneType::Index, first_size + BLOCK_SIZE),
            Some(100 * BLOCK_SIZE + BLOCK_SIZE)
        );
    }

    #[test]
    fn zone_extent_contains() {
        let e = ZoneExtent::new(1000 * BLOCK_SIZE, 10 * BLOCK_SIZE);
        assert!(e.contains(1000 * BLOCK_SIZE));
        assert!(e.contains(1009 * BLOCK_SIZE));
        assert!(!e.contains(1010 * BLOCK_SIZE));
        assert!(!e.contains(999 * BLOCK_SIZE));
    }

    #[test]
    fn zone_total_size_multi_extent() {
        let mut el = ExtentLayout::from(&ZoneLayout::compute(256 * 1024 * 1024).unwrap());
        let original = el.zone_size(ZoneType::Index);
        el.index_extents.push(ZoneExtent::new(0, 20 * BLOCK_SIZE));
        assert_eq!(el.zone_size(ZoneType::Index), original + 20 * BLOCK_SIZE);
    }
}
