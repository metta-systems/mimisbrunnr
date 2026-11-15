//! Freespace LRU mirror (ALLOCATION-DESIGN§1.4).
//!
//! Per ALLOCATION-DESIGN§1.4 each disk has its own BTREE-DESIGN§1 B+ tree rooted at
//! `DiskDescriptorOnDisk.freespace_root`, keyed by
//! `(fragmentation_band: u8, bucket_no: u32)`. The allocator uses band
//! 0 for the foreground fast path, band 255 for the most-fragmented end,
//! and copygc walks the high bands to reclaim space.
//!
//! ## Persistence
//!
//! On disk the table occupies one 256 KiB [`Region`] of
//! [`BtreeKind::FreespaceLru`], keyed by [`FreespaceLruKey`] with empty values:
//! ALLOCATION-DESIGN§1.4 describes a key-only tree.
//!
//! TODO(ALLOCATION5): one LRU per disk, rooted in its disk descriptor, keyed
//! `(band, bucket_no)` (ALLOCATION-DESIGN§1.4); for now one pool-scoped tree
//! whose key carries `disk_id`.
//!
//! TODO(ALLOCATION5): live-allocator integration. The table
//! ships only the persistence skeleton — the foreground allocator's
//! "scan band 0" fast path, the lazy-reband-on-8-sector-boundary
//! producer, and the copygc consumer come with ALLOCATION5 and RECONCILE3.

use std::{collections::BTreeSet, ops::Bound};

use crate::{
    block::BtreeKind,
    block_device::BlockDevice,
    error::StorageError,
    region::{FieldKind, Region, RegionKey, RegionParams, RunEntry},
};

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // not yet consumed by the engine layout.
pub const FREESPACE_LRU_REGION_SIZE: u64 = 256 * 1024;
const FREESPACE_LRU_REGION_SIZE_LOG2: u8 = 18;

/// Key of the freespace LRU, ordered band first so the allocator fast path
/// (`band == 0`) is a contiguous prefix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FreespaceLruKey {
    /// `0` = empty, `255` = nearly full, `1..=254` = in between (formula in
    /// ALLOCATION-DESIGN§1.4).
    pub fragmentation_band: u8,
    /// Disk id; goes when each disk has its own LRU (ALLOCATION5).
    pub disk_id: u16,
    /// Bucket number within the disk.
    pub bucket_no: u32,
}

impl FreespaceLruKey {
    /// Construct a new key.
    pub const fn new(fragmentation_band: u8, disk_id: u16, bucket_no: u32) -> Self {
        Self {
            fragmentation_band,
            disk_id,
            bucket_no,
        }
    }
}

impl RegionKey for FreespaceLruKey {
    const FIELDS: &'static [FieldKind] = &[FieldKind::Unsigned, FieldKind::Unsigned, FieldKind::Unsigned];

    fn field_values(&self, out: &mut [u64]) {
        out[0] = self.fragmentation_band as u64;
        out[1] = self.disk_id as u64;
        out[2] = self.bucket_no as u64;
    }

    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError> {
        let out_of_range = |_| StorageError::RegionCorrupt("freespace key field out of range");
        Ok(Self {
            fragmentation_band: u8::try_from(fields[0]).map_err(out_of_range)?,
            disk_id: u16::try_from(fields[1]).map_err(out_of_range)?,
            bucket_no: u32::try_from(fields[2]).map_err(out_of_range)?,
        })
    }
}

/// In-memory mirror of the ALLOCATION-DESIGN§1.4 freespace LRU.
///
/// Lookups are point (`contains`) or banded prefix scans (`iter_band`). This
/// only ships the persistence skeleton; live allocator integration
/// (foreground "scan band 0", lazy reband, copygc producer) is ALLOCATION5 /
/// RECONCILE3.
#[derive(Clone, Debug, Default)]
pub struct FreespaceLru {
    entries: BTreeSet<FreespaceLruKey>,
}

impl FreespaceLru {
    /// Empty freespace LRU.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `(band, disk_id, bucket_no)`. Idempotent — the second
    /// call is a no-op.
    pub fn insert(&mut self, fragmentation_band: u8, disk_id: u16, bucket_no: u32) {
        self.entries
            .insert(FreespaceLruKey::new(fragmentation_band, disk_id, bucket_no));
    }

    /// Remove `(band, disk_id, bucket_no)`. Returns `true` iff the
    /// entry was present.
    pub fn remove(&mut self, fragmentation_band: u8, disk_id: u16, bucket_no: u32) -> bool {
        self.entries
            .remove(&FreespaceLruKey::new(fragmentation_band, disk_id, bucket_no))
    }

    /// `true` iff `(band, disk_id, bucket_no)` is present.
    pub fn contains(&self, fragmentation_band: u8, disk_id: u16, bucket_no: u32) -> bool {
        self.entries
            .contains(&FreespaceLruKey::new(fragmentation_band, disk_id, bucket_no))
    }

    /// Iterate over every key with the given fragmentation band, in
    /// ascending `(disk_id, bucket_no)` order. The allocator fast path
    /// uses this with `band == 0`; copygc uses it with high bands.
    pub fn iter_band(&self, fragmentation_band: u8) -> impl Iterator<Item = &FreespaceLruKey> {
        let low = FreespaceLruKey::new(fragmentation_band, 0, 0);
        let high = FreespaceLruKey::new(fragmentation_band, u16::MAX, u32::MAX);
        self.entries.range((Bound::Included(low), Bound::Included(high)))
    }

    /// Number of entries in the LRU.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the LRU is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over every entry in `(band, disk_id, bucket_no)` order.
    pub fn iter(&self) -> impl Iterator<Item = &FreespaceLruKey> {
        self.entries.iter()
    }

