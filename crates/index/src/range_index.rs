//! Range index — `(TagId, NormalisedKey) → RoaringBitmap` (SPECIFICATION-DESIGN§5.4,
//! INDEX-DESIGN§4.2).
//!
//! ## Persistence
//!
//! On disk the index occupies one 256 KiB BTREE-DESIGN§1 [`Region`] of
//! [`BtreeKind::Range`], keyed by [`RangeIndexKey`] `(tag_id, norm_key,
//! snapshot)` — four fields, the 16-byte normalised key as two big-endian
//! byte fields so it orders lexicographically. Each value is the key's
//! roaring bitmap in its portable serialisation (variable length).
//!
//! TODO(INDEX12): replaced by term-dictionary scans.

use std::collections::BTreeMap;

use {
    mimisbrunnr_storage::{
        BlockDevice, BtreeKind, StorageError,
        region::{FieldKind, Region, RegionKey, RegionParams, RunEntry},
    },
    mimisbrunnr_types::{TagId, Value},
    roaring::RoaringBitmap,
    serde::{Deserialize, Serialize},
};

use crate::{
    error::IndexError,
    normalised_key::{NORMALISED_KEY_LEN, NormalisedKey},
};

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // not yet consumed by the engine layout.
pub const RANGE_INDEX_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

/// In-memory range index. The `BTreeMap` ordering matches the on-disk
/// `(tag_id, NormalisedKey)` key order so prefix scans are straightforward.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RangeIndex {
    map: BTreeMap<(TagId, NormalisedKey), RoaringBitmapSerde>,
}

impl RangeIndex {
    /// New, empty range index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `oid_local` for `(tag, value)`.
    pub fn insert(&mut self, tag: TagId, value: &Value, oid_local: u32) {
        let k = NormalisedKey::from_value(value);
        self.map.entry((tag, k)).or_default().0.insert(oid_local);
    }

    /// Remove `oid_local` from `(tag, value)`. Returns `true` if it was
    /// present.
    pub fn remove(&mut self, tag: TagId, value: &Value, oid_local: u32) -> bool {
        let k = NormalisedKey::from_value(value);
        let mut empty = false;
        let removed = if let Some(wrap) = self.map.get_mut(&(tag, k)) {
            let was = wrap.0.remove(oid_local);
            empty = wrap.0.is_empty();
            was
        } else {
            false
        };
        if empty {
            self.map.remove(&(tag, k));
        }
        removed
    }

    /// Equality lookup.
    pub fn lookup(&self, tag: TagId, value: &Value) -> RoaringBitmap {
        let k = NormalisedKey::from_value(value);
        self.map
            .get(&(tag, k))
            .map(|wrap| wrap.0.clone())
            .unwrap_or_default()
    }

    /// Half-open range scan: `low ≤ key < high`.
    pub fn range_scan(&self, tag: TagId, low: &Value, high: &Value) -> RoaringBitmap {
        let lk = NormalisedKey::from_value(low);
        let hk = NormalisedKey::from_value(high);
        let mut acc = RoaringBitmap::new();
        for (_, wrap) in self.map.range((tag, lk)..(tag, hk)) {
            acc |= &wrap.0;
        }
        acc
    }

    /// Number of `(tag, key)` entries.
    pub fn entry_count(&self) -> usize {
        self.map.len()
    }

    /// Serialise to CBOR.
    ///
    /// `RangeIndex` derives `Serialize`/`Deserialize` directly. The
    /// `RoaringBitmap` framing lives in [`RoaringBitmapSerde`]'s custom
    /// serde impls. This helper is kept for symmetry with the other indices
    /// and to map ciborium errors to [`IndexError`]; callers may equally
    /// reach for `ciborium::ser::into_writer` directly. On-disk persistence
    /// goes through the BTREE-DESIGN§1 region below, not this helper.
    pub fn serialise(&self) -> Result<Vec<u8>, IndexError> {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(self, &mut buf)
            .map_err(|e| IndexError::CborEncode(e.to_string()))?;
        Ok(buf)
    }

    /// Deserialise from CBOR. See the doc on [`Self::serialise`] for why
    /// this helper is retained.
    pub fn deserialise(bytes: &[u8]) -> Result<Self, IndexError> {
        ciborium::de::from_reader(bytes).map_err(|e| IndexError::CborDecode(e.to_string()))
    }

    // ----------------------------------------------------------------
    // BTREE-DESIGN§1 region persistence.
    // ----------------------------------------------------------------

