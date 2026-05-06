//! Backpointer table types — IMPL §6.2.
//!
//! Reverse mapping `(disk_id, bucket_no, sector_offset) → owning key` powers
//! copygc, evacuation, scrub, resilver and cluster reconcile. The actual
//! global B+ tree (`BtreeKind::Backpointer`) is a later phase; this module
//! provides:
//!
//! - The on-disk [`BackpointerKey`] (8 B, packed) and [`BackpointerValue`]
//!   (24 B, packed) struct definitions, sized exactly per spec.
//! - The [`OwnerKind`] u8 enum with discriminants pinned per IMPL §6.2.
//! - A [`BackpointerTable`] in-memory placeholder backed by a `BTreeMap`,
//!   with a `range_in_bucket` helper that exercises the prefix-scan
//!   property the §1.5 B+ tree will eventually accelerate.

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
    std::{
        collections::{BTreeMap, btree_map},
        ops::Bound,
    },
};

use crate::error::MetaError;

// ---------------- BackpointerKey (8 B, packed) ----------------

/// Physical-location key for the backpointer table (IMPL §6.2).
///
/// Layout:
/// ```text
///  [0..2]  disk_id        u16
///  [2..6]  bucket_no      u32
///  [6..8]  sector_offset  u16   4 KiB units within the bucket
/// ```
///
/// Total = 8 bytes; packed so that the §1.5.6 key-packing codec sees a
/// stable byte order. Lexicographic ordering of the byte representation
/// matches `(disk_id, bucket_no, sector_offset)` ordering — which is what
/// the bucket-prefix range scan relies on.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct BackpointerKey {
    /// `[0..2]` disk id.
    pub disk_id: u16,
    /// `[2..6]` bucket index within the disk.
    pub bucket_no: u32,
    /// `[6..8]` 4 KiB sector index within the bucket.
    pub sector_offset: u16,
}

const_assert_eq!(core::mem::size_of::<BackpointerKey>(), 8);

impl BackpointerKey {
    /// Construct a new key.
    pub const fn new(disk_id: u16, bucket_no: u32, sector_offset: u16) -> Self {
        Self {
            disk_id,
            bucket_no,
            sector_offset,
        }
    }
}

// `#[repr(C, packed)]` disables auto-derived comparison/hash because the
// fields are unaligned. Implement them explicitly using copied locals.
impl PartialEq for BackpointerKey {
    fn eq(&self, other: &Self) -> bool {
        let (a_disk, a_bucket, a_sector) = (self.disk_id, self.bucket_no, self.sector_offset);
        let (b_disk, b_bucket, b_sector) =
            (other.disk_id, other.bucket_no, other.sector_offset);
        a_disk == b_disk && a_bucket == b_bucket && a_sector == b_sector
    }
}

impl Eq for BackpointerKey {}

impl PartialOrd for BackpointerKey {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BackpointerKey {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let (a_disk, a_bucket, a_sector) = (self.disk_id, self.bucket_no, self.sector_offset);
        let (b_disk, b_bucket, b_sector) =
            (other.disk_id, other.bucket_no, other.sector_offset);
        (a_disk, a_bucket, a_sector).cmp(&(b_disk, b_bucket, b_sector))
    }
}

impl core::hash::Hash for BackpointerKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        let (disk, bucket, sector) = (self.disk_id, self.bucket_no, self.sector_offset);
        disk.hash(state);
        bucket.hash(state);
        sector.hash(state);
    }
}

// ---------------- OwnerKind (u8) ----------------

/// Discriminator for the kind of owner a backpointer points back to (IMPL §6.2).
///
/// Discriminants are **pinned by the spec**; do not renumber. The IMPL spec
/// numbers these starting at 1 (see IMPL §6.2 enum block). The prompt's
/// "BlobExtent = 0, …" form is not authoritative — the spec wins.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OwnerKind {
    /// `owner_key` = `ObjectId` (u64) || extent_index (u64).
    BlobExtent = 1,
    /// `owner_key` = first 16 B of chunk_hash (BLAKE3 prefix).
    Chunk = 2,
    /// `owner_key` = `(BtreeKind, level, min_key prefix)`.
    BtreeNode = 3,
    /// `owner_key` = `TagId` (u32) || container_idx (u32).
    TagBitmapExtent = 4,
    /// `owner_key` = `ObjectId` (u64).
    OverflowRecord = 5,
}

impl OwnerKind {
    /// Decode from the on-disk discriminant.
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::BlobExtent),
            2 => Some(Self::Chunk),
            3 => Some(Self::BtreeNode),
            4 => Some(Self::TagBitmapExtent),
            5 => Some(Self::OverflowRecord),
            _ => None,
        }
    }
}

// ---------------- BackpointerValue (24 B, packed) ----------------

