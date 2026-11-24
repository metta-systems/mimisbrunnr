//! Backpointer table types — METADATA-DESIGN§2.2.
//!
//! Reverse mapping `(disk_id, bucket_no, sector_offset) → owning key` powers
//! copygc, evacuation, scrub, resilver and cluster reconcile. The actual
//! global B+ tree (`BtreeKind::Backpointer`) is a later phase; this module
//! provides:
//!
//! - The on-disk [`BackpointerKey`] (8 B) and [`BackpointerValue`]
//!   (24 B) struct definitions, sized exactly per spec.
//! - The [`OwnerKind`] u8 enum with discriminants pinned per METADATA-DESIGN§2.2.
//! - A [`BackpointerTable`] in-memory placeholder backed by a `BTreeMap`,
//!   with a `range_in_bucket` helper that exercises the prefix-scan
//!   property the BTREE-DESIGN§1 B+ tree will eventually accelerate.

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{
        BlockDevice, BtreeKind, StorageError,
        region::{FieldKind, Region, RegionKey, RegionParams, RunEntry},
    },
    static_assertions::const_assert_eq,
    std::{
        collections::{BTreeMap, btree_map},
        ops::Bound,
    },
};

use crate::error::MetaError;

// ---------------- BackpointerKey (8 B) ----------------

/// Physical-location key for the backpointer table (METADATA-DESIGN§2.2).
///
/// Layout:
/// ```text
///  [0..2]  disk_id        u16
///  [2..6]  bucket_no      u32
///  [6..8]  sector_offset  u16   4 KiB units within the bucket
/// ```
///
/// Total = 8 bytes. In a region the key is three unsigned fields
/// (BTREE-DESIGN§1.6) in `(disk_id, bucket_no, sector_offset)` order — the
/// order the bucket-prefix range scan relies on.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct BackpointerKey {
    /// `[0..2]` disk id.
    pub disk_id: u16,
    /// `[2..6]` bucket index within the disk: little-endian `u32` at a
    /// 2-aligned offset; see [`Self::bucket_no`].
    pub bucket_no: [u8; 4],
    /// `[6..8]` 4 KiB sector index within the bucket.
    pub sector_offset: u16,
}

const_assert_eq!(core::mem::size_of::<BackpointerKey>(), 8);

impl BackpointerKey {
    /// Construct a new key.
    pub const fn new(disk_id: u16, bucket_no: u32, sector_offset: u16) -> Self {
        Self {
            disk_id,
            bucket_no: bucket_no.to_le_bytes(),
            sector_offset,
        }
    }

    pub const fn bucket_no(&self) -> u32 {
        u32::from_le_bytes(self.bucket_no)
    }

    /// `(disk_id, bucket_no, sector_offset)`, the key's sort order.
    fn fields(&self) -> (u16, u32, u16) {
        (self.disk_id, self.bucket_no(), self.sector_offset)
    }
}

// Comparison and hashing go through the numeric fields: the byte array
// `bucket_no` would compare in the wrong order.
impl PartialEq for BackpointerKey {
    fn eq(&self, other: &Self) -> bool {
        self.fields() == other.fields()
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
        self.fields().cmp(&other.fields())
    }
}

impl core::hash::Hash for BackpointerKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.fields().hash(state);
    }
}

impl RegionKey for BackpointerKey {
    const FIELDS: &'static [FieldKind] = &[FieldKind::Unsigned, FieldKind::Unsigned, FieldKind::Unsigned];

    fn field_values(&self, out: &mut [u64]) {
        out[0] = self.disk_id as u64;
        out[1] = self.bucket_no() as u64;
        out[2] = self.sector_offset as u64;
    }

    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError> {
        let out_of_range = |_| StorageError::RegionCorrupt("backpointer key field out of range");
        Ok(Self::new(
            u16::try_from(fields[0]).map_err(out_of_range)?,
            u32::try_from(fields[1]).map_err(out_of_range)?,
            u16::try_from(fields[2]).map_err(out_of_range)?,
        ))
    }
}

