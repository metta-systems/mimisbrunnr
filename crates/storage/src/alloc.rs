//! Per-disk bucket allocation table (§12.2 — *placeholder shape*).
//!
//! The full §1.5 B+ tree implementation is deferred to a later phase. This
//! module provides the on-disk record types, the data-type enum, the flag
//! consts, and an in-memory `BTreeMap`-backed placeholder that satisfies the
//! Phase 2+ allocator's needs.
//
// TODO(rewrite-phase-N): replace the in-memory `BTreeMap` placeholder with
// the §1.5 B+ tree-backed allocator once the index/meta crates land.

use std::collections::BTreeMap;

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
};

use crate::error::StorageError;

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
}
