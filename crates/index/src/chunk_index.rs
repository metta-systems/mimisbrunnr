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

/// In-memory `ChunkIndex`. Keyed by BLAKE3 chunk hash, value is the physical
/// extent and a reference count tracking how many `ChunkList` chains point
/// at this chunk (DESIGN §5 / IMPL §9.3).
#[derive(Debug, Clone, Default)]
pub struct ChunkIndex {
    entries: HashMap<[u8; 32], (BlobRef, u32)>,
}

impl ChunkIndex {
    /// New, empty `ChunkIndex`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a chunk by hash. Returns `None` if absent.
    pub fn lookup(&self, hash: &[u8; 32]) -> Option<BlobRef> {
        self.entries.get(hash).map(|(b, _)| *b)
    }

    /// Borrow `(blob, ref_count)` for a given hash.
    pub fn entry(&self, hash: &[u8; 32]) -> Option<(BlobRef, u32)> {
        self.entries.get(hash).copied()
    }

    /// Insert a new chunk if missing, otherwise increment its `ref_count`.
    /// Returns the resulting `BlobRef` (the freshly inserted one, or the
    /// existing one — content-addressed dedup makes them identical).
    pub fn insert_or_bump(&mut self, hash: [u8; 32], blob: BlobRef) -> BlobRef {
        let entry = self.entries.entry(hash).or_insert((blob, 0));
        entry.1 = entry.1.saturating_add(1);
        entry.0
    }

    /// Decrement the refcount; remove if it reaches zero. Returns `true` if
    /// the entry was removed (caller should reclaim the blob).
    pub fn decrement(&mut self, hash: &[u8; 32]) -> bool {
        if let Some(entry) = self.entries.get_mut(hash) {
            if entry.1 <= 1 {
                self.entries.remove(hash);
                true
            } else {
                entry.1 -= 1;
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
        self.entries.get(hash).map(|(_, c)| *c).unwrap_or(0)
    }

    /// Serialise to CBOR. TODO(rewrite-phase-N): replace with §1.5 B+ tree
    /// backing.
    pub fn serialise(&self) -> Result<Vec<u8>, IndexError> {
        let proxy: HashMap<[u8; 32], (BlobRefSerde, u32)> = self
            .entries
            .iter()
            .map(|(h, (b, c))| (*h, ((*b).into(), *c)))
            .collect();
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&proxy, &mut buf)
            .map_err(|e| IndexError::CborEncode(e.to_string()))?;
        Ok(buf)
    }

    /// Deserialise from CBOR.
    pub fn deserialise(bytes: &[u8]) -> Result<Self, IndexError> {
        let proxy: HashMap<[u8; 32], (BlobRefSerde, u32)> =
            ciborium::de::from_reader(bytes).map_err(|e| IndexError::CborDecode(e.to_string()))?;
        Ok(Self {
            entries: proxy
                .into_iter()
                .map(|(h, (b, c))| (h, (b.into(), c)))
                .collect(),
        })
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