/// Value stored under a [`BackpointerKey`] (IMPL §6.2).
///
/// Layout:
/// ```text
///  [0..1]   owner_kind      u8   OwnerKind discriminant
///  [1..2]   _pad            u8
///  [2..4]   length_sectors  u16   extent length in 4 KiB sectors
///  [4..8]   bucket_gen      u32   bucket generation at insertion time
///  [8..24]  owner_key       [u8; 16]   interpreted per OwnerKind
/// ```
///
/// Equality is byte-wise over the full 16-byte `owner_key` (IMPL §6.2
/// "owner_key padding"), so writers must zero unused trailing bytes.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, PartialEq, Eq)]
pub struct BackpointerValue {
    /// `[0..1]` `OwnerKind` discriminator.
    pub owner_kind: u8,
    /// `[1..2]` padding.
    pub _pad: u8,
    /// `[2..4]` extent length in 4 KiB sectors.
    pub length_sectors: u16,
    /// `[4..8]` bucket generation at insertion (used for lazy invalidation).
    pub bucket_gen: u32,
    /// `[8..24]` 16-byte owner key (interpreted per `owner_kind`).
    pub owner_key: [u8; 16],
}

const_assert_eq!(core::mem::size_of::<BackpointerValue>(), 24);

impl BackpointerValue {
    /// Decode the `owner_kind` byte.
    pub fn owner_kind(&self) -> Result<OwnerKind, MetaError> {
        OwnerKind::from_u8(self.owner_kind).ok_or(MetaError::InvalidOwnerKind(self.owner_kind))
    }
}

// ---------------- BackpointerTable (in-memory placeholder) ----------------

/// In-memory placeholder for the global backpointer B+ tree.
///
// TODO(rewrite-phase-N): replace with the §1.5 B+ tree of large nodes
// (`BtreeKind::Backpointer`). The proper implementation packs ~9 000
// backpointers per 256 KiB leaf via §1.5.6 key compression; this map is
// purely a placeholder so callers can be written and tested before the
// allocator/journal pieces land.
#[derive(Debug, Default)]
pub struct BackpointerTable {
    entries: BTreeMap<BackpointerKey, BackpointerValue>,
}

impl BackpointerTable {
    /// Empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or overwrite) the backpointer under `key`.
    pub fn insert(&mut self, key: BackpointerKey, value: BackpointerValue) {
        self.entries.insert(key, value);
    }

    /// Remove the backpointer under `key`. Returns the removed value.
    pub fn remove(&mut self, key: &BackpointerKey) -> Option<BackpointerValue> {
        self.entries.remove(key)
    }

    /// Look up a single backpointer.
    pub fn get(&self, key: &BackpointerKey) -> Option<&BackpointerValue> {
        self.entries.get(key)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterator over all entries in `(disk_id, bucket_no, sector_offset)` order.
    pub fn iter(&self) -> btree_map::Iter<'_, BackpointerKey, BackpointerValue> {
        self.entries.iter()
    }

