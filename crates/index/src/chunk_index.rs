//! Chunk index — content-addressed dedup directory (INDEX-DESIGN§4.3).
//!
//! On-disk: a B+ tree of large nodes keyed by 32-byte BLAKE3 chunk hash; the
//! leaf entry [`ChunkIndexLeafEntry`] is 56 B per INDEX-DESIGN§4.3 lines 1946–1951
//! (32 B hash + 4 B `ref_count` + 4 B `length` + 16 B `BlobRef`). With the
//! hash carried as the BTREE-DESIGN§1.6 packed key, the on-disk value tail is the
//! 24-byte [`ChunkIndexValue`] = `(ref_count, length, blob_ref)`.
//!
//! In-memory mirror: [`ChunkIndex`] = `HashMap<[u8;32], ChunkIndexValue>`.
//!
//! TODO(BLOB3): the value becomes a 16-byte `ChunkHolder { oid, end_offset }`
//! naming one content-map entry that holds the chunk (INDEX-DESIGN§4.3);
//! `ref_count` and the `BlobRef` go. Writers copy the holder's replicas;
//! readers never consult the index; who still uses a location is tracked by
//! one backpointer per owner (METADATA-DESIGN§2.2).
//!
//! ## Persistence
//!
//! On disk the index occupies one 256 KiB BTREE-DESIGN§1 [`Region`] of
//! [`BtreeKind::ChunkIndex`]. Keys are [`ChunkHashKey`]s — four big-endian
//! byte fields covering the 32-byte hash (BTREE-DESIGN§1.6) — and each value
//! is the 24-byte [`ChunkIndexValue`] byte image.

use std::collections::HashMap;

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{
        BlobRef, BlockDevice, BtreeKind, StorageError,
        region::{FieldKind, Region, RegionKey, RegionParams, RunEntry},
    },
    static_assertions::const_assert_eq,
};

use crate::error::IndexError;

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // not yet consumed by the engine layout.
pub const CHUNK_INDEX_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

// ---------- ChunkIndexLeafEntry (full record, mirror only) ----------

/// Size in bytes of [`ChunkIndexLeafEntry`] (56). INDEX-DESIGN§4.3.
pub const CHUNK_INDEX_LEAF_ENTRY_SIZE: usize = 56;

/// On-disk leaf entry layout described by INDEX-DESIGN§4.3. The full record is
/// 56 B; the codec splits it into the 32 B `chunk_hash` (the BTREE-DESIGN§1.6
/// packed key) and the 24 B [`ChunkIndexValue`] tail. This struct exists
/// for analyze-style tooling that wants to render the full record.
///
/// ```text
/// [0..32]  chunk_hash  [u8; 32]   (BLAKE3 of plaintext)
/// [32..36] ref_count   u32        (number of ChunkLists referencing this chunk)
/// [36..40] length      u32        (plaintext byte count)
/// [40..56] blob        BlobRef    (16 B; physical extent in the blob zone)
/// ```
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct ChunkIndexLeafEntry {
    pub chunk_hash: [u8; 32], // [0..32]
    pub ref_count: u32,       // [32..36]
    pub length: u32,          // [36..40]
    pub blob: BlobRef,        // [40..56]
}

const_assert_eq!(
    core::mem::size_of::<ChunkIndexLeafEntry>(),
    CHUNK_INDEX_LEAF_ENTRY_SIZE
);
// 32 (chunk_hash) + 4 (ref_count) + 4 (length) + 16 (BlobRef) = 56

// ---------- ChunkHashKey (region key) ----------

/// 32-byte BLAKE3 chunk hash. Used as the [`ChunkIndex`] map key and as the
/// region key (four big-endian byte fields, so field order matches the
/// natural hash byte order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkHashKey(pub [u8; 32]);

impl ChunkHashKey {
    /// Borrow the raw bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for ChunkHashKey {
    fn from(b: [u8; 32]) -> Self {
        Self(b)
    }
}

impl From<ChunkHashKey> for [u8; 32] {
    fn from(k: ChunkHashKey) -> Self {
        k.0
    }
}

