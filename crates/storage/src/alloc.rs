//! Per-disk bucket allocation table (ALLOCATION-DESIGN§1.2).
//!
//! This module owns the on-disk record types, the data-type enum, the flag
//! consts, an in-memory `BTreeMap`-backed mirror, and the BTREE-DESIGN§1 B+ tree
//! persistence path that lets the table live in a real
//! [`BtreeKind::BucketAlloc`](crate::block::BtreeKind::BucketAlloc) region.
//!
//! ## Persistence
//!
//! On disk the table occupies one 256 KiB [`Region`] of
//! [`BtreeKind::BucketAlloc`] keyed by `bucket_no`; values are the 8-byte
//! [`BucketAllocEntry`].
//!
//! `live_sectors` changes only through [`BucketAllocTable::add_live_sectors`]
//! and [`BucketAllocTable::remove_live_sectors`], called for the journal
//! entries and checkpoint node rewrites that add or drop references; no
//! journal entry of its own records it (ALLOCATION-DESIGN§1.2).
//! TODO(ALLOCATION10): call them from replay of the data ops and from the
//! checkpoint's region accounting.
//!
//! TODO(ALLOCATION4): one table per disk, rooted in its disk descriptor
//! (ALLOCATION-DESIGN§1.2); for now one pool-scoped table in the index zone.

use std::collections::BTreeMap;

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
};

use crate::{
    block::BtreeKind,
    block_device::BlockDevice,
    error::StorageError,
    region::{Region, RegionParams, RunEntry},
};

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // not yet consumed by the engine layout.
pub const BUCKET_ALLOC_REGION_SIZE: u64 = 256 * 1024;
const BUCKET_ALLOC_REGION_SIZE_LOG2: u8 = 18;

/// Flag, in the high nibble of `kind_flags`: freed, TRIM not yet issued
/// (ALLOCATION-DESIGN§1.7).
pub const BUCKET_FLAG_NEEDS_DISCARD: u8 = 1 << 4;

/// Mask of the `BUCKET_FLAG_*` bits in `kind_flags`.
const BUCKET_FLAGS_MASK: u8 = 0xf0;
/// Mask of the [`BucketKind`] bits in `kind_flags`.
const BUCKET_KIND_MASK: u8 = 0x0f;

/// Largest count a `u24` `live_sectors` field holds.
pub const LIVE_SECTORS_MAX: u32 = (1 << 24) - 1;

/// Per-bucket allocation record (8 B). ALLOCATION-DESIGN§1.2.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, Eq, PartialEq)]
pub struct BucketAllocEntry {
    /// `[0..4]` monotonic; matched by `BlockRef.generation`.
    pub generation: u32,
    /// `[4..5]` low 4 bits [`BucketKind`], high 4 bits `BUCKET_FLAG_*`.
    pub kind_flags: u8,
    /// `[5..8]` `u24` little-endian: live 4 KiB sectors in this bucket.
    pub live_sectors: [u8; 3],
}

const_assert_eq!(core::mem::size_of::<BucketAllocEntry>(), 8);
const_assert_eq!(core::mem::offset_of!(BucketAllocEntry, kind_flags), 4);
const_assert_eq!(core::mem::offset_of!(BucketAllocEntry, live_sectors), 5);

/// Size in bytes of [`BucketAllocEntry`] (8). Used by the
/// sorted-run length validator.
pub const BUCKET_ALLOC_ENTRY_SIZE: usize = 8;

impl BucketAllocEntry {
    /// Build an entry. `flags` must use only the high nibble and
    /// `live_sectors` must fit `u24`.
    pub fn new(generation: u32, kind: BucketKind, flags: u8, live_sectors: u32) -> Self {
        debug_assert_eq!(flags & BUCKET_KIND_MASK, 0);
        debug_assert!(live_sectors <= LIVE_SECTORS_MAX);
        let mut entry = Self {
            generation,
            kind_flags: (kind as u8) | (flags & BUCKET_FLAGS_MASK),
            live_sectors: [0; 3],
        };
        entry.set_live_sectors(live_sectors);
        entry
    }

