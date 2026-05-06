//! Per-disk bucket allocation table (§12.2).
//!
//! This module owns the on-disk record types, the data-type enum, the flag
//! consts, an in-memory `BTreeMap`-backed mirror, and the §1.5 B+ tree
//! persistence path that lets the table live in a real
//! [`BtreeKind::BucketAlloc`](crate::block::BtreeKind::BucketAlloc) region.
//!
//! ## Persistence (R1b-12)
//!
//! On disk the table occupies one 256 KiB §1.5 B+ tree region of
//! [`BtreeKind::BucketAlloc`]. The mirror is materialised into a single
//! CBOR-encoded sorted run via [`BtreeRegion::write_full`] keyed by
//! `bucket_no: u32` ascending; values are the 16-byte
//! [`BucketAllocEntry`] byte image.
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

/// Size in bytes of [`BucketAllocEntry`] (16). Used by the R1b-12
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
    // R1b-12: §1.5 B+ tree persistence (CBOR-encoded sorted run).
    // ----------------------------------------------------------------

    /// Build a [`LoadedNode`] containing every entry as a single CBOR
    /// sorted run, sorted by `bucket_no`. The node uses
    /// [`BtreeKind::BucketAlloc`] and the spec's 18-bit (256 KiB) region
    /// size.
    pub fn to_loaded_node(&self) -> LoadedNode<u32, BucketAllocEntry> {
        let entries: Vec<(u32, BucketAllocEntry)> =
            self.entries.iter().map(|(k, v)| (*k, *v)).collect();

        let mut node: LoadedNode<u32, BucketAllocEntry> = LoadedNode::new(
            BtreeKind::BucketAlloc,
            0,
            BUCKET_ALLOC_REGION_SIZE_LOG2,
        );
        if !entries.is_empty() {
            let run = SortedRun::from_sorted(0, 0, entries);
            node.sorted_runs.push(run);
            node.header.sorted_run_count = 1;
        }
        node
    }

    /// Restore the in-memory state from a [`LoadedNode`] read via
    /// [`BtreeRegion::read`].
    pub fn from_loaded_node(node: &LoadedNode<u32, BucketAllocEntry>) -> Self {
        let mut entries: BTreeMap<u32, BucketAllocEntry> = BTreeMap::new();
        for (k, v) in node.merge_iter() {
            entries.insert(*k, *v);
        }
        Self { entries }
    }

    /// Write the in-memory state as a fresh 256 KiB region at byte
    /// `offset` on `device`.
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<(), StorageError> {
        let mut node = self.to_loaded_node();
        BtreeRegion::write_full::<D, u32, BucketAllocEntry>(device, offset, &mut node)?;
        Ok(())
    }

    /// Read the in-memory state from the 256 KiB region at byte `offset`
    /// on `device`. An all-zero region returns [`Self::default`].
    pub fn load_from_region<D: BlockDevice>(
        device: &D,
        offset: u64,
    ) -> Result<Self, StorageError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe)?;
        if probe.iter().all(|&b| b == 0) {
            return Ok(Self::default());
        }
        let node = BtreeRegion::read::<D, u32, BucketAllocEntry>(
            device,
            offset,
            BtreeKind::BucketAlloc,
        )?;
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

    // ----- B+ tree region round-trip (R1b-12) -----

    use crate::file_device::FileBlockDevice;
    use tempfile::TempDir;

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
    fn bucket_alloc_region_round_trip_empty() {
        let (_dir, dev) = fresh_device();
        let t = BucketAllocTable::load_from_region(&dev, 0).unwrap();
        assert!(t.is_empty());
    }

    #[test]
    fn bucket_alloc_region_round_trip_preserves_entries() {
        let (_dir, dev) = fresh_device();
        let mut t = BucketAllocTable::new();
        for i in 0u32..16 {
            t.insert(i, entry(i, BucketDataType::Blob, 0));
        }
        t.insert(99, entry(7, BucketDataType::Wal, BUCKET_FLAG_NEEDS_DISCARD));
        t.insert(
            100,
            entry(
                42,
                BucketDataType::Metadata,
                BUCKET_FLAG_PINNED_BY_SNAPSHOT,
            ),
        );

        t.flush_to_region(&dev, 0).unwrap();
        let back = BucketAllocTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), t.len());
        for i in 0u32..16 {
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
        t.flush_to_region(&dev, 0).unwrap();
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
        first.flush_to_region(&dev, 0).unwrap();

        let mut second = BucketAllocTable::new();
        second.insert(99, entry(7, BucketDataType::Stripe, 0));
        second.flush_to_region(&dev, 0).unwrap();

        let back = BucketAllocTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        let got = back.get(99).unwrap();
        let g = { got.generation };
        assert_eq!(g, 7);
        assert!(back.get(1).is_none());
    }
}
