//! Range index — `(TagId, NormalisedKey) → RoaringBitmap` (DESIGN §5.4,
//! IMPL §9.2).
//!
//! In-memory mirror only; the on-disk B+ tree uses the standard §1.5
//! large-node shape and is out of scope for this phase. Persistence here
//! goes through CBOR.

use std::collections::BTreeMap;

use {
    mimisbrunnr_types::{TagId, Value},
    roaring::RoaringBitmap,
    serde::{Deserialize, Serialize},
};

use crate::{error::IndexError, normalised_key::NormalisedKey};

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

    /// Serialise to CBOR. TODO(rewrite-phase-N): replace with §1.5 B+ tree
    /// backing.
    pub fn serialise(&self) -> Result<Vec<u8>, IndexError> {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(self, &mut buf)
            .map_err(|e| IndexError::CborEncode(e.to_string()))?;
        Ok(buf)
    }

    /// Deserialise from CBOR.
    pub fn deserialise(bytes: &[u8]) -> Result<Self, IndexError> {
        ciborium::de::from_reader(bytes).map_err(|e| IndexError::CborDecode(e.to_string()))
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
}