    /// The bucket's kind; an unassigned discriminant is a format error.
    pub fn kind(&self) -> Result<BucketKind, StorageError> {
        BucketKind::from_u8(self.kind_flags & BUCKET_KIND_MASK)
    }

    /// Replace the kind, keeping the flags.
    pub fn set_kind(&mut self, kind: BucketKind) {
        self.kind_flags = (self.kind_flags & BUCKET_FLAGS_MASK) | kind as u8;
    }

    /// The `BUCKET_FLAG_*` bits.
    pub fn flags(&self) -> u8 {
        self.kind_flags & BUCKET_FLAGS_MASK
    }

    /// Replace the flags, keeping the kind.
    pub fn set_flags(&mut self, flags: u8) {
        debug_assert_eq!(flags & BUCKET_KIND_MASK, 0);
        self.kind_flags = (self.kind_flags & BUCKET_KIND_MASK) | (flags & BUCKET_FLAGS_MASK);
    }

    /// Live 4 KiB sectors in the bucket.
    pub fn live_sectors(&self) -> u32 {
        let [low, middle, high] = self.live_sectors;
        u32::from_le_bytes([low, middle, high, 0])
    }

    /// Set the live-sector count; must fit `u24`.
    pub fn set_live_sectors(&mut self, live_sectors: u32) {
        debug_assert!(live_sectors <= LIVE_SECTORS_MAX);
        let [low, middle, high, _] = live_sectors.to_le_bytes();
        self.live_sectors = [low, middle, high];
    }
}

/// What a bucket holds. **Discriminants pinned by ALLOCATION-DESIGN§1.2.**
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BucketKind {
    /// Not in use; freespace LRU candidate.
    Free = 0,
    /// Journal buckets (JOURNAL-DESIGN§1.1).
    Journal = 1,
    /// B+ tree nodes and positional regions (BTREE-DESIGN§1).
    Node = 2,
    /// Object extents and chunks (BLOB-DESIGN).
    Data = 3,
    /// Small-object blocks (BLOB-D4).
    Packed = 4,
    /// Erasure-coding stripes (future).
    Stripe = 5,
    /// Copy GC forward-progress reserve.
    Reserved = 6,
    /// A write, read or checksum failed in it; never allocated.
    Bad = 7,
}

impl BucketKind {
    /// Decode a pinned discriminant; 8–15 are unassigned.
    pub fn from_u8(value: u8) -> Result<Self, StorageError> {
        Ok(match value {
            0 => Self::Free,
            1 => Self::Journal,
            2 => Self::Node,
            3 => Self::Data,
            4 => Self::Packed,
            5 => Self::Stripe,
            6 => Self::Reserved,
            7 => Self::Bad,
            other => return Err(StorageError::InvalidBucketKind(other)),
        })
    }
}

/// In-memory placeholder for the per-disk bucket alloc table.
///
/// Backed by a `BTreeMap<u32, BucketAllocEntry>`. Keys are `bucket_no`.
///
/// This keeps this in-memory state pool-scoped (single allocator, no
/// per-disk split) while the on-disk wire shape is already
/// `(disk_id, bucket_no)`-keyed for forward compatibility with ALLOCATION4.
#[derive(Clone, Debug, Default)]
pub struct BucketAllocTable {
    entries: BTreeMap<u32, BucketAllocEntry>,
}

impl BucketAllocTable {
    /// Build an empty alloc table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert / overwrite the entry for `bucket_no`.
    pub fn insert(&mut self, bucket_no: u32, entry: BucketAllocEntry) {
        self.entries.insert(bucket_no, entry);
    }

    /// Look up the entry for `bucket_no`.
    pub fn get(&self, bucket_no: u32) -> Option<&BucketAllocEntry> {
        self.entries.get(&bucket_no)
    }

