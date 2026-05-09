//! Chunk index — content-addressed dedup directory (IMPL §9.3).
//!
//! On-disk: a B+ tree of large nodes keyed by 32-byte BLAKE3 chunk hash; the
//! leaf entry [`ChunkIndexLeafEntry`] is 56 B per IMPL §9.3 lines 1946–1951
//! (32 B hash + 4 B `ref_count` + 4 B `length` + 16 B `BlobRef`). With the
//! hash carried as the §1.5.6 packed key, the on-disk value tail is the
//! 24-byte [`ChunkIndexValue`] = `(ref_count, length, blob_ref)`.
//!
//! In-memory mirror: [`ChunkIndex`] = `HashMap<[u8;32], ChunkIndexValue>`.
//!
//! ## Persistence (R1c-A3.1)
//!
//! On disk the index occupies one 256 KiB §1.5 B+ tree region of
//! [`BtreeKind::ChunkIndex`]. Entries are serialised through the
//! `SORTED_RUN_FLAG_PACKED_KEYS` codec (§1.5.6) — keys are packed as four
//! big-endian `u64` fields covering the 32-byte hash, values are written as
//! the 24-byte [`ChunkIndexValue`] byte image. The C1 amendment persists the
//! `common_value_prefix` bytes inline in the descriptor, so the codec
//! correctly round-trips even single-entry runs and runs whose values
//! happen to share leading bytes (e.g. all entries with the same `disk_id`).

use std::collections::HashMap;

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{
        BlobRef, BlockDevice, BtreeKind, BtreeRegion, FieldHints, LoadedNode, PackError,
        PackableKey, SortedRun,
    },
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::error::IndexError;

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // consumed by engine layout once R1b lands engine-side.
pub const CHUNK_INDEX_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

// ---------- ChunkIndexLeafEntry (full record, mirror only) ----------

/// Size in bytes of [`ChunkIndexLeafEntry`] (56). IMPL §9.3.
pub const CHUNK_INDEX_LEAF_ENTRY_SIZE: usize = 56;

/// On-disk leaf entry layout described by IMPL §9.3. The full record is
/// 56 B; the codec splits it into the 32 B `chunk_hash` (the §1.5.6
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

// ---------- ChunkHashKey (packable key) ----------

/// 32-byte BLAKE3 chunk hash. Used as the [`ChunkIndex`] map key and as the
/// §1.5.6 packed-run key (decomposes into four big-endian `u64` fields so
/// byte-wise lexicographic compare matches the natural hash byte order).
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

impl Serialize for ChunkHashKey {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        // Lowercase base-16; 64 chars total, no separators. Used by the CBOR
        // fallback path inside `BtreeRegion::read_packed`'s `K: Deserialize`
        // bound; the production path round-trips through the packed codec.
        let mut buf = [0u8; 64];
        for (i, byte) in self.0.iter().enumerate() {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            buf[2 * i] = HEX[(byte >> 4) as usize];
            buf[2 * i + 1] = HEX[(byte & 0xf) as usize];
        }
        // SAFETY: HEX bytes are all ASCII, so the buffer is valid UTF-8.
        let s = core::str::from_utf8(&buf).map_err(serde::ser::Error::custom)?;
        ser.serialize_str(s)
    }
}

impl<'de> Deserialize<'de> for ChunkHashKey {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        if s.len() != 64 {
            return Err(serde::de::Error::custom(format!(
                "expected 64 hex chars, got {}",
                s.len()
            )));
        }
        let mut out = [0u8; 32];
        let bytes = s.as_bytes();
        for (i, slot) in out.iter_mut().enumerate() {
            let hi = hex_nibble(bytes[2 * i]).map_err(serde::de::Error::custom)?;
            let lo = hex_nibble(bytes[2 * i + 1]).map_err(serde::de::Error::custom)?;
            *slot = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

fn hex_nibble(c: u8) -> Result<u8, &'static str> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err("non-hex character in ChunkHashKey"),
    }
}

