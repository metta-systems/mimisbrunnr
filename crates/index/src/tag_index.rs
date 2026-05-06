//! Tag inverted index — `TagId → TagStore` (DESIGN §5.1, IMPL §8).
//!
//! On disk:
//!
//! - [`TagIndexLeafEntry`] (48 B, IMPL §8.1) is the directory leaf entry.
//! - The `TagBitmapPage` / `SequencePage` / `RankedPage` block-framing
//!   shapes (IMPL §8.2, §8.3) live inside `BlockHeader`-prefixed 4 KiB
//!   blocks; this crate only owns the bitmap-store metadata and the
//!   roaring portable bytes — the block envelope itself is in
//!   `mimisbrunnr-storage`.
//!
//! In memory:
//!
//! - [`TagIndex`] = `HashMap<TagId, TagStore>` per IMPL §13.

use std::collections::HashMap;

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::BlockRef,
    mimisbrunnr_types::{ObjectId, TagId},
    roaring::RoaringBitmap,
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::{error::IndexError, tag_store::TagStore};

// ---------- Store kind discriminants ----------

/// Discriminants for `TagIndexLeafEntry::store_kind`. IMPL §8.1.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagStoreKind {
    /// Plain bitmap store.
    Simple = 0,
    /// Bitmap + sequence (playlist-like).
    Ordered = 1,
    /// Bitmap + ranked (scored) entries.
    Ranked = 2,
}

impl TagStoreKind {
    /// Decode from a raw byte.
    pub fn from_u8(value: u8) -> Result<Self, IndexError> {
        Ok(match value {
            0 => Self::Simple,
            1 => Self::Ordered,
            2 => Self::Ranked,
            other => return Err(IndexError::InvalidAssertionKind(other)),
        })
    }
}

// ---------- TagIndexLeafEntry ----------

/// Size of [`TagIndexLeafEntry`] in bytes (48). IMPL §8.1.
pub const TAG_INDEX_LEAF_ENTRY_SIZE: usize = 48;

/// On-disk leaf entry for the tag-directory B+ tree (IMPL §8.1).
///
/// Layout (48 bytes):
///
/// ```text
/// [0..8]   last_modify_lsn        u64
/// [8..24]  store_root             BlockRef (16 B)
/// [24..28] tag_id                 u32  (sort key prefix)
/// [28..32] snapshot               u32  (sort key suffix)
/// [32..36] cardinality            u32
/// [36..40] generation             u32
/// [40..41] store_kind             u8
/// [41..48] _pad                   [u8; 7]
/// ```
///
/// `#[repr(C, packed)]` per the rewrite contract §4 (despite IMPL §8.1
/// favouring `#[repr(C)]` for hot-path access — the contract pins the
/// packed form for "every fixed-size on-disk struct"; the size and field
/// offsets are unchanged either way).
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct TagIndexLeafEntry {
    pub last_modify_lsn: u64, // [0..8]
    pub store_root: BlockRef, // [8..24]
    pub tag_id: u32,          // [24..28]
    pub snapshot: u32,        // [28..32]
    pub cardinality: u32,     // [32..36]
    pub generation: u32,      // [36..40]
    pub store_kind: u8,       // [40..41]
    pub _pad: [u8; 7],        // [41..48]
}

const_assert_eq!(
    core::mem::size_of::<TagIndexLeafEntry>(),
    TAG_INDEX_LEAF_ENTRY_SIZE
);
// computed: 8 (last_modify_lsn) + 16 (store_root) + 4 (tag_id) + 4 (snapshot)
// + 4 (cardinality) + 4 (generation) + 1 (store_kind) + 7 (_pad) = 48

// ---------- TagIndex (in-memory mirror) ----------

/// In-memory tag inverted index (IMPL §13).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagIndex {
    stores: HashMap<TagId, TagStore>,
}

impl TagIndex {
    /// New empty tag index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or update) the store for `tag`.
    pub fn insert(&mut self, tag: TagId, store: TagStore) {
        self.stores.insert(tag, store);
    }