// ---------------- OwnerKind (u8) ----------------

/// Discriminator for the kind of owner a backpointer points back to (METADATA-DESIGN§2.2).
///
/// TODO(METADATA3): the spec's `OwnerKind` is `Chunk = 0` (owner key = oid +
/// start offset), `BtreeNode = 1`, `PackedBlock = 2`, and the owner moves from
/// the value into `BackpointerKey`, so a deduplicated location has one
/// backpointer per owner (METADATA-DESIGN§2.2).
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

// ---------------- BackpointerValue (24 B) ----------------

/// Value stored under a [`BackpointerKey`] (METADATA-DESIGN§2.2).
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
/// Equality is byte-wise over the full 16-byte `owner_key` (METADATA-DESIGN§2.2
/// "owner_key padding"), so writers must zero unused trailing bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, PartialEq, Eq)]
pub struct BackpointerValue {
    /// `[0..1]` `OwnerKind` discriminator.
    pub owner_kind: u8,
    /// `[1..2]` padding.
    pub _pad: u8,
    /// `[2..4]` extent length in 4 KiB sectors.
    ///
    /// TODO(METADATA3): the spec widens this to a u24 at `[1..4]` so a 256 MiB
    /// bucket's 65 536 sectors fit.
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

/// Size in bytes of [`BackpointerValue`] (24). Pinned by `const_assert_eq!`.
pub const BACKPOINTER_VALUE_SIZE: usize = 24;

// ---------------- BackpointerTable (in-memory placeholder) ----------------

/// In-memory mirror of the global backpointer B+ tree.
///
/// ## Persistence
///
/// On disk the table occupies one 256 KiB [`Region`] of
/// [`BtreeKind::Backpointer`] keyed by [`BackpointerKey`]; values are the
/// 24-byte [`BackpointerValue`] byte image.
#[derive(Debug, Default)]
pub struct BackpointerTable {
    entries: BTreeMap<BackpointerKey, BackpointerValue>,
}

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // not yet consumed by the engine layout.
pub const BACKPOINTER_TABLE_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

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

    /// Range scan of every backpointer in `(disk_id, bucket_no)`. The
    /// METADATA-DESIGN§2.2 "bucket-prefix scan" is the foundation of copygc /
    /// evacuation / scrub. The proper BTREE-DESIGN§1 B+ tree turns this into one or
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
    /// evacuation (METADATA-DESIGN§2.2 "Operations enabled" table).
    pub fn range_on_disk(
        &self,
        disk_id: u16,
    ) -> btree_map::Range<'_, BackpointerKey, BackpointerValue> {
        let lo = BackpointerKey::new(disk_id, 0, 0);
        let hi = BackpointerKey::new(disk_id, u32::MAX, u16::MAX);
        self.entries
            .range((Bound::Included(lo), Bound::Included(hi)))
    }

    /// Write the table as the 256 KiB region at byte `offset`, keyed by
    /// [`BackpointerKey`], each value a 24-byte [`BackpointerValue`].
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<(), MetaError> {
        let mut region = Region::create_in_place(device, offset, region_params(), &[])?;
        let entries: Vec<(BackpointerKey, RunEntry<'_>)> = self
            .entries
            .iter()
            .map(|(key, value)| (*key, RunEntry::Value(bytemuck::bytes_of(value))))
            .collect();
        region.append_entries(device, 0, &entries)?;
        Ok(())
    }

    /// Read the table from the region at byte `offset`; a never-written
    /// (all-zero) region is an empty table.
    pub fn load_from_region<D: BlockDevice>(
        device: &D,
        offset: u64,
    ) -> Result<Self, MetaError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe)?;
        if probe.iter().all(|&byte| byte == 0) {
            return Ok(Self::default());
        }
        let region = Region::open(device, offset, BtreeKind::Backpointer)?;
        let mut entries = BTreeMap::new();
        for (key, value) in region.entries::<D, BackpointerKey>(device)? {
            let value = bytemuck::try_pod_read_unaligned::<BackpointerValue>(&value).map_err(|_| {
                MetaError::BufferTooSmall {
                    needed: BACKPOINTER_VALUE_SIZE,
                    got: value.len(),
                }
            })?;
            entries.insert(key, value);
        }
        Ok(Self { entries })
    }
}

