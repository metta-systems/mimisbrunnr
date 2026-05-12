//! Freespace LRU mirror (§12.4).
//!
//! Per IMPL §12.4 each disk has its own §1.5 B+ tree rooted at
//! `DiskDescriptorOnDisk.freespace_root`, keyed by
//! `(fragmentation_band: u8, bucket_no: u32)`. The allocator uses band
//! 0 for the foreground fast path, band 255 for the most-fragmented end,
//! and copygc walks the high bands to reclaim space.
//!
//! ## Persistence (R1b-4)
//!
//! On disk the table occupies one 256 KiB §1.5 B+ tree region of
//! [`BtreeKind::FreespaceLru`]. The mirror is materialised into a single
//! CBOR-encoded sorted run via [`BtreeRegion::write_full`] keyed by
//! [`FreespaceLruKey`] ascending. Values are [`Empty`] — IMPL §12.4
//! describes a key-only B+ tree (the "value" is implicit in the key).
//!
//! R1b-4 simplifies to one pool-scoped tree. The wire key already
//! carries `disk_id: u16` so the per-disk split (R1d) is a layout-only
//! change with no on-disk format break.
//!
//! TODO(rewrite-phase-R1d): split into per-disk metadata zones per
//! IMPL §12.4 — one freespace LRU per disk, rooted at
//! `DiskDescriptorOnDisk.freespace_root`.
//!
//! TODO(rewrite-phase-Theme-H): live-allocator integration. R1b-4
//! ships only the persistence skeleton — the foreground allocator's
//! "scan band 0" fast path, the lazy-reband-on-8-sector-boundary
//! producer, and the copygc consumer all live in Theme H.

use std::{collections::BTreeMap, ops::Bound};

use {
    bytemuck::{Pod, Zeroable},
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::{
    block::BtreeKind,
    block_device::BlockDevice,
    btree::{BtreeRegion, LoadedNode, SortedRun},
    error::StorageError,
};

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // consumed by engine layout once R1b lands engine-side.
pub const FREESPACE_LRU_REGION_SIZE: u64 = 256 * 1024;
const FREESPACE_LRU_REGION_SIZE_LOG2: u8 = 18;

/// Wire key for the freespace LRU B+ tree.
///
/// Per IMPL §12.4 the per-disk tree is keyed by
/// `(fragmentation_band: u8, bucket_no: u32)`. R1b-4 simplifies to one
/// pool-scoped tree; `disk_id: u16` is added between the two for
/// forward compat with R1d. Lexicographic ordering of the byte image
/// matches `(band, disk_id, bucket_no)` — band first so the allocator
/// fast path (`band == 0`) is a contiguous prefix scan.
///
/// Layout:
/// ```text
///  [0..1]  fragmentation_band  u8
///  [1..3]  disk_id             u16
///  [3..7]  bucket_no           u32
/// ```
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct FreespaceLruKey {
    /// `[0..1]` `0` = empty, `255` = nearly full, `1..=254` = in
    /// between (formula in IMPL §12.4).
    pub fragmentation_band: u8,
    /// `[1..3]` disk id.
    pub disk_id: u16,
    /// `[3..7]` bucket number within the disk.
    pub bucket_no: u32,
}

const_assert_eq!(core::mem::size_of::<FreespaceLruKey>(), 7);

/// Size in bytes of [`FreespaceLruKey`] (7). Pinned by `const_assert_eq!`.
pub const FREESPACE_LRU_KEY_SIZE: usize = 7;

// `#[repr(C, packed)]` disables auto-derived comparison/hash because
// the `u16` and `u32` fields are unaligned.
impl PartialEq for FreespaceLruKey {
    fn eq(&self, other: &Self) -> bool {
        let (a_band, a_disk, a_bucket) = (self.fragmentation_band, self.disk_id, self.bucket_no);
        let (b_band, b_disk, b_bucket) = (other.fragmentation_band, other.disk_id, other.bucket_no);
        a_band == b_band && a_disk == b_disk && a_bucket == b_bucket
    }
}

impl Eq for FreespaceLruKey {}

impl PartialOrd for FreespaceLruKey {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FreespaceLruKey {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let (a_band, a_disk, a_bucket) = (self.fragmentation_band, self.disk_id, self.bucket_no);
        let (b_band, b_disk, b_bucket) = (other.fragmentation_band, other.disk_id, other.bucket_no);
        (a_band, a_disk, a_bucket).cmp(&(b_band, b_disk, b_bucket))
    }
}

impl core::hash::Hash for FreespaceLruKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        let (band, disk, bucket) = (self.fragmentation_band, self.disk_id, self.bucket_no);
        band.hash(state);
        disk.hash(state);
        bucket.hash(state);
    }
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