    /// Borrow the store for `tag`.
    pub fn get(&self, tag: TagId) -> Option<&TagStore> {
        self.stores.get(&tag)
    }

    /// Mutable borrow.
    pub fn get_mut(&mut self, tag: TagId) -> Option<&mut TagStore> {
        self.stores.get_mut(&tag)
    }

    /// Add `oid` to the membership bitmap of `tag`. Creates the tag with a
    /// `Simple` store if absent.
    pub fn add_member(&mut self, tag: TagId, oid: ObjectId) {
        self.stores
            .entry(tag)
            .or_insert_with(TagStore::new_simple)
            .add_member(oid);
    }

    /// Remove `oid` from `tag`. Returns `true` if it was present.
    pub fn remove_member(&mut self, tag: TagId, oid: ObjectId) -> bool {
        self.stores
            .get_mut(&tag)
            .map(|s| s.remove_member(oid))
            .unwrap_or(false)
    }

    /// `true` iff `oid` carries `tag`.
    pub fn contains(&self, tag: TagId, oid: ObjectId) -> bool {
        self.stores.get(&tag).is_some_and(|s| s.contains(oid))
    }

    /// Borrow the membership bitmap for `tag`. DESIGN §5.1 query primitive.
    pub fn query_simple(&self, tag: TagId) -> Option<&RoaringBitmap> {
        self.stores.get(&tag).map(|s| s.members())
    }

    /// AND of the bitmaps of `tags`. Empty bitmap if any tag is missing.
    pub fn intersect(&self, tags: &[TagId]) -> RoaringBitmap {
        let mut iter = tags.iter().copied();
        let mut acc = match iter.next() {
            Some(t) => match self.query_simple(t) {
                Some(bm) => bm.clone(),
                None => return RoaringBitmap::new(),
            },
            None => return RoaringBitmap::new(),
        };
        for t in iter {
            match self.query_simple(t) {
                Some(bm) => acc &= bm,
                None => return RoaringBitmap::new(),
            }
        }
        acc
    }

    /// OR of the bitmaps of `tags`.
    pub fn union(&self, tags: &[TagId]) -> RoaringBitmap {
        let mut acc = RoaringBitmap::new();
        for t in tags {
            if let Some(bm) = self.query_simple(*t) {
                acc |= bm;
            }
        }
        acc
    }

    /// Promote `tag`'s store from `Simple` to `Ordered`.
    pub fn upgrade_to_ordered(&mut self, tag: TagId) -> Result<(), IndexError> {
        let s = self.stores.get_mut(&tag).ok_or(IndexError::TagNotFound)?;
        s.upgrade_to_ordered();
        Ok(())
    }

    /// Promote `tag`'s store to `Ranked`.
    pub fn upgrade_to_ranked(&mut self, tag: TagId) -> Result<(), IndexError> {
        let s = self.stores.get_mut(&tag).ok_or(IndexError::TagNotFound)?;
        s.upgrade_to_ranked();
        Ok(())
    }

    /// Remove `oid` from every tag's bitmap (used when an object is deleted).
    /// Returns the list of tags it was removed from.
    pub fn remove_object_from_all(&mut self, oid: ObjectId) -> Vec<TagId> {
        let mut removed_from = Vec::new();
        for (tag, store) in &mut self.stores {
            if store.remove_member(oid) {
                removed_from.push(*tag);
            }
        }
        removed_from
    }