fn region_params() -> RegionParams {
    RegionParams {
        kind: BtreeKind::Backpointer,
        level: 0,
        region_size_log2: REGION_SIZE_LOG2,
        node_seq: 0,
        created_lsn: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_size_is_8() {
        // u16 + [u8; 4] + u16 = 8 bytes, no padding.
        assert_eq!(core::mem::size_of::<BackpointerKey>(), 8);
    }

    #[test]
    fn value_size_is_24() {
        // u8 + u8 + u16 + u32 + [u8;16] = 1+1+2+4+16 = 24 bytes, no padding.
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
            let (disk, bucket) = (k.disk_id, k.bucket_no());
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
        let v2: &BackpointerValue = &bytemuck::pod_read_unaligned(&bytes);
        assert_eq!(v2.length_sectors, 4);
        assert_eq!(v2.bucket_gen, 0xdead_beef);
        assert_eq!(v2.owner_key, [9u8; 16]);
    }

    #[test]
    fn key_round_trip_via_bytemuck() {
        let k = BackpointerKey::new(0xabcd, 0x1234_5678, 0xaabb);
        let bytes = bytemuck::bytes_of(&k).to_vec();
        assert_eq!(bytes.len(), 8);
        let k2: &BackpointerKey = &bytemuck::pod_read_unaligned(&bytes);
        assert_eq!(*k2, k);
    }

    // ----- B+ tree region round-trip -----

    use mimisbrunnr_storage::FileBlockDevice;
    use tempfile::TempDir;

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("backpointer_table.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    fn make_bp(disk: u16, bucket: u32, sector: u16, kind: OwnerKind) -> (BackpointerKey, BackpointerValue) {
        let k = BackpointerKey::new(disk, bucket, sector);
        let mut owner_key = [0u8; 16];
        owner_key[0..2].copy_from_slice(&disk.to_le_bytes());
        owner_key[2..6].copy_from_slice(&bucket.to_le_bytes());
        let v = BackpointerValue {
            owner_kind: kind as u8,
            length_sectors: 8,
            bucket_gen: bucket,
            owner_key,
            ..BackpointerValue::default()
        };
        (k, v)
    }

    #[test]
    fn backpointer_region_round_trip_empty() {
        let (_dir, dev) = fresh_device();
        let t = BackpointerTable::load_from_region(&dev, 0).unwrap();
        assert!(t.is_empty());
    }

    #[test]
    fn backpointer_region_round_trip_preserves_entries() {
        let (_dir, dev) = fresh_device();
        let mut t = BackpointerTable::new();
        for disk in 0u16..3 {
            for bucket in 0u32..4 {
                for sector in [0u16, 1, 7] {
                    let (k, v) = make_bp(disk, bucket, sector, OwnerKind::BlobExtent);
                    t.insert(k, v);
                }
            }
        }
        // One with a different OwnerKind for kind-byte coverage.
        let (k, v) = make_bp(9, 99, 9, OwnerKind::TagBitmapExtent);
        t.insert(k, v);

        let expected_len = t.len();
        t.flush_to_region(&dev, 0).unwrap();
        let back = BackpointerTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), expected_len);

        // Spot-check via range_in_bucket — preserves the prefix-scan property.
        let scan: Vec<_> = back.range_in_bucket(0, 0).collect();
        assert_eq!(scan.len(), 3); // sectors 0, 1, 7
        let scan2: Vec<_> = back.range_in_bucket(2, 3).collect();
        assert_eq!(scan2.len(), 3);

        // Different-kind entry survives.
        let special = back
            .get(&BackpointerKey::new(9, 99, 9))
            .expect("special bp missing");
        assert_eq!(special.owner_kind, OwnerKind::TagBitmapExtent as u8);
    }

    #[test]
    fn backpointer_region_round_trip_single_entry() {
        let (_dir, dev) = fresh_device();
        let mut t = BackpointerTable::new();
        let (k, v) = make_bp(7, 700, 13, OwnerKind::Chunk);
        t.insert(k, v);
        t.flush_to_region(&dev, 0).unwrap();
        let back = BackpointerTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        let got = back.get(&k).unwrap();
        assert_eq!(got.owner_kind, OwnerKind::Chunk as u8);
        assert_eq!(got.bucket_gen, 700);
    }

    #[test]
    fn backpointer_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = BackpointerTable::new();
        let (k1, v1) = make_bp(1, 100, 0, OwnerKind::BlobExtent);
        let (k2, v2) = make_bp(1, 200, 0, OwnerKind::BlobExtent);
        first.insert(k1, v1);
        first.insert(k2, v2);
        first.flush_to_region(&dev, 0).unwrap();

        let mut second = BackpointerTable::new();
        let (k9, v9) = make_bp(99, 999, 9, OwnerKind::OverflowRecord);
        second.insert(k9, v9);
        second.flush_to_region(&dev, 0).unwrap();

        let back = BackpointerTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(
            back.get(&k9).unwrap().owner_kind,
            OwnerKind::OverflowRecord as u8
        );
        assert!(back.get(&k1).is_none());
    }

    #[test]
    fn backpointer_region_stores_fixed_values_under_three_field_keys() {
        let (_dir, dev) = fresh_device();
        let mut table = BackpointerTable::new();
        for sector in 0u16..20 {
            let (key, value) = make_bp(2, 40, sector, OwnerKind::BlobExtent);
            table.insert(key, value);
        }
        table.flush_to_region(&dev, 0).unwrap();
        let region = Region::open(&dev, 0, BtreeKind::Backpointer).unwrap();
        assert_eq!(region.run_count(), 1);
        let key = BackpointerKey::new(2, 40, 13);
        let value = region.lookup(&dev, &key).unwrap().unwrap();
        assert_eq!(value.as_bytes(), bytemuck::bytes_of(table.get(&key).unwrap()));
        assert!(region.lookup(&dev, &BackpointerKey::new(2, 41, 0)).unwrap().is_none());
    }

    #[test]
    fn backpointer_region_rejects_wrong_value_size() {
        let (_dir, dev) = fresh_device();
        let mut region = Region::create_in_place(&dev, 0, region_params(), &[]).unwrap();
        let short_value = [0u8; 10];
        region
            .append_entries(&dev, 0, &[(BackpointerKey::new(1, 2, 3), RunEntry::Value(&short_value))])
            .unwrap();
        let err = BackpointerTable::load_from_region(&dev, 0).unwrap_err();
        assert!(matches!(err, MetaError::BufferTooSmall { needed: BACKPOINTER_VALUE_SIZE, got: 10 }));
    }

    #[test]
    fn backpointer_region_load_kind_mismatch_fails() {
        let (_dir, dev) = fresh_device();
        let mut objects = crate::ObjectTable::new();
        objects.insert(crate::ObjectRecord::new(1));
        objects.flush_to_region(&dev, 0).unwrap();
        let err = BackpointerTable::load_from_region(&dev, 0).unwrap_err();
        assert!(matches!(
            err,
            MetaError::Storage(StorageError::RegionKindMismatch { expected, actual })
                if expected == BtreeKind::Backpointer as u16 && actual == BtreeKind::ObjectTable as u16
        ));
    }
}
