//! Per-disk bucket allocation table (§12.2).
//!
//! This module owns the on-disk record types, the data-type enum, the flag
//! consts, an in-memory `BTreeMap`-backed mirror, and the §1.5 B+ tree
//! persistence path that lets the table live in a real
//! [`BtreeKind::BucketAlloc`](crate::block::BtreeKind::BucketAlloc) region.
//!
//! ## Persistence (R1b-4)
//!
//! On disk the table occupies one 256 KiB §1.5 B+ tree region of
//! [`BtreeKind::BucketAlloc`]. The mirror is materialised into a single
//! CBOR-encoded sorted run via [`BtreeRegion::write_full`] keyed by
//! [`BucketAllocKey`] ascending; values are the 16-byte
//! [`BucketAllocEntry`] byte image.
//!
//! Per IMPL §12.2 each disk gets its **own** B+ tree rooted at
//! `DiskDescriptorOnDisk.buckets_root`. R1b-4 simplifies to one
//! pool-scoped tree carried in the index zone. The wire key already
//! carries `disk_id: u16` so the per-disk split (R1d) is a layout-only
//! change with no on-disk format break.
//!
//! TODO(rewrite-phase-R1d): split into per-disk metadata zones per
//! IMPL §12.2 — one bucket alloc B+ tree per disk, rooted at
//! `DiskDescriptorOnDisk.buckets_root`.
//!
//! TODO(rewrite-phase-R1d): switch to the IMPL §1.5.6 packed-key codec
//! once R1c lands the prefix-template fix and variable-value-size
//! support. Keys are dense small `u32`, ideal for packed bit-width
//! compression.

use std::collections::BTreeMap;

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
pub const BUCKET_ALLOC_REGION_SIZE: u64 = 256 * 1024;
const BUCKET_ALLOC_REGION_SIZE_LOG2: u8 = 18;

/// Flag: bucket queued for TRIM (§12.7).
pub const BUCKET_FLAG_NEEDS_DISCARD: u8 = 1 << 0;
/// Flag: bucket pinned by a live snapshot.
pub const BUCKET_FLAG_PINNED_BY_SNAPSHOT: u8 = 1 << 1;

/// Per-bucket allocation record (16 B). IMPL §12.2.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, Eq, PartialEq)]
pub struct BucketAllocEntry {
    pub generation: u32,      // [0..4]
    pub data_type: u8,        // [4..5]   BucketDataType
    pub flags: u8,            // [5..6]   BUCKET_FLAG_*
    pub dirty_sectors: u16,   // [6..8]
    pub last_modify_lsn: u64, // [8..16]
}

const_assert_eq!(core::mem::size_of::<BucketAllocEntry>(), 16);

/// Size in bytes of [`BucketAllocEntry`] (16). Used by the R1b-4
/// sorted-run length validator.
pub const BUCKET_ALLOC_ENTRY_SIZE: usize = 16;

// `BucketAllocEntry` is `#[repr(C, packed)] Pod` — round-trip the byte
// image so the CBOR wire form mirrors the on-disk POD layout exactly.
impl Serialize for BucketAllocEntry {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_bytes(bytemuck::bytes_of(self))
    }
}

impl<'de> Deserialize<'de> for BucketAllocEntry {
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
        if bytes.len() != BUCKET_ALLOC_ENTRY_SIZE {
            return Err(serde::de::Error::custom(format!(
                "expected {BUCKET_ALLOC_ENTRY_SIZE} bytes for BucketAllocEntry, got {}",
                bytes.len()
            )));
        }
        let mut buf = [0u8; BUCKET_ALLOC_ENTRY_SIZE];
        buf.copy_from_slice(&bytes);
        Ok(*bytemuck::from_bytes::<Self>(&buf))
    }
}

/// Wire key for the bucket-alloc B+ tree.
///
/// Per IMPL §12.2, the per-disk tree is keyed by `bucket_no: u32` only.
/// R1b-4 simplifies to one pool-scoped tree; `disk_id: u16` is added to
/// the key so this can split per-disk later (R1d) without an on-disk
/// format break. Lexicographic ordering of the byte image matches
/// `(disk_id, bucket_no)`.
///
/// Layout:
/// ```text
///  [0..2]  disk_id    u16
///  [2..6]  bucket_no  u32
/// ```
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct BucketAllocKey {
    /// `[0..2]` disk id.
    pub disk_id: u16,
    /// `[2..6]` bucket index within the disk.
    pub bucket_no: u32,
}

