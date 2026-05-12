//! Range index — `(TagId, NormalisedKey) → RoaringBitmap` (DESIGN §5.4,
//! IMPL §9.2).
//!
//! ## Persistence (R1b-4)
//!
//! On disk the index occupies one 256 KiB §1.5 B+ tree region of
//! [`BtreeKind::Range`]. The in-memory mirror is materialised into a
//! single CBOR-encoded sorted run via [`BtreeRegion::write_full`]; reload
//! goes through [`BtreeRegion::read`]. Keys are `(u32 tag_id,
//! NormalisedKey)` pairs sorted ascending; values are the per-key
//! [`RoaringBitmapSerde`] (variable-length serialised bitmap).
//!
//! Variable-shape values preclude the §1.5.6 packed-key codec: it
//! requires every entry's value to share the same byte length. Until
//! R1a-pack-2 grows variable-size value support, RangeIndex flushes
//! through the CBOR run codec.
//!
//! TODO(rewrite-phase-R1d): once R1c lands variable-value-size support,
//! switch the *key* to the packed codec — the `(tag_id, NormalisedKey)`
//! tuple is fixed-shape (4 + 16 = 20 B) and a packed key buys O(20%)
//! payload savings.

use std::collections::BTreeMap;

use {
    mimisbrunnr_storage::{BlockDevice, BtreeKind, BtreeRegion, LoadedNode, SortedRun},
    mimisbrunnr_types::{TagId, Value},
    roaring::RoaringBitmap,
    serde::{Deserialize, Serialize},
};

use crate::{error::IndexError, normalised_key::NormalisedKey};

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // consumed by engine layout once R1b lands engine-side.
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
    /// reach for `ciborium::ser::into_writer` directly.
    ///
    /// TODO(rewrite-phase-N): replace with §1.5 B+ tree backing.
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
    // R1b-2: §1.5 B+ tree persistence (CBOR-encoded sorted run).
    // ----------------------------------------------------------------

    /// Build a [`LoadedNode`] containing every entry as a single CBOR
    /// sorted run, sorted by `(tag_id, norm_key, snapshot)`. The node uses
    /// [`BtreeKind::Range`] and the spec's 18-bit (256 KiB) region size.
    ///
    /// Each value is the per-key [`RoaringBitmap`] serialised via
    /// `RoaringBitmap::serialize_into` and wrapped in [`RangeIndexValue`].
    ///
    /// Per IMPL §11.2 the on-disk key carries a `snapshot: u32` suffix;
    /// snapshots are deferred to R6 so every key written today carries
    /// `snapshot = 0`. The field is on-disk now so R6 won't need a layout
    /// break.
    pub fn to_loaded_node(&self) -> LoadedNode<RangeIndexKey, RangeIndexValue> {
        // BTreeMap iteration is already sorted by (TagId, NormalisedKey),
        // which matches RangeIndexKey's derived (tag_id, norm_key, snapshot)
        // ordering when snapshot is uniformly 0.
        let entries: Vec<(RangeIndexKey, RangeIndexValue)> = self
            .map
            .iter()
            .map(|((tag, norm), wrap)| {
                let mut bytes = Vec::with_capacity(wrap.0.serialized_size());
                wrap.0
                    .serialize_into(&mut bytes)
                    .expect("RoaringBitmap::serialize_into into Vec must not fail");
                (
                    RangeIndexKey {
                        tag_id: tag.raw(),
                        norm_key: *norm,
                        snapshot: 0,
                    },
                    RangeIndexValue(bytes),
                )
            })
            .collect();

        let mut node: LoadedNode<RangeIndexKey, RangeIndexValue> =
            LoadedNode::new(BtreeKind::Range, 0, REGION_SIZE_LOG2);
        if !entries.is_empty() {
            let run = SortedRun::from_sorted(0, 0, entries);
            node.sorted_runs.push(run);
            node.header.sorted_run_count = 1;
        }
        node
    }

    /// Restore the in-memory state from a [`LoadedNode`] read via
    /// [`BtreeRegion::read`]. Re-deserialises each per-entry roaring
    /// bitmap from its byte image.
    pub fn from_loaded_node(
        node: &LoadedNode<RangeIndexKey, RangeIndexValue>,
    ) -> Result<Self, IndexError> {
        let mut map: BTreeMap<(TagId, NormalisedKey), RoaringBitmapSerde> = BTreeMap::new();
        for (k, v) in node.merge_iter() {
            // Snapshot != 0 won't appear until R6 lands snapshot-aware reads.
            let bm = RoaringBitmap::deserialize_from(v.0.as_slice())
                .map_err(|e| IndexError::Roaring(e.to_string()))?;
            map.insert((TagId::new(k.tag_id), k.norm_key), RoaringBitmapSerde(bm));
        }
        Ok(Self { map })
    }

    /// Write the in-memory state as a fresh 256 KiB region at byte `offset`
    /// on `device`. Replaces the region wholesale via
    /// [`BtreeRegion::write_full`].
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &mut D,
        offset: u64,
    ) -> Result<(), IndexError> {
        let mut node = self.to_loaded_node();
        BtreeRegion::write_full::<RangeIndexKey, RangeIndexValue>(device, offset, &mut node)?;
        Ok(())
    }

    /// Read the in-memory state from the 256 KiB region at byte `offset` on
    /// `device`. An all-zero region is treated as "empty index" and returns
    /// [`Self::default`].
    pub fn load_from_region<D: BlockDevice>(
        device: &mut D,
        offset: u64,
    ) -> Result<Self, IndexError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe)?;
        if probe.iter().all(|&b| b == 0) {
            return Ok(Self::default());
        }
        let node =
            BtreeRegion::read_as_loaded_node::<RangeIndexKey, RangeIndexValue>(device, offset)?;
        Self::from_loaded_node(&node)
    }
}