    /// Write the in-memory state as the 256 KiB region at byte `offset` on
    /// `device`, replacing whatever was there. Every key carries
    /// `snapshot = 0` until SNAPSHOT2 (SNAPSHOTS-DESIGN§1.2).
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<(), IndexError> {
        // BTreeMap iteration is sorted by (TagId, NormalisedKey), which with
        // a uniform zero snapshot is RangeIndexKey order.
        let mut serialised: Vec<(RangeIndexKey, Vec<u8>)> = Vec::with_capacity(self.map.len());
        for ((tag, norm_key), wrapped) in &self.map {
            let mut bytes = Vec::with_capacity(wrapped.0.serialized_size());
            wrapped
                .0
                .serialize_into(&mut bytes)
                .map_err(|error| IndexError::Roaring(error.to_string()))?;
            let key = RangeIndexKey {
                tag_id: tag.raw(),
                norm_key: *norm_key,
                snapshot: 0,
            };
            serialised.push((key, bytes));
        }
        let entries: Vec<(RangeIndexKey, RunEntry<'_>)> = serialised
            .iter()
            .map(|(key, bytes)| (*key, RunEntry::Value(bytes)))
            .collect();
        let mut region = Region::create_in_place(device, offset, region_params(), &[])?;
        region.append_entries(device, 0, &entries)?;
        Ok(())
    }

    /// Read the in-memory state from the 256 KiB region at byte `offset` on
    /// `device`. A never-written (all-zero) region is an empty index.
    pub fn load_from_region<D: BlockDevice>(
        device: &D,
        offset: u64,
    ) -> Result<Self, IndexError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe)?;
        if probe.iter().all(|&byte| byte == 0) {
            return Ok(Self::default());
        }
        let region = Region::open(device, offset, BtreeKind::Range)?;
        let mut map: BTreeMap<(TagId, NormalisedKey), RoaringBitmapSerde> = BTreeMap::new();
        for (key, value) in region.entries::<D, RangeIndexKey>(device)? {
            // Snapshot != 0 won't appear until SNAPSHOT2 lands snapshot-aware reads.
            let bitmap = RoaringBitmap::deserialize_from(value.as_slice())
                .map_err(|error| IndexError::Roaring(error.to_string()))?;
            map.insert((TagId::new(key.tag_id), key.norm_key), RoaringBitmapSerde(bitmap));
        }
        Ok(Self { map })
    }
}

fn region_params() -> RegionParams {
    RegionParams {
        kind: BtreeKind::Range,
        level: 0,
        region_size_log2: REGION_SIZE_LOG2,
        node_seq: 0,
        created_lsn: 0,
    }
}

// ---------- RangeIndexKey (region key) ----------

/// Region key for the range index: `(tag_id, norm_key, snapshot)` per
/// INDEX-DESIGN§4.2 + SNAPSHOTS-DESIGN§1.2.
///
/// `norm_key` is the 16-byte order-preserving encoding of the original
/// [`Value`]. Snapshots are deferred to SNAPSHOT2 so every key written today
/// carries `snapshot = 0`. Sort order is `(tag_id, norm_key, snapshot)`
/// — the derived `Ord` matches because field order is the same as
/// declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RangeIndexKey {
    pub tag_id: u32,
    pub norm_key: NormalisedKey,
    pub snapshot: u32,
}

impl RegionKey for RangeIndexKey {
    const FIELDS: &'static [FieldKind] = &[
        FieldKind::Unsigned,
        FieldKind::BigEndianBytes,
        FieldKind::BigEndianBytes,
        FieldKind::Unsigned,
    ];

    fn field_values(&self, out: &mut [u64]) {
        let (high_half, low_half) = self.norm_key.0.split_at(8);
        out[0] = self.tag_id as u64;
        out[1] = u64::from_be_bytes(high_half.try_into().expect("8-byte half"));
        out[2] = u64::from_be_bytes(low_half.try_into().expect("8-byte half"));
        out[3] = self.snapshot as u64;
    }

    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError> {
        let out_of_range = |_| StorageError::RegionCorrupt("range key field out of range");
        let mut norm_key = [0u8; NORMALISED_KEY_LEN];
        norm_key[..8].copy_from_slice(&fields[1].to_be_bytes());
        norm_key[8..].copy_from_slice(&fields[2].to_be_bytes());
        Ok(Self {
            tag_id: u32::try_from(fields[0]).map_err(out_of_range)?,
            norm_key: NormalisedKey(norm_key),
            snapshot: u32::try_from(fields[3]).map_err(out_of_range)?,
        })
    }
}

// ---------- RoaringBitmapSerde (serde wrapper) ----------

#[derive(Debug, Clone, Default)]
struct RoaringBitmapSerde(RoaringBitmap);

impl Serialize for RoaringBitmapSerde {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut bytes = Vec::with_capacity(self.0.serialized_size());
        self.0
            .serialize_into(&mut bytes)
            .map_err(serde::ser::Error::custom)?;
        bytes.serialize(ser)
    }
}