const_assert_eq!(core::mem::size_of::<BucketAllocKey>(), 6);

/// Size in bytes of [`BucketAllocKey`] (6). Pinned by `const_assert_eq!`.
pub const BUCKET_ALLOC_KEY_SIZE: usize = 6;

// `#[repr(C, packed)]` disables auto-derived comparison/hash because
// fields are unaligned. Implement explicitly using copied locals.
impl PartialEq for BucketAllocKey {
    fn eq(&self, other: &Self) -> bool {
        let (a_disk, a_bucket) = (self.disk_id, self.bucket_no);
        let (b_disk, b_bucket) = (other.disk_id, other.bucket_no);
        a_disk == b_disk && a_bucket == b_bucket
    }
}

impl Eq for BucketAllocKey {}

impl PartialOrd for BucketAllocKey {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BucketAllocKey {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let (a_disk, a_bucket) = (self.disk_id, self.bucket_no);
        let (b_disk, b_bucket) = (other.disk_id, other.bucket_no);
        (a_disk, a_bucket).cmp(&(b_disk, b_bucket))
    }
}

impl core::hash::Hash for BucketAllocKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        let (disk, bucket) = (self.disk_id, self.bucket_no);
        disk.hash(state);
        bucket.hash(state);
    }
}

impl BucketAllocKey {
    /// Construct a new key.
    pub const fn new(disk_id: u16, bucket_no: u32) -> Self {
        Self { disk_id, bucket_no }
    }
}

impl Serialize for BucketAllocKey {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_bytes(bytemuck::bytes_of(self))
    }
}

impl<'de> Deserialize<'de> for BucketAllocKey {
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
        if bytes.len() != BUCKET_ALLOC_KEY_SIZE {
            return Err(serde::de::Error::custom(format!(
                "expected {BUCKET_ALLOC_KEY_SIZE} bytes for BucketAllocKey, got {}",
                bytes.len()
            )));
        }
        let mut buf = [0u8; BUCKET_ALLOC_KEY_SIZE];
        buf.copy_from_slice(&bytes);
        Ok(*bytemuck::from_bytes::<Self>(&buf))
    }
}

/// Bucket-content classification. **Discriminants pinned by IMPL §12.2.**
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BucketDataType {
    Free = 0,
    Wal = 1,
    Index = 2,
    Metadata = 3,
    Blob = 4,
    BtreeNode = 5,
    Stripe = 6,
    NeedDiscard = 7,
    Reserved = 8,
}

impl BucketDataType {
    pub fn from_u8(value: u8) -> Result<Self, StorageError> {
        Ok(match value {
            0 => Self::Free,
            1 => Self::Wal,
            2 => Self::Index,
            3 => Self::Metadata,
            4 => Self::Blob,
            5 => Self::BtreeNode,
            6 => Self::Stripe,
            7 => Self::NeedDiscard,
            8 => Self::Reserved,
            other => return Err(StorageError::InvalidBucketDataType(other)),
        })
    }
}