impl Serialize for FreespaceLruKey {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_bytes(bytemuck::bytes_of(self))
    }
}

impl<'de> Deserialize<'de> for FreespaceLruKey {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        use serde::de::{Error, SeqAccess, Visitor};
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                f.write_str("byte string")
            }
            fn visit_bytes<E: Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                Ok(v.to_vec())
            }
            fn visit_byte_buf<E: Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
                Ok(v)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(b) = seq.next_element::<u8>()? {
                    out.push(b);
                }
                Ok(out)
            }
        }
        let bytes: Vec<u8> = de.deserialize_bytes(V)?;
        if bytes.len() != FREESPACE_LRU_KEY_SIZE {
            return Err(serde::de::Error::custom(format!(
                "expected {FREESPACE_LRU_KEY_SIZE} bytes for FreespaceLruKey, got {}",
                bytes.len()
            )));
        }
        let mut buf = [0u8; FREESPACE_LRU_KEY_SIZE];
        buf.copy_from_slice(&bytes);
        Ok(*bytemuck::from_bytes::<Self>(&buf))
    }
}

/// Empty wire value (zero on-disk bytes).
///
/// IMPL §12.4 describes the freespace LRU as a key-only B+ tree. To
/// fit the [`LoadedNode<K, V>`] generic shape, we round-trip a
/// zero-byte placeholder via CBOR (`null` in CBOR = 1 byte). Once R1c
/// teaches the §1.5.6 packed-key codec to handle zero-width values,
/// this becomes a true zero-payload encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Empty;

impl Serialize for Empty {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_unit()
    }
}

impl<'de> Deserialize<'de> for Empty {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Empty;
            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                f.write_str("unit")
            }
            fn visit_unit<E>(self) -> Result<Empty, E> {
                Ok(Empty)
            }
            fn visit_none<E>(self) -> Result<Empty, E> {
                Ok(Empty)
            }
        }
        de.deserialize_unit(V)
    }
}

/// In-memory mirror of the §12.4 freespace LRU.
///
/// Backed by a `BTreeMap<FreespaceLruKey, ()>`. Lookups are point
/// (`contains`) or banded prefix scans (`iter_band`). R1b-4 only ships
/// the persistence skeleton; live allocator integration (foreground
/// "scan band 0", lazy reband, copygc producer) is Theme H.
#[derive(Clone, Debug, Default)]
pub struct FreespaceLru {
    entries: BTreeMap<FreespaceLruKey, ()>,
}

impl FreespaceLru {
    /// Empty freespace LRU.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `(band, disk_id, bucket_no)`. Idempotent — the second
    /// call is a no-op.
    pub fn insert(&mut self, fragmentation_band: u8, disk_id: u16, bucket_no: u32) {
        self.entries.insert(
            FreespaceLruKey::new(fragmentation_band, disk_id, bucket_no),
            (),
        );
    }

    /// Remove `(band, disk_id, bucket_no)`. Returns `true` iff the
    /// entry was present.
    pub fn remove(&mut self, fragmentation_band: u8, disk_id: u16, bucket_no: u32) -> bool {
        self.entries
            .remove(&FreespaceLruKey::new(
                fragmentation_band,
                disk_id,
                bucket_no,
            ))
            .is_some()
    }

    /// `true` iff `(band, disk_id, bucket_no)` is present.
    pub fn contains(&self, fragmentation_band: u8, disk_id: u16, bucket_no: u32) -> bool {
        self.entries.contains_key(&FreespaceLruKey::new(
            fragmentation_band,
            disk_id,
            bucket_no,
        ))
    }

    /// Iterate over every key with the given fragmentation band, in
    /// ascending `(disk_id, bucket_no)` order. The allocator fast path
    /// uses this with `band == 0`; copygc uses it with high bands.
    pub fn iter_band(&self, fragmentation_band: u8) -> impl Iterator<Item = &FreespaceLruKey> {
        let lo = FreespaceLruKey::new(fragmentation_band, 0, 0);
        let hi = FreespaceLruKey::new(fragmentation_band, u16::MAX, u32::MAX);
        self.entries
            .range((Bound::Included(lo), Bound::Included(hi)))
            .map(|(k, _)| k)
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
        self.entries.keys()
    }

    // ----------------------------------------------------------------
    // R1b-4: §1.5 B+ tree persistence (CBOR-encoded sorted run).
    // ----------------------------------------------------------------