    /// Iterator over `(tag, store)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&TagId, &TagStore)> {
        self.stores.iter()
    }

    /// Number of tags in the index.
    pub fn tag_count(&self) -> usize {
        self.stores.len()
    }

    /// All tag ids.
    pub fn all_tags(&self) -> Vec<TagId> {
        self.stores.keys().copied().collect()
    }

    /// Serialise to CBOR.
    ///
    /// `TagIndex` derives `Serialize`/`Deserialize` directly — the
    /// `RoaringBitmap` framing lives in [`crate::TagStore`]'s custom serde
    /// impls (`bitmap_bytes` field, IMPL §13.1). This helper exists for
    /// symmetry with the other indices and to map ciborium errors to
    /// [`IndexError`]; callers may equally reach for `ciborium::ser::into_writer`
    /// directly.
    ///
    /// TODO(rewrite-phase-N): replace with §1.5 B+ tree backing.
    pub fn serialise(&self) -> Result<Vec<u8>, IndexError> {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(self, &mut buf)
            .map_err(|e| IndexError::CborEncode(e.to_string()))?;
        Ok(buf)
    }

    /// Deserialise from CBOR. See the doc on [`Self::serialise`] for why this
    /// helper is retained.
    pub fn deserialise(bytes: &[u8]) -> Result<Self, IndexError> {
        ciborium::de::from_reader(bytes).map_err(|e| IndexError::CborDecode(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0, local)
    }
    fn t(id: u32) -> TagId {
        TagId::new(id)
    }

    #[test]
    fn tag_index_leaf_entry_size_is_48() {
        // computed: 8 + 16 + 4 + 4 + 4 + 4 + 1 + 7 = 48
        assert_eq!(core::mem::size_of::<TagIndexLeafEntry>(), 48);
    }

    #[test]
    fn add_remove_round_trip() {
        let mut idx = TagIndex::new();
        idx.add_member(t(1), oid(10));
        idx.add_member(t(1), oid(20));
        assert_eq!(idx.query_simple(t(1)).unwrap().len(), 2);
        assert!(idx.contains(t(1), oid(10)));
        assert!(idx.remove_member(t(1), oid(10)));
        assert!(!idx.contains(t(1), oid(10)));
        assert_eq!(idx.query_simple(t(1)).unwrap().len(), 1);
    }

    #[test]
    fn intersect_and_union() {
        let mut idx = TagIndex::new();
        idx.add_member(t(1), oid(1));
        idx.add_member(t(1), oid(2));
        idx.add_member(t(1), oid(3));
        idx.add_member(t(2), oid(2));
        idx.add_member(t(2), oid(3));
        idx.add_member(t(2), oid(4));

        let inter = idx.intersect(&[t(1), t(2)]);
        assert_eq!(inter.len(), 2);
        assert!(inter.contains(2));
        assert!(inter.contains(3));

        let uni = idx.union(&[t(1), t(2)]);
        assert_eq!(uni.len(), 4);
    }

    #[test]
    fn upgrade_to_ordered_preserves_members() {
        let mut idx = TagIndex::new();
        idx.add_member(t(1), oid(7));
        idx.add_member(t(1), oid(9));
        idx.upgrade_to_ordered(t(1)).unwrap();
        let s = idx.get(t(1)).unwrap();
        assert!(matches!(s, TagStore::Ordered { .. }));
        assert!(s.contains(oid(7)));
        assert!(s.contains(oid(9)));
    }

    #[test]
    fn remove_object_from_all() {
        let mut idx = TagIndex::new();
        idx.add_member(t(1), oid(5));
        idx.add_member(t(2), oid(5));
        idx.add_member(t(3), oid(5));
        idx.add_member(t(1), oid(6));
        let mut removed = idx.remove_object_from_all(oid(5));
        removed.sort();
        assert_eq!(removed, vec![t(1), t(2), t(3)]);
        assert!(!idx.contains(t(1), oid(5)));
        assert!(idx.contains(t(1), oid(6)));
    }

    #[test]
    fn cbor_round_trip() {
        let mut idx = TagIndex::new();
        idx.add_member(t(1), oid(10));
        idx.add_member(t(2), oid(20));
        idx.upgrade_to_ordered(t(1)).unwrap();
        let bytes = idx.serialise().unwrap();
        let back = TagIndex::deserialise(&bytes).unwrap();
        assert_eq!(back.tag_count(), 2);
        assert!(back.contains(t(1), oid(10)));
        assert!(back.contains(t(2), oid(20)));
    }
}
