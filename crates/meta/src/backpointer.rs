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
    mimisbrunnr_storage::{BlockDevice, BtreeKind, BtreeRegion, LoadedNode, SortedRun},
    serde::{Deserialize, Serialize},
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

/// Size in bytes of [`BackpointerKey`] (8). Pinned by `const_assert_eq!`.
pub const BACKPOINTER_KEY_SIZE: usize = 8;

impl Serialize for BackpointerKey {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_bytes(bytemuck::bytes_of(self))
    }
}

impl<'de> Deserialize<'de> for BackpointerKey {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let bytes: Vec<u8> = crate::serde_pod_bytes::deserialize_bytes(de)?;
        if bytes.len() != BACKPOINTER_KEY_SIZE {
            return Err(serde::de::Error::custom(format!(
                "expected {BACKPOINTER_KEY_SIZE} bytes for BackpointerKey, got {}",
                bytes.len()
            )));
        }
        let mut buf = [0u8; BACKPOINTER_KEY_SIZE];
        buf.copy_from_slice(&bytes);
        Ok(*bytemuck::from_bytes::<Self>(&buf))
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

/// Size in bytes of [`BackpointerValue`] (24). Pinned by `const_assert_eq!`.
pub const BACKPOINTER_VALUE_SIZE: usize = 24;

impl Serialize for BackpointerValue {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_bytes(bytemuck::bytes_of(self))
    }
}

impl<'de> Deserialize<'de> for BackpointerValue {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let bytes: Vec<u8> = crate::serde_pod_bytes::deserialize_bytes(de)?;
        if bytes.len() != BACKPOINTER_VALUE_SIZE {
            return Err(serde::de::Error::custom(format!(
                "expected {BACKPOINTER_VALUE_SIZE} bytes for BackpointerValue, got {}",
                bytes.len()
            )));
        }
        let mut buf = [0u8; BACKPOINTER_VALUE_SIZE];
        buf.copy_from_slice(&bytes);
        Ok(*bytemuck::from_bytes::<Self>(&buf))
    }
}

// ---------------- BackpointerTable (in-memory placeholder) ----------------

/// In-memory mirror of the global backpointer B+ tree.
///
/// ## Persistence (R1b-7)
///
/// On disk the table occupies one 256 KiB §1.5 B+ tree region of
/// [`BtreeKind::Backpointer`]. The mirror is materialised into a single
/// CBOR-encoded sorted run via [`BtreeRegion::write_full`] keyed by
/// 8-byte [`BackpointerKey`] ascending; values are the 24-byte
/// [`BackpointerValue`] byte image.
///
/// TODO(rewrite-phase-R1d): switch to the IMPL §1.5.6 packed-key codec
/// once R1c lands the prefix-template fix and variable-value-size
/// support. With ~9 000 backpointers per 256 KiB leaf via §1.5.6 key
/// compression, that landing reduces this region's payload by an order
/// of magnitude.
#[derive(Debug, Default)]
pub struct BackpointerTable {
    entries: BTreeMap<BackpointerKey, BackpointerValue>,
}

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // consumed by engine layout once R1b lands engine-side.
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

    // ----------------------------------------------------------------
    // R1b-7: §1.5 B+ tree persistence (CBOR-encoded sorted run).
    // ----------------------------------------------------------------

    /// Build a [`LoadedNode`] containing every entry as a single CBOR
    /// sorted run, sorted by [`BackpointerKey`]. The node uses
    /// [`BtreeKind::Backpointer`] and the spec's 18-bit (256 KiB) region
    /// size.
    pub fn to_loaded_node(&self) -> LoadedNode<BackpointerKey, BackpointerValue> {
        let entries: Vec<(BackpointerKey, BackpointerValue)> =
            self.entries.iter().map(|(k, v)| (*k, *v)).collect();

        let mut node: LoadedNode<BackpointerKey, BackpointerValue> =
            LoadedNode::new(BtreeKind::Backpointer, 0, REGION_SIZE_LOG2);
        if !entries.is_empty() {
            let run = SortedRun::from_sorted(0, 0, entries);
            node.sorted_runs.push(run);
            node.header.sorted_run_count = 1;
        }
        node
    }

    /// Restore the in-memory state from a [`LoadedNode`] read via
    /// [`BtreeRegion::read`].
    pub fn from_loaded_node(node: &LoadedNode<BackpointerKey, BackpointerValue>) -> Self {
        let mut entries: BTreeMap<BackpointerKey, BackpointerValue> = BTreeMap::new();
        for (k, v) in node.merge_iter() {
            entries.insert(*k, *v);
        }
        Self { entries }
    }

    /// Write the in-memory state as a fresh 256 KiB region at byte `offset`
    /// on `device`.
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<(), MetaError> {
        let mut node = self.to_loaded_node();
        BtreeRegion::write_full::<D, BackpointerKey, BackpointerValue>(
            device, offset, &mut node,
        )
        .map_err(MetaError::from)?;
        Ok(())
    }

    /// Read the in-memory state from the 256 KiB region at byte `offset`
    /// on `device`. An all-zero region returns [`Self::default`].
    pub fn load_from_region<D: BlockDevice>(
        device: &D,
        offset: u64,
    ) -> Result<Self, MetaError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe).map_err(MetaError::from)?;
        if probe.iter().all(|&b| b == 0) {
            return Ok(Self::default());
        }
        let node = BtreeRegion::read::<D, BackpointerKey, BackpointerValue>(
            device,
            offset,
            BtreeKind::Backpointer,
        )
        .map_err(MetaError::from)?;
        Ok(Self::from_loaded_node(&node))
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

    // ----- B+ tree region round-trip (R1b-7) -----

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
        assert_eq!({ got.bucket_gen }, 700);
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
}