    /// Number of entries in the table.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over `(bucket_no, entry)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&u32, &BucketAllocEntry)> {
        self.entries.iter()
    }

    /// Count `sectors` newly live in `bucket_no`, a bucket of
    /// `sectors_per_bucket` sectors (ALLOCATION-DESIGN§1.2 live-sector
    /// accounting: a location newly written or a region newly reached
    /// from the root).
    pub fn add_live_sectors(
        &mut self,
        bucket_no: u32,
        sectors: u32,
        sectors_per_bucket: u32,
    ) -> Result<(), StorageError> {
        let entry = self
            .entries
            .get_mut(&bucket_no)
            .ok_or(StorageError::UnknownBucket { bucket_no })?;
        let live = entry.live_sectors();
        let capacity = sectors_per_bucket.min(LIVE_SECTORS_MAX);
        match live.checked_add(sectors) {
            Some(total) if total <= capacity => {
                entry.set_live_sectors(total);
                Ok(())
            }
            _ => Err(StorageError::LiveSectorsOverflow {
                bucket_no,
                live,
                added: sectors,
                capacity,
            }),
        }
    }

    /// Count `sectors` no longer live in `bucket_no` (a location whose last
    /// owner went, or a region the root no longer reaches).
    pub fn remove_live_sectors(&mut self, bucket_no: u32, sectors: u32) -> Result<(), StorageError> {
        let entry = self
            .entries
            .get_mut(&bucket_no)
            .ok_or(StorageError::UnknownBucket { bucket_no })?;
        let live = entry.live_sectors();
        let remaining = live.checked_sub(sectors).ok_or(StorageError::LiveSectorsUnderflow {
            bucket_no,
            live,
            removed: sectors,
        })?;
        entry.set_live_sectors(remaining);
        Ok(())
    }

    /// Write the table as the 256 KiB region at byte `offset`, keyed by
    /// `bucket_no`, each value an 8-byte [`BucketAllocEntry`].
    pub fn flush_to_region<D: BlockDevice>(&self, device: &D, offset: u64) -> Result<(), StorageError> {
        let mut region = Region::create_in_place(device, offset, region_params(), &[])?;
        let entries: Vec<(u32, RunEntry<'_>)> = self
            .entries
            .iter()
            .map(|(bucket_no, entry)| (*bucket_no, RunEntry::Value(bytemuck::bytes_of(entry))))
            .collect();
        region.append_entries(device, 0, &entries)
    }

    /// Read the table from the region at byte `offset`; a never-written
    /// (all-zero) region is an empty table.
    pub fn load_from_region<D: BlockDevice>(device: &D, offset: u64) -> Result<Self, StorageError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe)?;
        if probe.iter().all(|&byte| byte == 0) {
            return Ok(Self::default());
        }
        let region = Region::open(device, offset, BtreeKind::BucketAlloc)?;
        let mut entries = BTreeMap::new();
        for (bucket_no, value) in region.entries::<D, u32>(device)? {
            let entry = bytemuck::try_pod_read_unaligned::<BucketAllocEntry>(&value)
                .map_err(|_| StorageError::RegionCorrupt("bucket alloc entry is not 8 bytes"))?;
            entries.insert(bucket_no, entry);
        }
        Ok(Self { entries })
    }
}