    /// Write the LRU as the 256 KiB region at byte `offset`.
    pub fn flush_to_region<D: BlockDevice>(&self, device: &D, offset: u64) -> Result<(), StorageError> {
        let params = RegionParams {
            kind: BtreeKind::FreespaceLru,
            level: 0,
            region_size_log2: FREESPACE_LRU_REGION_SIZE_LOG2,
            node_seq: 0,
            created_lsn: 0,
        };
        let mut region = Region::create_in_place(device, offset, params, &[])?;
        let entries: Vec<(FreespaceLruKey, RunEntry<'_>)> =
            self.entries.iter().map(|key| (*key, RunEntry::Value(&[]))).collect();
        region.append_entries(device, 0, &entries)
    }

    /// Read the LRU from the region at byte `offset`; a never-written
    /// (all-zero) region is an empty LRU.
    pub fn load_from_region<D: BlockDevice>(device: &D, offset: u64) -> Result<Self, StorageError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe)?;
        if probe.iter().all(|&byte| byte == 0) {
            return Ok(Self::default());
        }
        let region = Region::open(device, offset, BtreeKind::FreespaceLru)?;
        let entries = region
            .entries::<D, FreespaceLruKey>(device)?
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        Ok(Self { entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::file_device::FileBlockDevice;
    use tempfile::TempDir;

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("freespace_lru.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    #[test]
    fn freespace_lru_key_orders_by_band_then_disk_then_bucket() {
        // Band first so band==0 fast-path is a contiguous prefix scan.
        let a = FreespaceLruKey::new(0, 0, 0);
        let b = FreespaceLruKey::new(0, 0, 100);
        let c = FreespaceLruKey::new(0, 1, 0);
        let d = FreespaceLruKey::new(1, 0, 0);
        let e = FreespaceLruKey::new(255, 0, 0);
        assert!(a < b);
        assert!(b < c);
        assert!(c < d);
        assert!(d < e);
    }

    #[test]
    fn insert_remove_contains() {
        let mut lru = FreespaceLru::new();
        assert!(lru.is_empty());
        lru.insert(0, 0, 100);
        lru.insert(255, 0, 200);
        lru.insert(100, 1, 0);
        assert_eq!(lru.len(), 3);
        assert!(lru.contains(0, 0, 100));
        assert!(lru.contains(255, 0, 200));
        assert!(!lru.contains(0, 0, 200));

        // Idempotent insert.
        lru.insert(0, 0, 100);
        assert_eq!(lru.len(), 3);

        assert!(lru.remove(0, 0, 100));
        assert!(!lru.remove(0, 0, 100));
        assert_eq!(lru.len(), 2);
    }

    #[test]
    fn iter_band_filters_by_band() {
        let mut lru = FreespaceLru::new();
        lru.insert(0, 0, 1);
        lru.insert(0, 0, 2);
        lru.insert(0, 1, 0);
        lru.insert(100, 0, 5);
        lru.insert(255, 0, 10);

        let band0: Vec<_> = lru.iter_band(0).collect();
        assert_eq!(band0.len(), 3);
        assert!(band0.iter().all(|key| key.fragmentation_band == 0));

        assert_eq!(lru.iter_band(100).count(), 1);
        assert_eq!(lru.iter_band(1).count(), 0);
    }

    #[test]
    fn freespace_lru_region_round_trip_empty() {
        let (_dir, dev) = fresh_device();
        let lru = FreespaceLru::load_from_region(&dev, 0).unwrap();
        assert!(lru.is_empty());
    }

    #[test]
    fn freespace_lru_region_round_trip_preserves_entries() {
        let (_dir, dev) = fresh_device();
        let mut lru = FreespaceLru::new();
        // Spread across bands 0, 100, 255 — covers the allocator fast
        // path, copygc mid-band, and the saturated end.
        for bucket in 0u32..20 {
            lru.insert(0, 0, bucket);
        }
        for bucket in 100u32..110 {
            lru.insert(100, 0, bucket);
        }
        for bucket in 200u32..205 {
            lru.insert(255, 0, bucket);
        }
        // Cross-disk entry to prove disk_id round-trips.
        lru.insert(50, 7, 999);
        let total = lru.len();

        lru.flush_to_region(&dev, 0).unwrap();
        let back = FreespaceLru::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), total);
        assert_eq!(back.iter().collect::<Vec<_>>(), lru.iter().collect::<Vec<_>>());
        assert!(back.contains(50, 7, 999));
        assert_eq!(back.iter_band(0).count(), 20);
    }

    #[test]
    fn freespace_lru_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = FreespaceLru::new();
        first.insert(0, 0, 1);
        first.insert(0, 0, 2);
        first.flush_to_region(&dev, 0).unwrap();

        let mut second = FreespaceLru::new();
        second.insert(255, 0, 99);
        second.flush_to_region(&dev, 0).unwrap();

        let back = FreespaceLru::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        assert!(back.contains(255, 0, 99));
        assert!(!back.contains(0, 0, 1));
    }

    #[test]
    fn freespace_lru_region_kind_mismatch_errors() {
        // Write a BucketAlloc-kind region at offset 0, then try to load
        // it as FreespaceLru — must surface RegionKindMismatch.
        let (_dir, dev) = fresh_device();
        let mut alloc = crate::alloc::BucketAllocTable::new();
        alloc.insert(
            0,
            crate::alloc::BucketAllocEntry::new(1, crate::alloc::BucketKind::Free, 0, 0),
        );
        alloc.flush_to_region(&dev, 0).unwrap();
        let err = FreespaceLru::load_from_region(&dev, 0).err();
        assert!(matches!(err, Some(StorageError::RegionKindMismatch { expected: 13, actual: 12 })));
    }
}
