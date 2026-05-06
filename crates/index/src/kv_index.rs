//! KV equality index — `(TagId, value_hash) → RoaringBitmap` (DESIGN §5.3,
//! IMPL §9.1).
//!
//! On disk: extendible hash directory + buckets framed inside 4 KiB
//! `BlockKind::KvHashDirectory` / `KvHashBucket` blocks. This crate owns the
//! per-block **header** layouts after the standard 32 B `BlockHeader`; the
//! envelope itself lives in `mimisbrunnr-storage`.
//!
//! In memory: [`KvIndex`] = `HashMap<(TagId, u64), RoaringBitmap>` per IMPL
//! §13.

use std::collections::HashMap;

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_types::{TagId, Value, value_hash},
    roaring::RoaringBitmap,
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::error::IndexError;

// ---------- KvHashDirectoryHeader ----------

/// Size in bytes of [`KvHashDirectoryHeader`] (8 B). Sits at offset 32 inside
/// a 4 KiB `KvHashDirectory` block (IMPL §9.1):
///
/// ```text
/// [0..1]  global_depth   u8
/// [1..4]  _pad0          [u8; 3]
/// [4..8]  bucket_count   u32   (= 1 << global_depth)
/// ```
pub const KV_HASH_DIRECTORY_HEADER_SIZE: usize = 8;

/// Header following the standard `BlockHeader` inside a 4 KiB
/// `BlockKind::KvHashDirectory` block. IMPL §9.1.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct KvHashDirectoryHeader {
    pub global_depth: u8, // [0..1]
    pub _pad0: [u8; 3],   // [1..4]
    pub bucket_count: u32, // [4..8]   = 1 << global_depth
}

const_assert_eq!(
    core::mem::size_of::<KvHashDirectoryHeader>(),
    KV_HASH_DIRECTORY_HEADER_SIZE
);
// computed: 1 (global_depth) + 3 (_pad0) + 4 (bucket_count) = 8

// ---------- KvHashBucketHeader ----------

/// Size in bytes of [`KvHashBucketHeader`] (4 B). Sits at offset 32 inside a
/// 4 KiB `KvHashBucket` block (IMPL §9.1):
///
/// ```text
/// [0..1] local_depth   u8
/// [1..3] entry_count   u16
/// [3..4] _pad          u8
/// ```
pub const KV_HASH_BUCKET_HEADER_SIZE: usize = 4;

/// Header following the standard `BlockHeader` inside a 4 KiB
/// `BlockKind::KvHashBucket` block. IMPL §9.1.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct KvHashBucketHeader {
    pub local_depth: u8, // [0..1]
    pub entry_count: u16, // [1..3]
    pub _pad: u8,        // [3..4]
}

const_assert_eq!(
    core::mem::size_of::<KvHashBucketHeader>(),
    KV_HASH_BUCKET_HEADER_SIZE
);
// computed: 1 (local_depth) + 2 (entry_count) + 1 (_pad) = 4

// ---------- KvIndex (in-memory mirror) ----------

/// In-memory KV equality index (IMPL §13).
///
/// The map key is `(TagId, value_hash)` — the full `Value` is hashed via
/// [`mimisbrunnr_types::value_hash`] using a per-pool secret. Two `Value`s
/// representing the same logical content (e.g. two `Value::Int(42)` clones)
/// land in the same bucket regardless of `Value` form.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KvIndex {
    /// `(tag, value_hash) → bitmap-of-object-locals`.
    entries: HashMap<(TagId, u64), RoaringBitmapSerde>,
    /// 16-byte secret used to hash values. Defaults to all-zero; the engine
    /// is expected to plumb the real per-pool secret.
    /// TODO(rewrite-phase-N): plumb the per-pool secret from `Superblock`.
    #[serde(default = "default_secret")]
    secret: [u8; 16],
}

fn default_secret() -> [u8; 16] {
    [0u8; 16]
}