impl RegionKey for ChunkHashKey {
    const FIELDS: &'static [FieldKind] = &[
        FieldKind::BigEndianBytes,
        FieldKind::BigEndianBytes,
        FieldKind::BigEndianBytes,
        FieldKind::BigEndianBytes,
    ];

    fn field_values(&self, out: &mut [u64]) {
        // Big-endian within each 8-byte chunk so numeric field compare
        // matches lexicographic byte compare.
        let (chunks, _) = self.0.as_chunks::<8>();
        for (slot, chunk) in out.iter_mut().zip(chunks) {
            *slot = u64::from_be_bytes(*chunk);
        }
    }

    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError> {
        let mut bytes = [0u8; 32];
        let (chunks, _) = bytes.as_chunks_mut::<8>();
        for (chunk, field) in chunks.iter_mut().zip(fields) {
            *chunk = field.to_be_bytes();
        }
        Ok(Self(bytes))
    }
}

// ---------- ChunkIndexValue (24 B fixed value image) ----------

/// Size in bytes of [`ChunkIndexValue`]'s on-disk byte image (24 = 4 B
/// `ref_count` + 4 B `length` + 16 B `BlobRef`).
pub const CHUNK_INDEX_VALUE_SIZE: usize = 24;

/// Fixed-size 24-byte value tail for the [`ChunkIndex`] B+ tree. Together
/// with the 32 B `chunk_hash` key this reproduces the spec's
/// 56 B [`ChunkIndexLeafEntry`] layout. Field order matches INDEX-DESIGN§4.3.
///
/// ```text
/// [0..4]   ref_count u32
/// [4..8]   length    u32
/// [8..24]  blob_ref  BlobRef   (disk_id u16 | sector u16 | bucket_no u32 | length u64)
/// ```
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct ChunkIndexValue {
    ref_count: u32, // [0..4]
    length: u32,    // [4..8]
    blob_ref: BlobRef, // [8..24]
}

const_assert_eq!(core::mem::size_of::<ChunkIndexValue>(), CHUNK_INDEX_VALUE_SIZE);

impl ChunkIndexValue {
    /// Construct a fresh value.
    pub fn new(blob_ref: BlobRef, ref_count: u32, length: u32) -> Self {
        Self {
            ref_count,
            length,
            blob_ref,
        }
    }

    /// Copy the [`BlobRef`] out (works around `#[repr(packed)]` alignment).
    pub fn blob_ref(&self) -> BlobRef {
        self.blob_ref
    }

    /// Reference count.
    pub fn ref_count(&self) -> u32 {
        self.ref_count
    }

    /// Plaintext byte count of the chunk this entry references.
    pub fn length(&self) -> u32 {
        self.length
    }
}

impl AsRef<[u8]> for ChunkIndexValue {
    fn as_ref(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

impl ChunkIndexValue {
    /// Decode a value read from the region; anything but exactly
    /// [`CHUNK_INDEX_VALUE_SIZE`] bytes is a corrupt region.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StorageError> {
        bytemuck::try_pod_read_unaligned(bytes)
            .map_err(|_| StorageError::RegionCorrupt("chunk index value is not 24 bytes"))
    }
}

// ---------- ChunkIndex (in-memory mirror) ----------

/// In-memory `ChunkIndex`. Keyed by BLAKE3 chunk hash, value is the physical
/// extent + reference count + plaintext length tracked per chunk
/// (SPECIFICATION-DESIGN§5 / INDEX-DESIGN§4.3).
#[derive(Debug, Clone, Default)]
pub struct ChunkIndex {
    entries: HashMap<ChunkHashKey, ChunkIndexValue>,
}

impl ChunkIndex {
    /// New, empty `ChunkIndex`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a chunk by hash. Returns `None` if absent.
    pub fn lookup(&self, hash: &[u8; 32]) -> Option<BlobRef> {
        self.entries
            .get(&ChunkHashKey(*hash))
            .map(|v| v.blob_ref())
    }