    /// Build a [`LoadedNode`] containing every entry as a single CBOR
    /// sorted run, sorted by [`FreespaceLruKey`]. The node uses
    /// [`BtreeKind::FreespaceLru`] and the spec's 18-bit (256 KiB)
    /// region size.
    pub fn to_loaded_node(&self) -> LoadedNode<FreespaceLruKey, Empty> {
        let entries: Vec<(FreespaceLruKey, Empty)> =
            self.entries.keys().map(|k| (*k, Empty)).collect();

        let mut node: LoadedNode<FreespaceLruKey, Empty> =
            LoadedNode::new(BtreeKind::FreespaceLru, 0, FREESPACE_LRU_REGION_SIZE_LOG2);
        if !entries.is_empty() {
            let run = SortedRun::from_sorted(0, 0, entries);
            node.sorted_runs.push(run);
            node.header.sorted_run_count = 1;
        }
        node
    }

    /// Restore the in-memory state from a [`LoadedNode`] read via
    /// [`BtreeRegion::read`].
    pub fn from_loaded_node(node: &LoadedNode<FreespaceLruKey, Empty>) -> Self {
        let mut entries: BTreeMap<FreespaceLruKey, ()> = BTreeMap::new();
        for (k, _) in node.merge_iter() {
            entries.insert(*k, ());
        }
        Self { entries }
    }

    /// Write the in-memory state as a fresh 256 KiB region at byte
    /// `offset` on `device`.
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &mut D,
        offset: u64,
    ) -> Result<(), StorageError> {
        let mut node = self.to_loaded_node();
        BtreeRegion::write_full(device, offset, &mut node)?;
        Ok(())
    }

    /// Read the in-memory state from the 256 KiB region at byte
    /// `offset` on `device`. An all-zero region returns
    /// [`Self::default`].
    pub fn load_from_region<D: BlockDevice>(
        device: &mut D,
        offset: u64,
    ) -> Result<Self, StorageError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe)?;
        if probe.iter().all(|&b| b == 0) {
            return Ok(Self::default());
        }
        let node = BtreeRegion::read_as_loaded_node::<
            FreespaceLruKey, Empty
        >(device, offset)?;
        Ok(Self::from_loaded_node(&node))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use {crate::file_device::FileBlockDevice, tempfile::TempDir};

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("freespace_lru.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    #[test]
    fn freespace_lru_key_is_7_bytes() {
        assert_eq!(core::mem::size_of::<FreespaceLruKey>(), 7);
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
        for k in &band0 {
            let band = k.fragmentation_band;
            assert_eq!(band, 0);
        }

        let band100: Vec<_> = lru.iter_band(100).collect();
        assert_eq!(band100.len(), 1);
        let band1 = lru.iter_band(1).count();
        assert_eq!(band1, 0);
    }

    #[test]
    fn freespace_lru_to_loaded_node_round_trip_preserves_entries() {
        let mut lru = FreespaceLru::new();
        for band in [0u8, 100, 255] {
            for bucket in 0u32..5 {
                lru.insert(band, 0, bucket);
            }
        }
        let node = lru.to_loaded_node();
        let back = FreespaceLru::from_loaded_node(&node);
        assert_eq!(back.len(), lru.len());
        for band in [0u8, 100, 255] {
            for bucket in 0u32..5 {
                assert!(back.contains(band, 0, bucket));
            }
        }
    }

    #[test]
    fn freespace_lru_to_loaded_node_round_trip_empty() {
        let lru = FreespaceLru::new();
        let node = lru.to_loaded_node();
        let back = FreespaceLru::from_loaded_node(&node);
        assert!(back.is_empty());
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
        for bucket in 0u32..20 {
            assert!(back.contains(0, 0, bucket));
        }
        for bucket in 100u32..110 {
            assert!(back.contains(100, 0, bucket));
        }
        for bucket in 200u32..205 {
            assert!(back.contains(255, 0, bucket));
        }
        assert!(back.contains(50, 7, 999));

        // Banded scan still works after round-trip.
        let band0: Vec<_> = back.iter_band(0).collect();
        assert_eq!(band0.len(), 20);
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
        // it as FreespaceLru — must surface InvalidBtreeKind.
        let (_dir, dev) = fresh_device();
        let alloc = crate::alloc::BucketAllocTable::new();
        // Emit a non-empty alloc table so the region isn't all-zero
        // (the all-zero shortcut would otherwise hide the kind check).
        let mut alloc = alloc;
        alloc.insert(
            0,
            crate::alloc::BucketAllocEntry {
                generation: 1,
                ..Default::default()
            },
        );
        alloc.flush_to_region(&dev, 0, 0).unwrap();
        let err = FreespaceLru::load_from_region(&dev, 0).err();
        assert!(matches!(err, Some(StorageError::InvalidBtreeKind(_))));
    }
}
