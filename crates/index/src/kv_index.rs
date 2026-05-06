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
    mimisbrunnr_storage::{BlockDevice, BtreeKind, BtreeRegion, LoadedNode, SortedRun},
    mimisbrunnr_types::{TagId, Value, value_hash},
    roaring::RoaringBitmap,
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::error::IndexError;

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // consumed by engine layout once R1b lands engine-side.
pub const KV_INDEX_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

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

    /// Serialise to CBOR.
    ///
    /// `KvIndex` derives `Serialize`/`Deserialize` directly. The
    /// `RoaringBitmap` framing lives in [`RoaringBitmapSerde`]'s custom serde
    /// impls. This helper is kept for symmetry with the other indices and to
    /// map ciborium errors to [`IndexError`]; callers may equally reach for
    /// `ciborium::ser::into_writer` directly.
    ///
    /// TODO(rewrite-phase-N): replace with the extendible-hash on-disk
    /// backing.
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
    // R1b-1: §1.5 B+ tree persistence (CBOR-encoded sorted run).
    // ----------------------------------------------------------------
    //
    // Variable-shape values (roaring bitmaps, serialised to bytes) preclude
    // the §1.5.6 packed-key codec: it requires every entry's value to share
    // the same byte length. Until R1a-pack-2 grows variable-size value
    // support, KvIndex flushes through the CBOR run codec.

    /// Build a [`LoadedNode`] containing every entry as a single CBOR sorted
    /// run, sorted by `(tag_id, value_hash)`. Each value is the per-key
    /// [`RoaringBitmap`] serialised via `RoaringBitmap::serialize_into`.
    ///
    /// The node uses [`BtreeKind::KvDirectory`] (the spec doesn't yet
    /// allocate a dedicated `BtreeKind::KvIndex` — see TODO below) and the
    /// spec's 18-bit (256 KiB) region size.
    /// TODO(rewrite-phase-R1c): once `BtreeKind::KvIndex` exists in the
    /// storage spec, switch to it.
    pub fn to_loaded_node(&self) -> LoadedNode<KvIndexKey, KvIndexValue> {
        let mut entries: Vec<(KvIndexKey, KvIndexValue)> = self
            .entries
            .iter()
            .map(|(k, bm)| {
                let mut bytes = Vec::with_capacity(bm.0.serialized_size());
                // serialize_into errors only on I/O; Vec write is infallible.
                bm.0.serialize_into(&mut bytes)
                    .expect("RoaringBitmap::serialize_into into Vec must not fail");
                (
                    KvIndexKey {
                        tag_id: k.0.raw(),
                        value_hash: k.1,
                    },
                    KvIndexValue(bytes),
                )
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let mut node: LoadedNode<KvIndexKey, KvIndexValue> =
            LoadedNode::new(BtreeKind::KvDirectory, 0, REGION_SIZE_LOG2);
        if !entries.is_empty() {
            let run = SortedRun::from_sorted(0, 0, entries);
            node.sorted_runs.push(run);
            node.header.sorted_run_count = 1;
        }
        node
    }

    /// Restore the in-memory state from a [`LoadedNode`] read via
    /// [`BtreeRegion::read`]. Re-deserialises each per-entry roaring bitmap
    /// from its byte image.
    pub fn from_loaded_node(
        node: &LoadedNode<KvIndexKey, KvIndexValue>,
    ) -> Result<Self, IndexError> {
        let mut entries: HashMap<(TagId, u64), RoaringBitmapSerde> = HashMap::new();
        for (k, v) in node.merge_iter() {
            let bm = RoaringBitmap::deserialize_from(v.0.as_slice())
                .map_err(|e| IndexError::Roaring(e.to_string()))?;
            entries.insert((TagId::new(k.tag_id), k.value_hash), RoaringBitmapSerde(bm));
        }
        Ok(Self {
            entries,
            secret: [0u8; 16],
        })
    }

    /// Write the in-memory state as a fresh 256 KiB region at byte `offset`
    /// on `device`. Replaces the region wholesale via
    /// [`BtreeRegion::write_full`].
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<(), IndexError> {
        let mut node = self.to_loaded_node();
        BtreeRegion::write_full::<D, KvIndexKey, KvIndexValue>(device, offset, &mut node)?;
        Ok(())
    }

    /// Read the in-memory state from the 256 KiB region at byte `offset` on
    /// `device`. An all-zero region is treated as "empty index" and returns
    /// [`Self::default`].
    pub fn load_from_region<D: BlockDevice>(
        device: &D,
        offset: u64,
    ) -> Result<Self, IndexError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe)?;
        if probe.iter().all(|&b| b == 0) {
            return Ok(Self::default());
        }
        let node = BtreeRegion::read::<D, KvIndexKey, KvIndexValue>(
            device,
            offset,
            BtreeKind::KvDirectory,
        )?;
        Self::from_loaded_node(&node)
    }
}

