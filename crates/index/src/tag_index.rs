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
//!
//! ## Persistence (R1b-3)
//!
//! On disk the tag-directory level occupies one 256 KiB §1.5 B+ tree
//! region of [`BtreeKind::TagDirectory`]. The in-memory mirror is
//! materialised into a single CBOR-encoded sorted run via
//! [`BtreeRegion::write_full`]; reload goes through [`BtreeRegion::read`].
//! Keys are raw `TagId` (`u32`) sorted ascending; values are the per-tag
//! [`TagStore`] (variable-shape — Simple bitmap, Ordered = bitmap +
//! sequence, Ranked = bitmap + scored entries; plus the embedded
//! roaring bitmap is itself variable-length).
//!
//! Variable-shape values preclude the §1.5.6 packed-key codec: it
//! requires every entry's value to share the same byte length. Until
//! R1a-pack-2 grows variable-size value support, TagIndex flushes
//! through the CBOR run codec.
//!
//! TODO(rewrite-phase-R1d): once R1c lands variable-value-size support
//! and the storage layer offers the §8.1 `TagIndexLeafEntry` / §8.2
//! `TagBitmapPage` / §8.3 `SequencePage`/`RankedPage` block-framing path,
//! switch to native encoding so the per-tag bitmap pages live in their
//! own 4 KiB blocks (rather than inline in the directory's CBOR payload).