// ---------- RangeIndexKey / RangeIndexValue (B+ tree wire types) ----------

/// B+ tree key for the range index: `(tag_id, norm_key, snapshot)` per
/// IMPL §9.2 + §11.2.
///
/// `norm_key` is the 16-byte order-preserving encoding of the original
/// [`Value`]. Snapshots are deferred to R6 so every key written today
/// carries `snapshot = 0`. Sort order is `(tag_id, norm_key, snapshot)`
/// — the derived `Ord` matches because field order is the same as
/// declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RangeIndexKey {
    pub tag_id: u32,
    pub norm_key: NormalisedKey,
    pub snapshot: u32,
}

/// B+ tree value for the range index: the roaring bitmap of object-locals,
/// serialised via `RoaringBitmap::serialize_into`. Wrapping in a newtype
/// gives us `Serialize`/`Deserialize` via serde's blanket `Vec<u8>` impl
/// without conflicting with the [`RoaringBitmapSerde`] proxy used by the
/// CBOR-blob persistence path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeIndexValue(pub Vec<u8>);

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
        let bm =
            RoaringBitmap::deserialize_from(bytes.as_slice()).map_err(serde::de::Error::custom)?;
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

    // ----- B+ tree region round-trip (R1b-4) -----

    use {mimisbrunnr_storage::FileBlockDevice, tempfile::TempDir};

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
    fn range_loaded_node_round_trip_empty() {
        let idx = RangeIndex::new();
        let node = idx.to_loaded_node();
        assert_eq!(node.sorted_runs.len(), 0);
        let back = RangeIndex::from_loaded_node(&node).unwrap();
        assert_eq!(back.entry_count(), 0);
    }

    #[test]
    fn range_loaded_node_keys_carry_zero_snapshot() {
        let mut idx = RangeIndex::new();
        idx.insert(t(7), &Value::Int(42), 1);
        let node = idx.to_loaded_node();
        assert_eq!(node.sorted_runs[0].entries[0].0.snapshot, 0);
        assert_eq!(node.sorted_runs[0].entries[0].0.tag_id, 7);
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
        let res =
            BtreeRegion::read::<_, RangeIndexKey, RangeIndexValue>(&dev, 0, BtreeKind::Forward);
        assert!(res.is_err());
    }
}