    /// Range scan of every backpointer in `(disk_id, bucket_no)`. The IMPL
    /// §6.2 "bucket-prefix scan" is the foundation of copygc /
    /// evacuation / scrub. The proper §1.5 B+ tree turns this into one or
    /// two contiguous leaf loads; here it's a `BTreeMap::range`.
    pub fn range_in_bucket(
        &self,
        disk_id: u16,
        bucket_no: u32,
    ) -> btree_map::Range<'_, BackpointerKey, BackpointerValue> {
        let lo = BackpointerKey::new(disk_id, bucket_no, 0);
        let hi = BackpointerKey::new(disk_id, bucket_no, u16::MAX);
        self.entries
            .range((Bound::Included(lo), Bound::Included(hi)))
    }

    /// Range scan of every backpointer on a given disk. Used by disk
    /// evacuation (IMPL §6.2 "Operations enabled" table).
    pub fn range_on_disk(
        &self,
        disk_id: u16,
    ) -> btree_map::Range<'_, BackpointerKey, BackpointerValue> {
        let lo = BackpointerKey::new(disk_id, 0, 0);
        let hi = BackpointerKey::new(disk_id, u32::MAX, u16::MAX);
        self.entries
            .range((Bound::Included(lo), Bound::Included(hi)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_size_is_8() {
        // u16 + u32 + u16 = 8 bytes packed (multiple of 8 — no extra _pad needed).
        assert_eq!(core::mem::size_of::<BackpointerKey>(), 8);
    }

    #[test]
    fn value_size_is_24() {
        // u8 + u8 + u16 + u32 + [u8;16] = 1+1+2+4+16 = 24 bytes packed.
        assert_eq!(core::mem::size_of::<BackpointerValue>(), 24);
    }

    #[test]
    fn owner_kind_discriminants_pinned() {
        assert_eq!(OwnerKind::BlobExtent as u8, 1);
        assert_eq!(OwnerKind::Chunk as u8, 2);
        assert_eq!(OwnerKind::BtreeNode as u8, 3);
        assert_eq!(OwnerKind::TagBitmapExtent as u8, 4);
        assert_eq!(OwnerKind::OverflowRecord as u8, 5);
    }

    #[test]
    fn owner_kind_round_trip() {
        for v in [1u8, 2, 3, 4, 5] {
            assert_eq!(OwnerKind::from_u8(v).unwrap() as u8, v);
        }
        assert!(OwnerKind::from_u8(0).is_none());
        assert!(OwnerKind::from_u8(6).is_none());
    }

    #[test]
    fn invalid_owner_kind_decoding_errors() {
        let v = BackpointerValue {
            owner_kind: 99,
            ..BackpointerValue::default()
        };
        assert!(matches!(
            v.owner_kind(),
            Err(MetaError::InvalidOwnerKind(99))
        ));
    }

    #[test]
    fn key_orders_by_disk_then_bucket_then_sector() {
        let a = BackpointerKey::new(1, 100, 0);
        let b = BackpointerKey::new(1, 100, 5);
        let c = BackpointerKey::new(1, 200, 0);
        let d = BackpointerKey::new(2, 0, 0);
        assert!(a < b);
        assert!(b < c);
        assert!(c < d);
    }

    fn dummy_value(kind: OwnerKind, gen_: u32) -> BackpointerValue {
        BackpointerValue {
            owner_kind: kind as u8,
            length_sectors: 1,
            bucket_gen: gen_,
            ..BackpointerValue::default()
        }
    }

    #[test]
    fn range_in_bucket_scoped_to_disk_and_bucket() {
        let mut t = BackpointerTable::new();
        // disk=1 bucket=10
        t.insert(
            BackpointerKey::new(1, 10, 0),
            dummy_value(OwnerKind::BlobExtent, 1),
        );
        t.insert(
            BackpointerKey::new(1, 10, 7),
            dummy_value(OwnerKind::BlobExtent, 1),
        );
        t.insert(
            BackpointerKey::new(1, 10, u16::MAX),
            dummy_value(OwnerKind::BlobExtent, 1),
        );
        // disk=1 bucket=11 (excluded)
        t.insert(
            BackpointerKey::new(1, 11, 0),
            dummy_value(OwnerKind::Chunk, 1),
        );
        // disk=2 bucket=10 (excluded)
        t.insert(
            BackpointerKey::new(2, 10, 0),
            dummy_value(OwnerKind::Chunk, 1),
        );

        let hits: Vec<_> = t.range_in_bucket(1, 10).map(|(k, _)| *k).collect();
        assert_eq!(hits.len(), 3);
        for k in &hits {
            let (disk, bucket) = (k.disk_id, k.bucket_no);
            assert_eq!(disk, 1);
            assert_eq!(bucket, 10);
        }
    }

    #[test]
    fn range_on_disk_excludes_other_disks() {
        let mut t = BackpointerTable::new();
        t.insert(
            BackpointerKey::new(1, 0, 0),
            dummy_value(OwnerKind::BlobExtent, 1),
        );
        t.insert(
            BackpointerKey::new(1, 5, 9),
            dummy_value(OwnerKind::Chunk, 1),
        );
        t.insert(
            BackpointerKey::new(3, 0, 0),
            dummy_value(OwnerKind::Chunk, 1),
        );
        let hits: Vec<_> = t.range_on_disk(1).map(|(k, _)| k.disk_id).collect();
        assert_eq!(hits, vec![1, 1]);
    }

    #[test]
    fn insert_remove_get_round_trip() {
        let mut t = BackpointerTable::new();
        let k = BackpointerKey::new(7, 42, 3);
        let v = dummy_value(OwnerKind::OverflowRecord, 99);
        assert!(t.is_empty());
        t.insert(k, v);
        assert_eq!(t.len(), 1);
        let got = t.get(&k).unwrap();
        assert_eq!(got.owner_kind, OwnerKind::OverflowRecord as u8);
        let removed = t.remove(&k).unwrap();
        assert_eq!(removed.owner_kind, OwnerKind::OverflowRecord as u8);
        assert!(t.is_empty());
    }

    #[test]
    fn value_round_trip_via_bytemuck() {
        let v = BackpointerValue {
            owner_kind: OwnerKind::BlobExtent as u8,
            length_sectors: 4,
            bucket_gen: 0xdead_beef,
            owner_key: [9u8; 16],
            ..BackpointerValue::default()
        };

        let bytes = bytemuck::bytes_of(&v).to_vec();
        assert_eq!(bytes.len(), 24);
        let v2: &BackpointerValue = bytemuck::from_bytes(&bytes);
        assert_eq!({ v2.length_sectors }, 4);
        assert_eq!({ v2.bucket_gen }, 0xdead_beef);
        assert_eq!(v2.owner_key, [9u8; 16]);
    }

    #[test]
    fn key_round_trip_via_bytemuck() {
        let k = BackpointerKey::new(0xabcd, 0x1234_5678, 0xaabb);
        let bytes = bytemuck::bytes_of(&k).to_vec();
        assert_eq!(bytes.len(), 8);
        let k2: &BackpointerKey = bytemuck::from_bytes(&bytes);
        assert_eq!(*k2, k);
    }
}