/// In-memory placeholder for the per-disk bucket alloc table.
///
/// Backed by a `BTreeMap<u32, BucketAllocEntry>`. Keys are `bucket_no`.
///
/// R1b-4 keeps this in-memory state pool-scoped (single allocator, no
/// per-disk split) while the on-disk wire shape is already
/// `(disk_id, bucket_no)`-keyed for forward compatibility with R1d.
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

    // ----------------------------------------------------------------
    // R1b-4: §1.5 B+ tree persistence (CBOR-encoded sorted run).
    // ----------------------------------------------------------------

    /// Build a [`LoadedNode`] containing every entry as a single CBOR
    /// sorted run, sorted by [`BucketAllocKey`]. The node uses
    /// [`BtreeKind::BucketAlloc`] and the spec's 18-bit (256 KiB) region
    /// size.
    ///
    /// Every entry is keyed by `(disk_id, bucket_no)`. R1b-4 emits a
    /// single-disk view because the in-memory `BucketAllocTable` is
    /// pool-scoped — callers pass `disk_id = 0` until R1d splits the
    /// allocator per-disk.
    pub fn to_loaded_node(&self, disk_id: u16) -> LoadedNode<BucketAllocKey, BucketAllocEntry> {
        let entries: Vec<(BucketAllocKey, BucketAllocEntry)> = self
            .entries
            .iter()
            .map(|(bucket_no, e)| (BucketAllocKey::new(disk_id, *bucket_no), *e))
            .collect();

        let mut node: LoadedNode<BucketAllocKey, BucketAllocEntry> =
            LoadedNode::new(BtreeKind::BucketAlloc, 0, BUCKET_ALLOC_REGION_SIZE_LOG2);
        if !entries.is_empty() {
            let run = SortedRun::from_sorted(0, 0, entries);
            node.sorted_runs.push(run);
            node.header.sorted_run_count = 1;
        }
        node
    }

    /// Restore the in-memory state from a [`LoadedNode`] read via
    /// [`BtreeRegion::read`].
    ///
    /// R1b-4 ignores the wire `disk_id`: the in-memory table is
    /// pool-wide, indexed only by `bucket_no`. R1d will split per-disk
    /// and use the full key.
    pub fn from_loaded_node(node: &LoadedNode<BucketAllocKey, BucketAllocEntry>) -> Self {
        let mut entries: BTreeMap<u32, BucketAllocEntry> = BTreeMap::new();
        for (k, v) in node.merge_iter() {
            // Drop disk_id for now; R1d will key the in-memory map by
            // (disk_id, bucket_no).
            entries.insert(k.bucket_no, *v);
        }
        Self { entries }
    }

    /// Write the in-memory state as a fresh 256 KiB region at byte
    /// `offset` on `device`.
    ///
    /// `disk_id` is the wire-level disk id stamped onto every emitted
    /// key. Pool-scoped callers pass `0` until R1d.
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &mut D,
        offset: u64,
        disk_id: u16,
    ) -> Result<(), StorageError> {
        let mut node = self.to_loaded_node(disk_id);
        BtreeRegion::write_full(device, offset, &mut node)?;
        Ok(())
    }

    /// Read the in-memory state from the 256 KiB region at byte `offset`
    /// on `device`. An all-zero region returns [`Self::default`].
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
            BucketAllocKey, BucketAllocEntry
        >(device, offset)?;
        Ok(Self::from_loaded_node(&node))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_alloc_entry_is_16_bytes() {
        assert_eq!(core::mem::size_of::<BucketAllocEntry>(), 16);
    }

    #[test]
    fn bucket_alloc_key_is_6_bytes() {
        assert_eq!(core::mem::size_of::<BucketAllocKey>(), 6);
    }

    #[test]
    fn bucket_alloc_key_orders_by_disk_then_bucket() {
        let a = BucketAllocKey::new(0, 0);
        let b = BucketAllocKey::new(0, 100);
        let c = BucketAllocKey::new(1, 0);
        let d = BucketAllocKey::new(1, 50);
        assert!(a < b);
        assert!(b < c);
        assert!(c < d);
    }

    #[test]
    fn data_type_round_trips() {
        for raw in 0u8..=8 {
            let t = BucketDataType::from_u8(raw).unwrap();
            assert_eq!(t as u8, raw);
        }
        assert!(BucketDataType::from_u8(99).is_err());
    }

    #[test]
    fn placeholder_table_inserts_and_looks_up() {
        let mut t = BucketAllocTable::new();
        let e = BucketAllocEntry {
            generation: 5,
            data_type: BucketDataType::Index as u8,
            flags: BUCKET_FLAG_PINNED_BY_SNAPSHOT,
            dirty_sectors: 12,
            last_modify_lsn: 99,
        };
        t.insert(7, e);
        assert_eq!(t.len(), 1);
        let got = t.get(7).unwrap();
        let gen_ = { got.generation };
        let dirty = { got.dirty_sectors };
        assert_eq!(gen_, 5);
        assert_eq!(dirty, 12);
    }

    // ----- B+ tree region round-trip (R1b-4) -----

    use {crate::file_device::FileBlockDevice, tempfile::TempDir};

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bucket_alloc.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    fn entry(generation: u32, data_type: BucketDataType, flags: u8) -> BucketAllocEntry {
        BucketAllocEntry {
            generation,
            data_type: data_type as u8,
            flags,
            dirty_sectors: (generation & 0xff) as u16,
            last_modify_lsn: generation as u64 * 100,
        }
    }

    #[test]
    fn bucket_alloc_to_loaded_node_round_trip_preserves_entries() {
        let mut t = BucketAllocTable::new();
        for i in 0u32..16 {
            t.insert(i, entry(i, BucketDataType::Blob, 0));
        }
        let node = t.to_loaded_node(0);
        let back = BucketAllocTable::from_loaded_node(&node);
        assert_eq!(back.len(), t.len());
        for i in 0u32..16 {
            let got = back.get(i).expect("missing");
            let g = { got.generation };
            assert_eq!(g, i);
        }
    }

    #[test]
    fn bucket_alloc_to_loaded_node_round_trip_empty() {
        let t = BucketAllocTable::new();
        let node = t.to_loaded_node(0);
        let back = BucketAllocTable::from_loaded_node(&node);
        assert!(back.is_empty());
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
            t.insert(i, entry(i, BucketDataType::Blob, 0));
        }
        t.insert(99, entry(7, BucketDataType::Wal, BUCKET_FLAG_NEEDS_DISCARD));
        t.insert(
            100,
            entry(42, BucketDataType::Metadata, BUCKET_FLAG_PINNED_BY_SNAPSHOT),
        );

        t.flush_to_region(&dev, 0, 0).unwrap();
        let back = BucketAllocTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), t.len());
        for i in 0u32..50 {
            let got = back.get(i).expect("bucket missing");
            let g = { got.generation };
            let d = got.data_type;
            assert_eq!(g, i);
            assert_eq!(d, BucketDataType::Blob as u8);
        }
        let wal = back.get(99).unwrap();
        assert_eq!(wal.data_type, BucketDataType::Wal as u8);
        assert_eq!(wal.flags, BUCKET_FLAG_NEEDS_DISCARD);
    }

    #[test]
    fn bucket_alloc_region_round_trip_single_entry() {
        let (_dir, dev) = fresh_device();
        let mut t = BucketAllocTable::new();
        t.insert(42, entry(7, BucketDataType::BtreeNode, 0));
        t.flush_to_region(&dev, 0, 0).unwrap();
        let back = BucketAllocTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        let got = back.get(42).unwrap();
        let g = { got.generation };
        assert_eq!(g, 7);
        assert_eq!(got.data_type, BucketDataType::BtreeNode as u8);
    }

    #[test]
    fn bucket_alloc_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = BucketAllocTable::new();
        first.insert(1, entry(1, BucketDataType::Free, 0));
        first.insert(2, entry(2, BucketDataType::Free, 0));
        first.flush_to_region(&dev, 0, 0).unwrap();

        let mut second = BucketAllocTable::new();
        second.insert(99, entry(7, BucketDataType::Stripe, 0));
        second.flush_to_region(&dev, 0, 0).unwrap();

        let back = BucketAllocTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        let got = back.get(99).unwrap();
        let g = { got.generation };
        assert_eq!(g, 7);
        assert!(back.get(1).is_none());
    }

    #[test]
    fn bucket_alloc_region_kind_mismatch_errors() {
        // Write a node with a *different* BtreeKind, then try to load it
        // back as BucketAlloc — must return InvalidBtreeKind.
        let (_dir, dev) = fresh_device();
        // Borrow the freespace path to write a different kind at offset 0.
        let lru = crate::freespace::FreespaceLru::new();
        lru.flush_to_region(&dev, 0).unwrap();
        let err = BucketAllocTable::load_from_region(&dev, 0).err();
        assert!(matches!(err, Some(StorageError::InvalidBtreeKind(_))));
    }
}