const CHUNK_HASH_KEY_HINTS: [FieldHints; 4] = [
    FieldHints::unsigned_msb(),
    FieldHints::unsigned_msb(),
    FieldHints::unsigned_msb(),
    FieldHints::unsigned_msb(),
];

impl PackableKey for ChunkHashKey {
    fn nr_fields() -> usize {
        4
    }
    fn key_header_bytes() -> usize {
        0
    }
    fn field_hints() -> &'static [FieldHints] {
        &CHUNK_HASH_KEY_HINTS
    }
    fn field_values(&self, out: &mut [u64]) {
        // Big-endian within each 8-byte chunk so lexicographic byte compare
        // matches numeric compare (and matches the packed `MSB_FIRST` flag).
        let mut buf = [0u8; 8];
        for (i, slot) in out.iter_mut().enumerate().take(4) {
            buf.copy_from_slice(&self.0[i * 8..i * 8 + 8]);
            *slot = u64::from_be_bytes(buf);
        }
    }
    fn from_components(_header: u32, fields: &[u64]) -> Result<Self, PackError> {
        if fields.len() != 4 {
            return Err(PackError::Malformed("ChunkHashKey: wrong field count"));
        }
        let mut bytes = [0u8; 32];
        for (i, &field) in fields.iter().enumerate().take(4) {
            bytes[i * 8..i * 8 + 8].copy_from_slice(&field.to_be_bytes());
        }
        Ok(Self(bytes))
    }
}

// ---------- ChunkIndexValue (24 B fixed value image) ----------

/// Size in bytes of [`ChunkIndexValue`]'s on-disk byte image (24 = 4 B
/// `ref_count` + 4 B `length` + 16 B `BlobRef`).
pub const CHUNK_INDEX_VALUE_SIZE: usize = 24;

/// Fixed-size 24-byte value tail for the [`ChunkIndex`] B+ tree. Together
/// with the 32 B `chunk_hash` packed-key prefix this reproduces the spec's
/// 56 B [`ChunkIndexLeafEntry`] layout. Field order matches IMPL §9.3.
///
/// ```text
/// [0..4]   ref_count u32
/// [4..8]   length    u32
/// [8..24]  blob_ref  BlobRef   (disk_id u16 | _pad u16 | block_no u32 | length u64)
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

impl From<Vec<u8>> for ChunkIndexValue {
    fn from(v: Vec<u8>) -> Self {
        // The packed reader always supplies exactly `value_size` bytes; pad
        // or truncate defensively so a malformed payload yields a zero value
        // instead of panicking. (`BtreeRegion::read_packed` validates the
        // sorted-run CRC before we get here, so this is belt-and-braces.)
        let mut buf = [0u8; CHUNK_INDEX_VALUE_SIZE];
        let n = v.len().min(CHUNK_INDEX_VALUE_SIZE);
        buf[..n].copy_from_slice(&v[..n]);
        *bytemuck::from_bytes(&buf)
    }
}

// `ChunkIndexValue` needs `Serialize + Deserialize + Clone` to satisfy the
// `BtreeRegion::read_packed` bounds (the CBOR fallback path). We always
// write packed runs so the CBOR codec is never exercised for this type, but
// the compiler needs the impls anyway.
impl Serialize for ChunkIndexValue {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_bytes(self.as_ref())
    }
}

impl<'de> Deserialize<'de> for ChunkIndexValue {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let bytes: Vec<u8> = serde_bytes_helper::deserialize_bytes(de)?;
        Ok(Self::from(bytes))
    }
}

mod serde_bytes_helper {
    //! Tiny shim to deserialise a byte array via either `serialize_bytes` or
    //! a sequence of u8 — `ciborium` emits one form, JSON the other.
    use serde::de::{Error, SeqAccess, Visitor};