// ---------- KvIndexKey / KvIndexValue (B+ tree wire types) ----------

/// B+ tree key for the KV index: `(tag_id, value_hash)` per IMPL §9.1.
///
/// Order is `tag_id` then `value_hash`, lexicographically — matches the
/// derived `Ord` impl below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KvIndexKey {
    pub tag_id: u32,
    pub value_hash: u64,
}

/// B+ tree value for the KV index: the roaring bitmap of object-locals,
/// serialised via `RoaringBitmap::serialize_into`. Wrapping in a newtype
/// gives us `Serialize` / `Deserialize` via serde's blanket
/// `Vec<u8>` impls without conflicting with the
/// [`RoaringBitmapSerde`] proxy used by the CBOR-blob persistence path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KvIndexValue(pub Vec<u8>);

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

    // ----- B+ tree region round-trip (R1b-1) -----

    use mimisbrunnr_storage::FileBlockDevice;
    use tempfile::TempDir;

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("kv_index.bin");
        // 1 MiB is plenty for one 256 KiB region.
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    #[test]
    fn empty_loaded_node_round_trip() {
        let idx = KvIndex::new();
        let node = idx.to_loaded_node();
        assert_eq!(node.sorted_runs.len(), 0);
        let back = KvIndex::from_loaded_node(&node).unwrap();
        assert_eq!(back.entry_count(), 0);
    }

    #[test]
    fn loaded_node_round_trip_preserves_entries() {
        let mut idx = KvIndex::new();
        idx.insert(t(1), &Value::Int(42), 100);
        idx.insert(t(1), &Value::Int(42), 101);
        idx.insert(t(1), &Value::Int(7), 200);
        idx.insert(t(2), &Value::Text("foo".into()), 300);
        let node = idx.to_loaded_node();
        assert_eq!(node.sorted_runs.len(), 1);
        let back = KvIndex::from_loaded_node(&node).unwrap();
        assert_eq!(back.entry_count(), 3);
        let bm = back.lookup(t(1), &Value::Int(42));
        assert!(bm.contains(100));
        assert!(bm.contains(101));
        assert_eq!(bm.len(), 2);
        assert!(back.lookup(t(2), &Value::Text("foo".into())).contains(300));
    }

    #[test]
    fn region_round_trip_empty_returns_default() {
        let (_dir, dev) = fresh_device();
        let idx = KvIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(idx.entry_count(), 0);
    }

    #[test]
    fn region_round_trip_50_entries_with_100_oid_bitmaps() {
        let (_dir, dev) = fresh_device();
        let mut idx = KvIndex::new();
        // 50 distinct (tag, value) entries; each carrying 100 random-ish u32s.
        let oid_for = |i: u32, j: u32| i.wrapping_mul(31).wrapping_add(j).wrapping_add(1);
        for i in 0u32..50 {
            let v = Value::Int(i as i64 * 13 + 1);
            for j in 0u32..100 {
                idx.insert(t(i), &v, oid_for(i, j));
            }
        }
        assert_eq!(idx.entry_count(), 50);
        idx.flush_to_region(&dev, 0).unwrap();
        let back = KvIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.entry_count(), 50);
        for i in 0u32..50 {
            let v = Value::Int(i as i64 * 13 + 1);
            let bm = back.lookup(t(i), &v);
            assert_eq!(bm.len(), 100, "entry {i} should still have 100 ids");
            for j in 0u32..100 {
                assert!(bm.contains(oid_for(i, j)));
            }
        }
    }

    #[test]
    fn region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = KvIndex::new();
        first.insert(t(1), &Value::Int(1), 11);
        first.insert(t(1), &Value::Int(2), 22);
        first.flush_to_region(&dev, 0).unwrap();

        let mut second = KvIndex::new();
        second.insert(t(9), &Value::Int(99), 999);
        second.flush_to_region(&dev, 0).unwrap();

        let back = KvIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.entry_count(), 1);
        assert!(back.lookup(t(9), &Value::Int(99)).contains(999));
        assert!(back.lookup(t(1), &Value::Int(1)).is_empty());
    }
}