    /// Borrow `(blob, ref_count, length)` for a given hash.
    pub fn entry(&self, hash: &[u8; 32]) -> Option<(BlobRef, u32, u32)> {
        self.entries
            .get(&ChunkHashKey(*hash))
            .map(|v| (v.blob_ref(), v.ref_count(), v.length()))
    }

    /// Plaintext byte count of the chunk, or 0 if absent.
    pub fn length(&self, hash: &[u8; 32]) -> u32 {
        self.entries
            .get(&ChunkHashKey(*hash))
            .map(|v| v.length())
            .unwrap_or(0)
    }

    /// Insert a new chunk if missing, otherwise increment its `ref_count`.
    /// `length` is the chunk's plaintext byte count; on a duplicate insert
    /// the stored length is left unchanged (the existing entry is the
    /// authoritative copy by content-addressed dedup). Returns the
    /// resulting `BlobRef`.
    pub fn insert_or_bump(&mut self, hash: [u8; 32], blob: BlobRef, length: u32) -> BlobRef {
        let entry = self
            .entries
            .entry(ChunkHashKey(hash))
            .or_insert_with(|| ChunkIndexValue::new(blob, 0, length));
        entry.ref_count = entry.ref_count.saturating_add(1);
        entry.blob_ref()
    }

    /// Decrement the refcount; remove if it reaches zero. Returns `true` if
    /// the entry was removed (caller should reclaim the blob).
    pub fn decrement(&mut self, hash: &[u8; 32]) -> bool {
        let key = ChunkHashKey(*hash);
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.ref_count <= 1 {
                self.entries.remove(&key);
                true
            } else {
                entry.ref_count -= 1;
                false
            }
        } else {
            false
        }
    }

    /// Number of unique chunks tracked.
    pub fn chunk_count(&self) -> usize {
        self.entries.len()
    }

    /// Reference count for a hash, or 0 if absent.
    pub fn ref_count(&self, hash: &[u8; 32]) -> u32 {
        self.entries
            .get(&ChunkHashKey(*hash))
            .map(|v| v.ref_count())
            .unwrap_or(0)
    }

    // ----------------------------------------------------------------
    // BTREE-DESIGN§1 region persistence.
    // ----------------------------------------------------------------