use std::collections::HashMap;

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{BlockDevice, BlockRef, BtreeKind, BtreeRegion, LoadedNode, SortedRun},
    mimisbrunnr_types::{ObjectId, TagId},
    roaring::RoaringBitmap,
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::{error::IndexError, tag_store::TagStore};

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // consumed by engine layout once R1b lands engine-side.
pub const TAG_INDEX_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

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

    // ----------------------------------------------------------------
    // R1b-2: §1.5 B+ tree persistence (CBOR-encoded sorted run).
    // ----------------------------------------------------------------

    /// Build a [`LoadedNode`] containing every entry as a single CBOR
    /// sorted run, sorted by `(tag_id, snapshot)`. The node uses
    /// [`BtreeKind::TagDirectory`] and the spec's 18-bit (256 KiB) region
    /// size.
    ///
    /// Per IMPL §11.2 the on-disk key is `(tag_id, snapshot)`; snapshots
    /// are deferred to R6 so every key written today carries `snapshot = 0`.
    /// The field is on-disk now so the layout doesn't break when R6 lands.
    pub fn to_loaded_node(&self) -> LoadedNode<TagIndexKey, TagStore> {
        let mut entries: Vec<(TagIndexKey, TagStore)> = self
            .stores
            .iter()
            .map(|(k, v)| {
                (
                    TagIndexKey {
                        tag_id: k.raw(),
                        snapshot: 0,
                    },
                    v.clone(),
                )
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let mut node: LoadedNode<TagIndexKey, TagStore> =
            LoadedNode::new(BtreeKind::TagDirectory, 0, REGION_SIZE_LOG2);
        if !entries.is_empty() {
            let run = SortedRun::from_sorted(0, 0, entries);
            node.sorted_runs.push(run);
            node.header.sorted_run_count = 1;
        }
        node
    }

    /// Restore the in-memory state from a [`LoadedNode`] read via
    /// [`BtreeRegion::read`].
    pub fn from_loaded_node(node: &LoadedNode<TagIndexKey, TagStore>) -> Result<Self, IndexError> {
        let mut stores: HashMap<TagId, TagStore> = HashMap::new();
        for (k, v) in node.merge_iter() {
            // Snapshot != 0 won't appear until R6 lands snapshot-aware reads.
            stores.insert(TagId::new(k.tag_id), v.clone());
        }
        Ok(Self { stores })
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
        BtreeRegion::write_full::<D, TagIndexKey, TagStore>(device, offset, &mut node)?;
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
        let node = BtreeRegion::read::<D, TagIndexKey, TagStore>(
            device,
            offset,
            BtreeKind::TagDirectory,
        )?;
        Self::from_loaded_node(&node)
    }
}

// ---------- TagIndexKey (B+ tree wire type) ----------

/// B+ tree key for the tag inverted index: `(tag_id, snapshot)` per IMPL
/// §11.2.
///
/// Snapshots are deferred to R6; every key written today carries
/// `snapshot = 0`. The field is on-disk now so R6 won't need a layout break.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct TagIndexKey {
    pub tag_id: u32,
    pub snapshot: u32,
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

    // ----- B+ tree region round-trip (R1b-3) -----

    use mimisbrunnr_storage::FileBlockDevice;
    use tempfile::TempDir;

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tag_index.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    #[test]
    fn tag_region_round_trip_empty_returns_default() {
        let (_dir, dev) = fresh_device();
        let idx = TagIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(idx.tag_count(), 0);
    }

    #[test]
    fn tag_region_round_trip_simple_stores() {
        let (_dir, dev) = fresh_device();
        let mut idx = TagIndex::new();
        for tag_no in 1u32..=10 {
            for member in 0u64..(tag_no as u64) {
                idx.add_member(t(tag_no), oid(tag_no as u64 * 100 + member));
            }
        }
        idx.flush_to_region(&dev, 0).unwrap();
        let back = TagIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.tag_count(), 10);
        for tag_no in 1u32..=10 {
            for member in 0u64..(tag_no as u64) {
                assert!(
                    back.contains(t(tag_no), oid(tag_no as u64 * 100 + member)),
                    "tag {tag_no} member {member}",
                );
            }
        }
    }

    #[test]
    fn tag_region_round_trip_single_tag() {
        // CBOR sorted-run path is unaffected by the packed-codec
        // single-entry trap (chunk_index R1c TODO).
        let (_dir, dev) = fresh_device();
        let mut idx = TagIndex::new();
        idx.add_member(t(42), oid(100));
        idx.add_member(t(42), oid(200));
        idx.flush_to_region(&dev, 0).unwrap();
        let back = TagIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.tag_count(), 1);
        assert!(back.contains(t(42), oid(100)));
        assert!(back.contains(t(42), oid(200)));
    }

    #[test]
    fn tag_region_round_trip_preserves_ordered_and_ranked_kinds() {
        let (_dir, dev) = fresh_device();
        let mut idx = TagIndex::new();
        idx.add_member(t(1), oid(10));
        idx.add_member(t(1), oid(11));
        idx.upgrade_to_ordered(t(1)).unwrap();

        idx.add_member(t(2), oid(20));
        idx.upgrade_to_ranked(t(2)).unwrap();

        idx.add_member(t(3), oid(30)); // stays Simple

        idx.flush_to_region(&dev, 0).unwrap();
        let back = TagIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.tag_count(), 3);
        assert!(back.get(t(1)).is_some());
        assert!(back.contains(t(1), oid(10)));
        assert!(back.contains(t(2), oid(20)));
        assert!(back.contains(t(3), oid(30)));
    }

    #[test]
    fn tag_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = TagIndex::new();
        first.add_member(t(1), oid(10));
        first.add_member(t(2), oid(20));
        first.flush_to_region(&dev, 0).unwrap();

        let mut second = TagIndex::new();
        second.add_member(t(99), oid(900));
        second.flush_to_region(&dev, 0).unwrap();

        let back = TagIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.tag_count(), 1);
        assert!(back.contains(t(99), oid(900)));
        assert!(back.get(t(1)).is_none());
    }

    #[test]
    fn tag_loaded_node_round_trip_empty() {
        let idx = TagIndex::new();
        let node = idx.to_loaded_node();
        assert_eq!(node.sorted_runs.len(), 0);
        let back = TagIndex::from_loaded_node(&node).unwrap();
        assert_eq!(back.tag_count(), 0);
    }

    #[test]
    fn tag_loaded_node_keys_carry_zero_snapshot() {
        let mut idx = TagIndex::new();
        idx.add_member(t(7), oid(42));
        let node = idx.to_loaded_node();
        assert_eq!(node.sorted_runs[0].entries[0].0.snapshot, 0);
        assert_eq!(node.sorted_runs[0].entries[0].0.tag_id, 7);
    }

    #[test]
    fn tag_region_kind_mismatch_detected() {
        use mimisbrunnr_storage::BtreeRegion;
        let (_dir, dev) = fresh_device();
        let mut idx = TagIndex::new();
        idx.add_member(t(1), oid(10));
        idx.flush_to_region(&dev, 0).unwrap();
        let res = BtreeRegion::read::<_, TagIndexKey, TagStore>(&dev, 0, BtreeKind::Range);
        assert!(res.is_err());
    }

    #[test]
    fn tag_region_round_trip_50_tags() {
        let (_dir, dev) = fresh_device();
        let mut idx = TagIndex::new();
        for i in 0u32..50 {
            for member in 0u64..3 {
                idx.add_member(t(i), oid(i as u64 * 100 + member));
            }
        }
        idx.flush_to_region(&dev, 0).unwrap();
        let back = TagIndex::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.tag_count(), 50);
        for i in 0u32..50 {
            assert!(back.contains(t(i), oid(i as u64 * 100)));
        }
    }
}