fn region_params() -> RegionParams {
    RegionParams {
        kind: BtreeKind::BucketAlloc,
        level: 0,
        region_size_log2: BUCKET_ALLOC_REGION_SIZE_LOG2,
        node_seq: 0,
        created_lsn: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_alloc_entry_is_8_bytes() {
        assert_eq!(core::mem::size_of::<BucketAllocEntry>(), 8);
    }

    #[test]
    fn bucket_alloc_entry_fixed_bytes() {
        // generation 0x0102_0304, kind Data (3) with NEEDS_DISCARD (0x10),
        // live_sectors 0x0a0b0c — little-endian throughout.
        let entry = BucketAllocEntry::new(0x0102_0304, BucketKind::Data, BUCKET_FLAG_NEEDS_DISCARD, 0x0a_0b0c);
        assert_eq!(
            bytemuck::bytes_of(&entry),
            &[0x04, 0x03, 0x02, 0x01, 0x13, 0x0c, 0x0b, 0x0a]
        );
        assert_eq!(entry.kind().unwrap(), BucketKind::Data);
        assert_eq!(entry.flags(), BUCKET_FLAG_NEEDS_DISCARD);
        assert_eq!(entry.live_sectors(), 0x0a_0b0c);
    }

    #[test]
    fn full_256_mib_bucket_counts_65_536_live_sectors() {
        // bucket_size_log2 = 28: 2^(28 - 12) sectors.
        let sectors_per_bucket = 1u32 << (28 - 12);
        let mut table = BucketAllocTable::new();
        table.insert(3, BucketAllocEntry::new(1, BucketKind::Data, 0, 0));
        table.add_live_sectors(3, sectors_per_bucket, sectors_per_bucket).unwrap();
        assert_eq!(table.get(3).unwrap().live_sectors(), 65_536);
        let err = table.add_live_sectors(3, 1, sectors_per_bucket).unwrap_err();
        assert!(matches!(err, StorageError::LiveSectorsOverflow { live: 65_536, .. }));
    }

    #[test]
    fn live_sectors_add_and_remove_balance() {
        let mut table = BucketAllocTable::new();
        table.insert(1, BucketAllocEntry::new(4, BucketKind::Node, 0, 0));
        table.add_live_sectors(1, 64, 256).unwrap();
        table.add_live_sectors(1, 64, 256).unwrap();
        table.remove_live_sectors(1, 100).unwrap();
        assert_eq!(table.get(1).unwrap().live_sectors(), 28);
        let err = table.remove_live_sectors(1, 29).unwrap_err();
        assert!(matches!(err, StorageError::LiveSectorsUnderflow { live: 28, removed: 29, .. }));
        // A failed change leaves the count as it was.
        assert_eq!(table.get(1).unwrap().live_sectors(), 28);
        let err = table.add_live_sectors(9, 1, 256).unwrap_err();
        assert!(matches!(err, StorageError::UnknownBucket { bucket_no: 9 }));
    }

    #[test]
    fn kind_and_flags_set_independently() {
        let mut entry = BucketAllocEntry::new(1, BucketKind::Free, BUCKET_FLAG_NEEDS_DISCARD, 0);
        entry.set_kind(BucketKind::Bad);
        assert_eq!(entry.kind().unwrap(), BucketKind::Bad);
        assert_eq!(entry.flags(), BUCKET_FLAG_NEEDS_DISCARD);
        entry.set_flags(0);
        assert_eq!(entry.kind().unwrap(), BucketKind::Bad);
        assert_eq!(entry.flags(), 0);
    }

    #[test]
    fn unassigned_kind_is_a_format_error() {
        let entry = BucketAllocEntry { generation: 1, kind_flags: 0x08, live_sectors: [0; 3] };
        assert!(matches!(entry.kind(), Err(StorageError::InvalidBucketKind(8))));
    }

    #[test]
    fn bucket_kind_discriminants_pinned() {
        for raw in 0u8..=7 {
            let kind = BucketKind::from_u8(raw).unwrap();
            assert_eq!(kind as u8, raw);
        }
        assert_eq!(BucketKind::Bad as u8, 7);
        for raw in 8u8..=15 {
            assert!(BucketKind::from_u8(raw).is_err());
        }
    }

    #[test]
    fn placeholder_table_inserts_and_looks_up() {
        let mut t = BucketAllocTable::new();
        t.insert(7, BucketAllocEntry::new(5, BucketKind::Node, 0, 12));
        assert_eq!(t.len(), 1);
        let got = t.get(7).unwrap();
        assert_eq!(got.generation, 5);
        assert_eq!(got.live_sectors(), 12);
    }

    // ----- B+ tree region round-trip -----

    use crate::file_device::FileBlockDevice;
    use tempfile::TempDir;

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bucket_alloc.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    fn entry(generation: u32, kind: BucketKind, flags: u8) -> BucketAllocEntry {
        BucketAllocEntry::new(generation, kind, flags, generation & 0xff)
    }

    #[test]
    fn bucket_alloc_region_packs_keys_and_values_without_cbor() {
        // 16 sequential buckets: 1-byte keys, 8-byte fixed values, one data block.
        let (_dir, dev) = fresh_device();
        let mut t = BucketAllocTable::new();
        for i in 0u32..16 {
            t.insert(i, entry(i, BucketKind::Data, 0));
        }
        t.flush_to_region(&dev, 0).unwrap();
        let region = Region::open(&dev, 0, BtreeKind::BucketAlloc).unwrap();
        assert_eq!(region.run_count(), 1);
        assert_eq!(region.next_block(), 3, "block 0, one index block, one data block");
        let value = region.lookup(&dev, &5u32).unwrap().unwrap();
        assert_eq!(value.as_bytes(), bytemuck::bytes_of(t.get(5).unwrap()));
    }

    #[test]
    fn bucket_alloc_region_round_trip_empty() {
        let (_dir, dev) = fresh_device();
        let t = BucketAllocTable::load_from_region(&dev, 0).unwrap();
        assert!(t.is_empty());
    }

    #[test]
    fn bucket_alloc_region_round_trip_preserves_entries() {
        let (_dir, dev) = fresh_device();
        let mut t = BucketAllocTable::new();
        for i in 0u32..50 {
            t.insert(i, entry(i, BucketKind::Data, 0));
        }
        t.insert(99, entry(7, BucketKind::Journal, BUCKET_FLAG_NEEDS_DISCARD));
        t.insert(100, entry(42, BucketKind::Bad, 0));

        t.flush_to_region(&dev, 0).unwrap();
        let back = BucketAllocTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), t.len());
        for i in 0u32..50 {
            let got = back.get(i).expect("bucket missing");
            assert_eq!(got.generation, i);
            assert_eq!(got.kind().unwrap(), BucketKind::Data);
            assert_eq!(got.live_sectors(), i);
        }
        let journal = back.get(99).unwrap();
        assert_eq!(journal.kind().unwrap(), BucketKind::Journal);
        assert_eq!(journal.flags(), BUCKET_FLAG_NEEDS_DISCARD);
        assert_eq!(back.get(100).unwrap().kind().unwrap(), BucketKind::Bad);
    }

    #[test]
    fn bucket_alloc_region_round_trip_single_entry() {
        let (_dir, dev) = fresh_device();
        let mut t = BucketAllocTable::new();
        t.insert(42, entry(7, BucketKind::Node, 0));
        t.flush_to_region(&dev, 0).unwrap();
        let back = BucketAllocTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        let got = back.get(42).unwrap();
        assert_eq!(got.generation, 7);
        assert_eq!(got.kind().unwrap(), BucketKind::Node);
    }

    #[test]
    fn bucket_alloc_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = BucketAllocTable::new();
        first.insert(1, entry(1, BucketKind::Free, 0));
        first.insert(2, entry(2, BucketKind::Free, 0));
        first.flush_to_region(&dev, 0).unwrap();

        let mut second = BucketAllocTable::new();
        second.insert(99, entry(7, BucketKind::Stripe, 0));
        second.flush_to_region(&dev, 0).unwrap();

        let back = BucketAllocTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back.get(99).unwrap().generation, 7);
        assert!(back.get(1).is_none());
    }

    #[test]
    fn bucket_alloc_region_kind_mismatch_errors() {
        // Write a node with a *different* BtreeKind, then try to load it
        // back as BucketAlloc — must return RegionKindMismatch.
        let (_dir, dev) = fresh_device();
        // Borrow the freespace path to write a different kind at offset 0.
        let lru = crate::freespace::FreespaceLru::new();
        lru.flush_to_region(&dev, 0).unwrap();
        let err = BucketAllocTable::load_from_region(&dev, 0).err();
        assert!(matches!(err, Some(StorageError::RegionKindMismatch { expected: 12, actual: 13 })));
    }
}
