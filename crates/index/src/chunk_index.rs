//! Chunk index — content-addressed dedup directory (IMPL §9.3).
//!
//! On-disk: a B+ tree of large nodes keyed by 32-byte BLAKE3 chunk hash; the
//! leaf entry [`ChunkIndexLeafEntry`] is 56 B per IMPL §9.3 lines 1946–1951.
//!
//! In-memory mirror: [`ChunkIndex`] = `HashMap<[u8;32], (BlobRef, u32 ref_count)>`.

use std::collections::HashMap;

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::BlobRef,
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::error::IndexError;

// ---------- ChunkIndexLeafEntry ----------

/// Size in bytes of [`ChunkIndexLeafEntry`] (56). IMPL §9.3.
pub const CHUNK_INDEX_LEAF_ENTRY_SIZE: usize = 56;

/// On-disk leaf entry for the content-addressed `ChunkIndex` B+ tree.
/// IMPL §9.3.
///
/// Layout:
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
// computed: 32 (chunk_hash) + 4 (ref_count) + 4 (length) + 16 (BlobRef) = 56

// ---------- ChunkIndex (in-memory mirror) ----------

/// Newtype around a 32-byte chunk hash so the `ChunkIndex` can derive
/// `Serialize`/`Deserialize` directly. Serde's default `[u8; 32]` map-key
/// representation depends on the format (CBOR encodes the byte array, but
/// JSON-style formats reject byte-array keys); using a base-16 string here
/// is unambiguous, format-agnostic, and stable across crate versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
        // Lowercase base-16; 64 chars total, no separators.
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

/// Serialisable proxy for `BlobRef` (the upstream type derives neither
/// `Serialize` nor `Deserialize`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct BlobRefSerde {
    disk_id: u16,
    pad: u16,
    block_no: u32,
    length: u64,
}

impl From<BlobRef> for BlobRefSerde {
    fn from(b: BlobRef) -> Self {
        Self {
            disk_id: { b.disk_id },
            pad: { b._pad },
            block_no: { b.block_no },
            length: { b.length },
        }
    }
}

impl From<BlobRefSerde> for BlobRef {
    fn from(s: BlobRefSerde) -> Self {
        Self {
            disk_id: s.disk_id,
            _pad: s.pad,
            block_no: s.block_no,
            length: s.length,
        }
    }
}

/// Internal serde-friendly value: the proxy `BlobRef` plus the refcount.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct ChunkEntrySerde {
    blob: BlobRefSerde,
    ref_count: u32,
}

/// In-memory `ChunkIndex`. Keyed by BLAKE3 chunk hash, value is the physical
/// extent and a reference count tracking how many `ChunkList` chains point
/// at this chunk (DESIGN §5 / IMPL §9.3).
///
/// `Serialize` / `Deserialize` are derived via the [`ChunkHashKey`] newtype
/// (base-16 string keys) and a per-entry serde proxy for [`BlobRef`]. That
/// makes the type usable directly with `ciborium::ser::into_writer` /
/// `ciborium::de::from_reader` — no crate-local helpers needed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChunkIndex {
    entries: HashMap<ChunkHashKey, ChunkEntrySerde>,
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
            .map(|e| e.blob.into())
    }

    /// Borrow `(blob, ref_count)` for a given hash.
    pub fn entry(&self, hash: &[u8; 32]) -> Option<(BlobRef, u32)> {
        self.entries
            .get(&ChunkHashKey(*hash))
            .map(|e| (e.blob.into(), e.ref_count))
    }

    /// Insert a new chunk if missing, otherwise increment its `ref_count`.
    /// Returns the resulting `BlobRef` (the freshly inserted one, or the
    /// existing one — content-addressed dedup makes them identical).
    pub fn insert_or_bump(&mut self, hash: [u8; 32], blob: BlobRef) -> BlobRef {
        let entry = self.entries.entry(ChunkHashKey(hash)).or_insert(ChunkEntrySerde {
            blob: blob.into(),
            ref_count: 0,
        });
        entry.ref_count = entry.ref_count.saturating_add(1);
        entry.blob.into()
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
            .map(|e| e.ref_count)
            .unwrap_or(0)
    }

    /// Serialise to CBOR. The type derives `Serialize` directly; this
    /// helper is retained for symmetry with the other indices.
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

    #[test]
    fn chunk_index_leaf_entry_size_is_56() {
        // computed: 32 + 4 + 4 + 16 = 56
        assert_eq!(core::mem::size_of::<ChunkIndexLeafEntry>(), 56);
    }

    #[test]
    fn insert_and_dedup_bumps_refcount() {
        let mut idx = ChunkIndex::new();
        let r1 = idx.insert_or_bump(h(1), b(100));
        let r2 = idx.insert_or_bump(h(1), b(200)); // duplicate — second blob arg ignored
        assert_eq!(r1, r2);
        assert_eq!(idx.ref_count(&h(1)), 2);
        assert_eq!(idx.chunk_count(), 1);
    }

    #[test]
    fn decrement_removes_when_zero() {
        let mut idx = ChunkIndex::new();
        idx.insert_or_bump(h(1), b(100));
        idx.insert_or_bump(h(1), b(100));
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
        assert!(!idx.decrement(&h(99)));
    }

    #[test]
    fn chunk_hash_key_serde_round_trip_via_ciborium_directly() {
        // Verify the ChunkIndex derives Serialize/Deserialize and round-trips
        // through `ciborium::{ser,de}` without crate-local helpers.
        let mut idx = ChunkIndex::new();
        idx.insert_or_bump(h(7), b(700));
        idx.insert_or_bump(h(8), b(800));
        idx.insert_or_bump(h(8), b(800));
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&idx, &mut buf).unwrap();
        let back: ChunkIndex = ciborium::de::from_reader(buf.as_slice()).unwrap();
        assert_eq!(back.chunk_count(), 2);
        assert_eq!(back.ref_count(&h(7)), 1);
        assert_eq!(back.ref_count(&h(8)), 2);
    }

    #[test]
    fn cbor_round_trip() {
        let mut idx = ChunkIndex::new();
        idx.insert_or_bump(h(1), b(100));
        idx.insert_or_bump(h(2), b(200));
        idx.insert_or_bump(h(2), b(200));
        let bytes = idx.serialise().unwrap();
        let back = ChunkIndex::deserialise(&bytes).unwrap();
        assert_eq!(back.chunk_count(), 2);
        assert_eq!(back.ref_count(&h(1)), 1);
        assert_eq!(back.ref_count(&h(2)), 2);
    }
}