    pub fn deserialize_bytes<'de, D: serde::Deserializer<'de>>(
        de: D,
    ) -> Result<Vec<u8>, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("byte string")
            }
            fn visit_bytes<E: Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                Ok(v.to_vec())
            }
            fn visit_byte_buf<E: Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
                Ok(v)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(b) = seq.next_element::<u8>()? {
                    out.push(b);
                }
                Ok(out)
            }
        }
        de.deserialize_bytes(V)
    }
}

// ---------- ChunkIndex (in-memory mirror) ----------

/// In-memory `ChunkIndex`. Keyed by BLAKE3 chunk hash, value is the physical
/// extent + reference count + plaintext length tracked per chunk
/// (DESIGN §5 / IMPL §9.3).
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
    // §1.5 B+ tree persistence (packed-key codec, §1.5.6).
    // ----------------------------------------------------------------

    /// Build a [`LoadedNode`] containing every entry as a single packed
    /// sorted run sorted by chunk hash. The node uses
    /// [`BtreeKind::ChunkIndex`] and the spec's 18-bit (256 KiB) region
    /// size.
    pub fn to_loaded_node(&self) -> LoadedNode<ChunkHashKey, ChunkIndexValue> {
        let mut entries: Vec<(ChunkHashKey, ChunkIndexValue)> =
            self.entries.iter().map(|(k, v)| (*k, *v)).collect();
        entries.sort_by_key(|e| e.0);

        let mut node: LoadedNode<ChunkHashKey, ChunkIndexValue> =
            LoadedNode::new(BtreeKind::ChunkIndex, 0, REGION_SIZE_LOG2);
        if !entries.is_empty() {
            let run = SortedRun::from_sorted(0, 0, entries);
            node.sorted_runs.push(run);
            node.header.sorted_run_count = 1;
        }
        node
    }

    /// Restore the in-memory state from a [`LoadedNode`] produced by
    /// [`BtreeRegion::read_packed`].
    pub fn from_loaded_node(node: &LoadedNode<ChunkHashKey, ChunkIndexValue>) -> Self {
        let mut entries: HashMap<ChunkHashKey, ChunkIndexValue> = HashMap::new();
        for (k, v) in node.merge_iter() {
            entries.insert(*k, *v);
        }
        Self { entries }
    }

    /// Write the in-memory state as a fresh 256 KiB region at byte `offset`
    /// on `device`. Replaces the region wholesale via
    /// [`BtreeRegion::write_full_packed`].
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<(), IndexError> {
        let mut node = self.to_loaded_node();
        BtreeRegion::write_full_packed::<D, ChunkHashKey, ChunkIndexValue>(
            device,
            offset,
            &mut node,
            CHUNK_INDEX_VALUE_SIZE,
        )?;
        Ok(())
    }

    /// Read the in-memory state from the 256 KiB region at byte `offset` on
    /// `device`. An all-zero region is treated as "empty index" and returns
    /// [`Self::default`].
    pub fn load_from_region<D: BlockDevice>(
        device: &D,
        offset: u64,
    ) -> Result<Self, IndexError> {
        // Probe the first 8 bytes — a fresh (all-zero) region has no magic.
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe)?;
        if probe.iter().all(|&b| b == 0) {
            return Ok(Self::default());
        }
        let node = BtreeRegion::read_packed::<D, ChunkHashKey, ChunkIndexValue>(
            device,
            offset,
            BtreeKind::ChunkIndex,
            CHUNK_INDEX_VALUE_SIZE,
        )?;
        Ok(Self::from_loaded_node(&node))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn b(no: u32) -> BlobRef {
        BlobRef {
            disk_id: 0,
            _pad: 0,
            block_no: no,
            length: 4096,
        }
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

    // ----- B+ tree region round-trip (packed wire format) -----

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
        // The single-entry case used to trigger the C1 prefix-elision bug
        // (the descriptor's `common_value_prefix` would saturate to the
        // value's full length, eliding the entire value with no on-disk
        // record). Post-C1, the descriptor persists the prefix bytes
        // inline so reconstruction is byte-exact.
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
        // All entries share `disk_id = 0` + `_pad = 0`, so the value tail
        // has 4 leading bytes (the BlobRef's leading u16+u16) shared
        // across every entry — wait, actually with the new layout the
        // BlobRef sits at offset 8, so the shared prefix is bounded by
        // the prefix of the (ref_count, length) pair, which varies. Even
        // so, this is a useful regression guard against future field
        // reorderings.
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
    fn empty_index_round_trips_through_loaded_node() {
        let idx = ChunkIndex::new();
        let node = idx.to_loaded_node();
        assert_eq!(node.sorted_runs.len(), 0);
        let back = ChunkIndex::from_loaded_node(&node);
        assert_eq!(back.chunk_count(), 0);
    }

    #[test]
    fn loaded_node_round_trip_preserves_all_entries() {
        let mut idx = ChunkIndex::new();
        for i in 1u8..=5 {
            idx.insert_or_bump(h(i), b(i as u32 * 10), i as u32 * 100);
        }
        idx.insert_or_bump(h(3), b(30), 300);
        idx.insert_or_bump(h(3), b(30), 300);

        let node = idx.to_loaded_node();
        assert_eq!(node.sorted_runs.len(), 1);
        assert_eq!(node.sorted_runs[0].entries.len(), 5);
        let back = ChunkIndex::from_loaded_node(&node);
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
    fn on_disk_run_carries_packed_keys_flag() {
        use mimisbrunnr_storage::{
            BLOCK_SIZE, BlockDevice, BtreeNodeHeader, SORTED_RUN_FLAG_PACKED_KEYS, SortedRunHeader,
        };
        let (_dir, dev) = fresh_device();
        let mut idx = ChunkIndex::new();
        for i in 1u8..=4 {
            idx.insert_or_bump(h(i), b(i as u32), TEST_LEN);
        }
        idx.flush_to_region(&dev, 0).unwrap();

        // Read the §1.5 region header + first sorted-run header off disk
        // and verify the packed flag is set — the codec is in use.
        let mut header_sector = vec![0u8; BLOCK_SIZE];
        dev.read_at(0, &mut header_sector).unwrap();
        let _ = BtreeNodeHeader::parse(&header_sector).unwrap();

        let mut run_header_buf = [0u8; std::mem::size_of::<SortedRunHeader>()];
        dev.read_at(BLOCK_SIZE as u64, &mut run_header_buf).unwrap();
        let run_header: SortedRunHeader = *bytemuck::from_bytes(&run_header_buf);
        assert!(({ run_header.flags } & SORTED_RUN_FLAG_PACKED_KEYS) != 0);
    }

    // ----- ChunkHashKey scaffolding -----

    #[test]
    fn chunk_hash_key_packable_round_trip() {
        let mut bytes = [0u8; 32];
        for (i, slot) in bytes.iter_mut().enumerate() {
            *slot = (i * 7 + 3) as u8;
        }
        let key = ChunkHashKey(bytes);
        let mut fields = [0u64; 4];
        key.field_values(&mut fields);
        let recovered = ChunkHashKey::from_components(0, &fields).unwrap();
        assert_eq!(recovered, key);
    }

    #[test]
    fn chunk_index_value_byte_image_round_trip() {
        let v = ChunkIndexValue::new(b(42), 7, 8192);
        let bytes = v.as_ref().to_vec();
        assert_eq!(bytes.len(), CHUNK_INDEX_VALUE_SIZE);
        let back = ChunkIndexValue::from(bytes);
        assert_eq!(back, v);
        assert_eq!(back.blob_ref(), b(42));
        assert_eq!(back.ref_count(), 7);
        assert_eq!(back.length(), 8192);
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