impl KvIndex {
    /// New, empty KV index using an all-zero secret.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            secret: [0u8; 16],
        }
    }

    /// Override the per-pool secret used to compute value hashes.
    pub fn with_secret(secret: [u8; 16]) -> Self {
        Self {
            entries: HashMap::new(),
            secret,
        }
    }

    fn hash(&self, value: &Value) -> u64 {
        value_hash(value, &self.secret)
    }

    /// Insert `oid_local` (the bottom 32 bits of an `ObjectId`) into the
    /// bitmap for `(tag, value)`.
    pub fn insert(&mut self, tag: TagId, value: &Value, oid_local: u32) {
        let h = self.hash(value);
        self.entries
            .entry((tag, h))
            .or_default()
            .0
            .insert(oid_local);
    }

    /// Remove `oid_local` from the bitmap for `(tag, value)`. Returns `true`
    /// if it was present.
    pub fn remove(&mut self, tag: TagId, value: &Value, oid_local: u32) -> bool {
        let h = self.hash(value);
        let mut empty = false;
        let removed = if let Some(bm) = self.entries.get_mut(&(tag, h)) {
            let was = bm.0.remove(oid_local);
            empty = bm.0.is_empty();
            was
        } else {
            false
        };
        if empty {
            self.entries.remove(&(tag, h));
        }
        removed
    }

    /// Lookup the bitmap for `(tag, value)`. Returns an empty bitmap when
    /// absent; this avoids forcing callers to handle the missing case.
    pub fn lookup(&self, tag: TagId, value: &Value) -> RoaringBitmap {
        let h = self.hash(value);
        self.entries
            .get(&(tag, h))
            .map(|wrap| wrap.0.clone())
            .unwrap_or_default()
    }

    /// All distinct `value_hash`es present for `tag` (faceted enumeration).
    pub fn value_hashes_for(&self, tag: TagId) -> Vec<u64> {
        self.entries
            .keys()
            .filter(|(t, _)| *t == tag)
            .map(|(_, h)| *h)
            .collect()
    }

    /// Number of (tag, value_hash) entries.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Serialise to CBOR. TODO(rewrite-phase-N): replace with the
    /// extendible-hash on-disk backing.
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
    fn directory_header_size_is_8() {
        // computed: 1 + 3 + 4 = 8
        assert_eq!(core::mem::size_of::<KvHashDirectoryHeader>(), 8);
    }

    #[test]
    fn bucket_header_size_is_4() {
        // computed: 1 + 2 + 1 = 4
        assert_eq!(core::mem::size_of::<KvHashBucketHeader>(), 4);
    }

    #[test]
    fn lookup_same_value_form_independent() {
        let mut idx = KvIndex::new();
        let v1 = Value::Int(42);
        let v2 = Value::Int(42); // distinct object, same content
        idx.insert(t(1), &v1, 100);
        idx.insert(t(1), &v1, 200);
        let r1 = idx.lookup(t(1), &v1);
        let r2 = idx.lookup(t(1), &v2);
        assert_eq!(r1, r2);
        assert_eq!(r1.len(), 2);
    }

    #[test]
    fn lookup_distinguishes_int_and_text_42() {
        let mut idx = KvIndex::new();
        idx.insert(t(1), &Value::Int(42), 100);
        idx.insert(t(1), &Value::Text("42".into()), 200);
        let int_hits = idx.lookup(t(1), &Value::Int(42));
        let text_hits = idx.lookup(t(1), &Value::Text("42".into()));
        assert_eq!(int_hits.len(), 1);
        assert_eq!(text_hits.len(), 1);
        assert!(int_hits.contains(100));
        assert!(text_hits.contains(200));
    }

    #[test]
    fn remove_and_empty_cleanup() {
        let mut idx = KvIndex::new();
        idx.insert(t(1), &Value::Int(7), 1);
        assert_eq!(idx.entry_count(), 1);
        assert!(idx.remove(t(1), &Value::Int(7), 1));
        assert_eq!(idx.entry_count(), 0);
    }

    #[test]
    fn lookup_missing_returns_empty() {
        let idx = KvIndex::new();
        assert!(idx.lookup(t(99), &Value::Int(0)).is_empty());
    }

    #[test]
    fn cbor_round_trip() {
        let mut idx = KvIndex::with_secret([7u8; 16]);
        idx.insert(t(1), &Value::Int(42), 100);
        idx.insert(t(2), &Value::Text("foo".into()), 200);
        let bytes = idx.serialise().unwrap();
        let back = KvIndex::deserialise(&bytes).unwrap();
        assert_eq!(back.entry_count(), 2);
        assert!(back.lookup(t(1), &Value::Int(42)).contains(100));
        assert!(back.lookup(t(2), &Value::Text("foo".into())).contains(200));
    }
}