impl<'de> Deserialize<'de> for RoaringBitmapSerde {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let bytes: Vec<u8> = Vec::deserialize(de)?;
        let bm = RoaringBitmap::deserialize_from(bytes.as_slice())
            .map_err(serde::de::Error::custom)?;
        Ok(Self(bm))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: u32) -> TagId {
        TagId::new(id)
    }

    #[test]
    fn equality_lookup() {
        let mut idx = RangeIndex::new();
        idx.insert(t(1), &Value::Int(2024), 100);
        idx.insert(t(1), &Value::Int(2024), 101);
        idx.insert(t(1), &Value::Int(2023), 200);
        let r = idx.lookup(t(1), &Value::Int(2024));
        assert_eq!(r.len(), 2);
        assert!(r.contains(100));
        assert!(r.contains(101));
    }

    #[test]
    fn range_scan_int() {
        let mut idx = RangeIndex::new();
        for year in 2020..2025i64 {
            idx.insert(t(1), &Value::Int(year), year as u32);
        }
        let scan = idx.range_scan(t(1), &Value::Int(2021), &Value::Int(2024));
        // half-open: 2021, 2022, 2023
        assert_eq!(scan.len(), 3);
        assert!(scan.contains(2021));
        assert!(scan.contains(2022));
        assert!(scan.contains(2023));
        assert!(!scan.contains(2024));
    }

    #[test]
    fn cbor_round_trip() {
        let mut idx = RangeIndex::new();
        idx.insert(t(1), &Value::Int(42), 1);
        idx.insert(t(2), &Value::Text("foo".into()), 2);
        let bytes = idx.serialise().unwrap();
        let back = RangeIndex::deserialise(&bytes).unwrap();
        assert_eq!(back.entry_count(), 2);
        assert_eq!(back.lookup(t(1), &Value::Int(42)).len(), 1);
    }

    #[test]
    fn remove_and_empty_cleanup() {
        let mut idx = RangeIndex::new();
        idx.insert(t(1), &Value::Int(1), 10);
        assert!(idx.remove(t(1), &Value::Int(1), 10));
        assert_eq!(idx.entry_count(), 0);
    }

    // ----- B+ tree region round-trip -----

    use mimisbrunnr_storage::FileBlockDevice;
    use tempfile::TempDir;

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("range_index.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    #[test]
    fn range_region_round_trip_empty_returns_default() {
        let (_dir, dev) = fresh_device();
        let idx = RangeIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(idx.entry_count(), 0);
    }

    #[test]
    fn range_region_round_trip_preserves_entries() {
        let (_dir, dev) = fresh_device();
        let mut idx = RangeIndex::new();
        for year in 2020..2025i64 {
            idx.insert(t(1), &Value::Int(year), year as u32);
            idx.insert(t(1), &Value::Int(year), (year + 1000) as u32);
        }
        idx.insert(t(2), &Value::Text("alpha".into()), 1);
        idx.insert(t(2), &Value::Text("beta".into()), 2);
        idx.insert(t(2), &Value::Text("alpha".into()), 3);

        idx.flush_to_region(&dev, 0).unwrap();
        let back = RangeIndex::load_from_region(&dev, 0).unwrap();

        assert_eq!(back.entry_count(), idx.entry_count());
        // Equality on a known key.
        let r = back.lookup(t(1), &Value::Int(2024));
        assert_eq!(r.len(), 2);
        assert!(r.contains(2024));
        assert!(r.contains(3024));
        // Half-open scan reproduces the pre-flush behaviour.
        let scan = back.range_scan(t(1), &Value::Int(2021), &Value::Int(2024));
        assert_eq!(scan.len(), 6); // 2021,2022,2023 × 2 oid_locals each
        // Text-keyed entry survives.
        let r2 = back.lookup(t(2), &Value::Text("alpha".into()));
        assert_eq!(r2.len(), 2);
    }

    #[test]
    fn range_region_round_trip_single_entry() {
        let (_dir, dev) = fresh_device();
        let mut idx = RangeIndex::new();
        idx.insert(t(7), &Value::Int(42), 700);
        idx.flush_to_region(&dev, 0).unwrap();
        let back = RangeIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.entry_count(), 1);
        let r = back.lookup(t(7), &Value::Int(42));
        assert_eq!(r.len(), 1);
        assert!(r.contains(700));
    }

    #[test]
    fn range_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = RangeIndex::new();
        first.insert(t(1), &Value::Int(1), 10);
        first.insert(t(1), &Value::Int(2), 20);
        first.flush_to_region(&dev, 0).unwrap();

        let mut second = RangeIndex::new();
        second.insert(t(99), &Value::Int(999), 9000);
        second.flush_to_region(&dev, 0).unwrap();

        let back = RangeIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.entry_count(), 1);
        let r = back.lookup(t(99), &Value::Int(999));
        assert_eq!(r.len(), 1);
        assert!(r.contains(9000));
        assert_eq!(back.lookup(t(1), &Value::Int(1)).len(), 0);
    }

    #[test]
    fn range_region_round_trip_flushed_empty_index() {
        // A written-but-empty region (header, no runs) loads as empty.
        let (_dir, dev) = fresh_device();
        RangeIndex::new().flush_to_region(&dev, 0).unwrap();
        let region = Region::open(&dev, 0, BtreeKind::Range).unwrap();
        assert_eq!(region.run_count(), 0);
        let back = RangeIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.entry_count(), 0);
    }

    #[test]
    fn range_region_keys_carry_zero_snapshot() {
        let (_dir, dev) = fresh_device();
        let mut idx = RangeIndex::new();
        idx.insert(t(7), &Value::Int(42), 1);
        idx.flush_to_region(&dev, 0).unwrap();
        let region = Region::open(&dev, 0, BtreeKind::Range).unwrap();
        let entries = region.entries::<_, RangeIndexKey>(&dev).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0.snapshot, 0);
        assert_eq!(entries[0].0.tag_id, 7);
        assert_eq!(entries[0].0.norm_key, NormalisedKey::from_value(&Value::Int(42)));
        let bitmap = RoaringBitmap::deserialize_from(entries[0].1.as_slice()).unwrap();
        assert!(bitmap.contains(1));
    }

    #[test]
    fn range_key_field_order_matches_ord() {
        // Keys that differ only in the second half of the normalised key
        // still order the same way through their fields.
        let low = RangeIndexKey {
            tag_id: 1,
            norm_key: NormalisedKey::from_value(&Value::Text("abcdefghijk".into())),
            snapshot: 0,
        };
        let high = RangeIndexKey {
            tag_id: 1,
            norm_key: NormalisedKey::from_value(&Value::Text("abcdefghijl".into())),
            snapshot: 0,
        };
        assert!(low < high);
        let mut low_fields = [0u64; 4];
        let mut high_fields = [0u64; 4];
        low.field_values(&mut low_fields);
        high.field_values(&mut high_fields);
        assert!(low_fields < high_fields);
        assert_eq!(RangeIndexKey::from_field_values(&low_fields).unwrap(), low);
        assert_eq!(RangeIndexKey::from_field_values(&high_fields).unwrap(), high);
    }

    #[test]
    fn range_region_round_trip_50_entries() {
        // Loaded-node round-trip with ~50 entries of mixed shape.
        let (_dir, dev) = fresh_device();
        let mut idx = RangeIndex::new();
        for i in 0u32..50 {
            for member in 0u32..3 {
                idx.insert(t(i / 5), &Value::Int(i as i64 * 13 + 1), i * 10 + member);
            }
        }
        let n_before = idx.entry_count();
        assert_eq!(n_before, 50); // 50 distinct (tag, value) keys
        idx.flush_to_region(&dev, 0).unwrap();
        let back = RangeIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.entry_count(), n_before);
        for i in 0u32..50 {
            let bm = back.lookup(t(i / 5), &Value::Int(i as i64 * 13 + 1));
            assert_eq!(bm.len(), 3);
        }
    }

    #[test]
    fn range_region_kind_mismatch_detected() {
        let (_dir, dev) = fresh_device();
        let mut idx = RangeIndex::new();
        idx.insert(t(1), &Value::Int(42), 1);
        idx.flush_to_region(&dev, 0).unwrap();
        let err = Region::open(&dev, 0, BtreeKind::Forward).err();
        assert!(matches!(
            err,
            Some(StorageError::RegionKindMismatch { expected: 4, actual: 0x8002 })
        ));
        // And the loader rejects a region of another kind.
        mimisbrunnr_storage::FreespaceLru::new().flush_to_region(&dev, 0).unwrap();
        let err = RangeIndex::load_from_region(&dev, 0).err();
        assert!(matches!(
            err,
            Some(IndexError::Storage(StorageError::RegionKindMismatch { expected: 0x8002, actual: 13 }))
        ));
    }

    #[test]
    fn range_region_round_trip_large_bitmap_value() {
        // A bitmap whose serialisation exceeds one 4 KiB block goes to value
        // blocks and still round-trips.
        let (_dir, dev) = fresh_device();
        let mut idx = RangeIndex::new();
        for member in 0u32..5_000 {
            idx.insert(t(3), &Value::Int(1), member * 3);
        }
        idx.insert(t(3), &Value::Int(2), 7);
        idx.flush_to_region(&dev, 0).unwrap();
        let back = RangeIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.entry_count(), 2);
        let bitmap = back.lookup(t(3), &Value::Int(1));
        assert_eq!(bitmap.len(), 5_000);
        assert!(bitmap.contains(4_999 * 3));
        assert!(back.lookup(t(3), &Value::Int(2)).contains(7));
    }
}