    /// Write the in-memory state as the 256 KiB region at byte `offset` on
    /// `device`, replacing whatever was there.
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<(), IndexError> {
        let mut entries: Vec<(ChunkHashKey, RunEntry<'_>)> = self
            .entries
            .iter()
            .map(|(hash, value)| (*hash, RunEntry::Value(bytemuck::bytes_of(value))))
            .collect();
        entries.sort_by_key(|(hash, _)| *hash);
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
        let region = Region::open(device, offset, BtreeKind::ChunkIndex)?;
        let mut entries: HashMap<ChunkHashKey, ChunkIndexValue> = HashMap::new();
        for (hash, value) in region.entries::<D, ChunkHashKey>(device)? {
            entries.insert(hash, ChunkIndexValue::from_bytes(&value)?);
        }
        Ok(Self { entries })
    }
}

fn region_params() -> RegionParams {
    RegionParams {
        kind: BtreeKind::ChunkIndex,
        level: 0,
        region_size_log2: REGION_SIZE_LOG2,
        node_seq: 0,
        created_lsn: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn b(bucket_no: u32) -> BlobRef {
        BlobRef::new(0, bucket_no, 0, 4096)
    }

    const TEST_LEN: u32 = 4096;

    #[test]
    fn chunk_index_leaf_entry_size_is_56() {
        // computed: 32 + 4 + 4 + 16 = 56
        assert_eq!(core::mem::size_of::<ChunkIndexLeafEntry>(), 56);
    }

    #[test]
    fn chunk_index_value_size_is_24() {
        assert_eq!(core::mem::size_of::<ChunkIndexValue>(), 24);
    }

    #[test]
    fn insert_and_dedup_bumps_refcount() {
        let mut idx = ChunkIndex::new();
        let r1 = idx.insert_or_bump(h(1), b(100), TEST_LEN);
        let r2 = idx.insert_or_bump(h(1), b(200), 9999); // duplicate — second args ignored
        assert_eq!(r1, r2);
        assert_eq!(idx.ref_count(&h(1)), 2);
        assert_eq!(idx.length(&h(1)), TEST_LEN);
        assert_eq!(idx.chunk_count(), 1);
    }

    #[test]
    fn decrement_removes_when_zero() {
        let mut idx = ChunkIndex::new();
        idx.insert_or_bump(h(1), b(100), TEST_LEN);
        idx.insert_or_bump(h(1), b(100), TEST_LEN);
        assert_eq!(idx.ref_count(&h(1)), 2);
        assert!(!idx.decrement(&h(1)));
        assert_eq!(idx.ref_count(&h(1)), 1);
        assert!(idx.decrement(&h(1))); // last ref
        assert_eq!(idx.chunk_count(), 0);
    }

    #[test]
    fn lookup_absent_returns_none() {
        let mut idx = ChunkIndex::new();
        assert!(idx.lookup(&h(99)).is_none());
        assert_eq!(idx.length(&h(99)), 0);
        assert!(!idx.decrement(&h(99)));
    }

    #[test]
    fn entry_returns_full_triple() {
        let mut idx = ChunkIndex::new();
        idx.insert_or_bump(h(5), b(50), 2048);
        let (blob, rc, len) = idx.entry(&h(5)).unwrap();
        assert_eq!(blob, b(50));
        assert_eq!(rc, 1);
        assert_eq!(len, 2048);
    }

    // ----- B+ tree region round-trip -----

    use mimisbrunnr_storage::FileBlockDevice;
    use tempfile::TempDir;

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("chunk_index.bin");
        // 1 MiB is plenty for one 256 KiB region.
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    #[test]
    fn region_round_trip_empty_returns_default() {
        let (_dir, dev) = fresh_device();
        let idx = ChunkIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(idx.chunk_count(), 0);
    }

    #[test]
    fn region_round_trip_preserves_entries_refcounts_and_lengths() {
        let (_dir, dev) = fresh_device();
        let mut idx = ChunkIndex::new();
        for i in 1u8..=10 {
            idx.insert_or_bump(h(i), b(i as u32 * 100), i as u32 * 1024);
        }
        // Bump a few to non-1 refcounts.
        idx.insert_or_bump(h(3), b(300), 3 * 1024);
        idx.insert_or_bump(h(3), b(300), 3 * 1024);
        idx.insert_or_bump(h(7), b(700), 7 * 1024);

        idx.flush_to_region(&dev, 0).unwrap();
        let back = ChunkIndex::load_from_region(&dev, 0).unwrap();

        assert_eq!(back.chunk_count(), idx.chunk_count());
        for i in 1u8..=10 {
            assert_eq!(back.lookup(&h(i)), Some(b(i as u32 * 100)), "blob {i}");
            assert_eq!(back.ref_count(&h(i)), idx.ref_count(&h(i)), "rc {i}");
            assert_eq!(back.length(&h(i)), i as u32 * 1024, "len {i}");
        }
    }

    #[test]
    fn region_round_trip_single_entry() {
        // A single-entry run: every key field is constant (width 0), so the
        // key is carried entirely by the field bases.
        let (_dir, dev) = fresh_device();
        let mut idx = ChunkIndex::new();
        idx.insert_or_bump(h(42), b(4242), 8192);
        idx.flush_to_region(&dev, 0).unwrap();
        let back = ChunkIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.chunk_count(), 1);
        assert_eq!(back.lookup(&h(42)), Some(b(4242)));
        assert_eq!(back.ref_count(&h(42)), 1);
        assert_eq!(back.length(&h(42)), 8192);
    }

    #[test]
    fn region_round_trip_with_shared_disk_id_value_prefix() {
        // All entries share `disk_id = 0`, `ref_count = 1` and `length`, so
        // most value bytes are identical across entries; values must still
        // round-trip byte-exact.
        let (_dir, dev) = fresh_device();
        let mut idx = ChunkIndex::new();
        for i in 0u8..32 {
            idx.insert_or_bump(h(i + 1), b(i as u32 + 1), 1024);
        }
        idx.flush_to_region(&dev, 0).unwrap();
        let back = ChunkIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.chunk_count(), 32);
        for i in 0u8..32 {
            assert_eq!(back.lookup(&h(i + 1)), Some(b(i as u32 + 1)));
        }
    }

    #[test]
    fn region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = ChunkIndex::new();
        first.insert_or_bump(h(1), b(100), TEST_LEN);
        first.insert_or_bump(h(2), b(200), TEST_LEN);
        first.flush_to_region(&dev, 0).unwrap();

        let mut second = ChunkIndex::new();
        second.insert_or_bump(h(9), b(900), TEST_LEN);
        second.flush_to_region(&dev, 0).unwrap();

        let back = ChunkIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.chunk_count(), 1);
        assert_eq!(back.lookup(&h(9)), Some(b(900)));
        assert!(back.lookup(&h(1)).is_none());
    }

    #[test]
    fn region_round_trip_flushed_empty_index() {
        let (_dir, dev) = fresh_device();
        ChunkIndex::new().flush_to_region(&dev, 0).unwrap();
        let region = Region::open(&dev, 0, BtreeKind::ChunkIndex).unwrap();
        assert_eq!(region.run_count(), 0);
        let back = ChunkIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.chunk_count(), 0);
    }

    #[test]
    fn region_round_trip_preserves_all_entries() {
        let (_dir, dev) = fresh_device();
        let mut idx = ChunkIndex::new();
        for i in 1u8..=5 {
            idx.insert_or_bump(h(i), b(i as u32 * 10), i as u32 * 100);
        }
        idx.insert_or_bump(h(3), b(30), 300);
        idx.insert_or_bump(h(3), b(30), 300);

        idx.flush_to_region(&dev, 0).unwrap();
        let region = Region::open(&dev, 0, BtreeKind::ChunkIndex).unwrap();
        assert_eq!(region.run_count(), 1);
        assert_eq!(region.entries::<_, ChunkHashKey>(&dev).unwrap().len(), 5);
        let back = ChunkIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.chunk_count(), 5);
        for i in 1u8..=5 {
            assert_eq!(back.lookup(&h(i)), Some(b(i as u32 * 10)));
            assert_eq!(back.length(&h(i)), i as u32 * 100);
        }
        assert_eq!(back.ref_count(&h(3)), 3);
    }

    #[test]
    fn region_round_trip_100_random_hashes() {
        // 100 distinct chunk hashes (synthetic, not BLAKE3 — deterministic
        // to keep tests reproducible) must round-trip bit-perfectly.
        let (_dir, dev) = fresh_device();
        let mut idx = ChunkIndex::new();
        for i in 0u32..100 {
            let mut hash = [0u8; 32];
            for (j, slot) in hash.iter_mut().enumerate() {
                *slot = ((i.wrapping_mul(j as u32 + 17) ^ 0xa5) & 0xff) as u8;
            }
            idx.insert_or_bump(hash, b(i + 1), i + 1);
        }
        assert_eq!(idx.chunk_count(), 100);
        idx.flush_to_region(&dev, 0).unwrap();
        let back = ChunkIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.chunk_count(), 100);
        for i in 0u32..100 {
            let mut hash = [0u8; 32];
            for (j, slot) in hash.iter_mut().enumerate() {
                *slot = ((i.wrapping_mul(j as u32 + 17) ^ 0xa5) & 0xff) as u8;
            }
            assert_eq!(back.lookup(&hash), Some(b(i + 1)));
            assert_eq!(back.length(&hash), i + 1);
        }
    }

    #[test]
    fn on_disk_region_stores_value_byte_images() {
        // Four entries fit one run of one data block; each value is the raw
        // 24-byte image, looked up directly by hash.
        let (_dir, dev) = fresh_device();
        let mut idx = ChunkIndex::new();
        for i in 1u8..=4 {
            idx.insert_or_bump(h(i), b(i as u32), TEST_LEN);
        }
        idx.flush_to_region(&dev, 0).unwrap();

        let region = Region::open(&dev, 0, BtreeKind::ChunkIndex).unwrap();
        assert_eq!(region.run_count(), 1);
        assert_eq!(region.next_block(), 3, "block 0, one index block, one data block");
        let value = region.lookup(&dev, &ChunkHashKey(h(3))).unwrap().unwrap();
        assert_eq!(value.as_bytes(), ChunkIndexValue::new(b(3), 1, TEST_LEN).as_ref());
        assert!(region.lookup(&dev, &ChunkHashKey(h(9))).unwrap().is_none());
    }

    #[test]
    fn region_kind_mismatch_errors() {
        let (_dir, dev) = fresh_device();
        mimisbrunnr_storage::FreespaceLru::new().flush_to_region(&dev, 0).unwrap();
        let err = ChunkIndex::load_from_region(&dev, 0).err();
        assert!(matches!(
            err,
            Some(IndexError::Storage(StorageError::RegionKindMismatch { expected: 10, actual: 13 }))
        ));
    }

    // ----- ChunkHashKey scaffolding -----

    #[test]
    fn chunk_hash_key_region_key_round_trip() {
        let mut bytes = [0u8; 32];
        for (i, slot) in bytes.iter_mut().enumerate() {
            *slot = (i * 7 + 3) as u8;
        }
        let key = ChunkHashKey(bytes);
        let mut fields = [0u64; 4];
        key.field_values(&mut fields);
        let recovered = ChunkHashKey::from_field_values(&fields).unwrap();
        assert_eq!(recovered, key);
    }

    #[test]
    fn chunk_hash_key_field_order_matches_ord() {
        // Hashes differing only in the last byte order the same through
        // their fields as through `Ord`.
        let mut low_bytes = [0xffu8; 32];
        low_bytes[31] = 0x01;
        let mut high_bytes = [0xffu8; 32];
        high_bytes[31] = 0x02;
        let (low, high) = (ChunkHashKey(low_bytes), ChunkHashKey(high_bytes));
        assert!(low < high);
        let mut low_fields = [0u64; 4];
        let mut high_fields = [0u64; 4];
        low.field_values(&mut low_fields);
        high.field_values(&mut high_fields);
        assert!(low_fields < high_fields);
    }

    #[test]
    fn chunk_index_value_byte_image_round_trip() {
        let v = ChunkIndexValue::new(b(42), 7, 8192);
        let bytes = v.as_ref().to_vec();
        assert_eq!(bytes.len(), CHUNK_INDEX_VALUE_SIZE);
        let back = ChunkIndexValue::from_bytes(&bytes).unwrap();
        assert_eq!(back, v);
        assert_eq!(back.blob_ref(), b(42));
        assert_eq!(back.ref_count(), 7);
        assert_eq!(back.length(), 8192);
        assert!(matches!(
            ChunkIndexValue::from_bytes(&bytes[..23]),
            Err(StorageError::RegionCorrupt(_))
        ));
    }

    // ----- Two-region independence (ChunkIndex + KvIndex side-by-side) -----

    #[test]
    fn two_regions_at_distinct_offsets_dont_interfere() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("two-regions.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();

        let mut idx_a = ChunkIndex::new();
        idx_a.insert_or_bump(h(1), b(11), TEST_LEN);
        idx_a.insert_or_bump(h(2), b(22), TEST_LEN);
        idx_a.flush_to_region(&dev, 0).unwrap();

        let mut idx_b = ChunkIndex::new();
        idx_b.insert_or_bump(h(9), b(99), TEST_LEN);
        idx_b.flush_to_region(&dev, 256 * 1024).unwrap();

        let back_a = ChunkIndex::load_from_region(&dev, 0).unwrap();
        let back_b = ChunkIndex::load_from_region(&dev, 256 * 1024).unwrap();
        assert_eq!(back_a.chunk_count(), 2);
        assert_eq!(back_b.chunk_count(), 1);
        assert_eq!(back_a.lookup(&h(1)), Some(b(11)));
        assert_eq!(back_b.lookup(&h(9)), Some(b(99)));
    }
}
