//! Tag inverted index — `TagId → TagStore` (DESIGN §5.1, IMPL §8).
//!
//! On disk:
//!
//! - The directory is a §1.5 B+ tree region of [`BtreeKind::TagDirectory`]
//!   with the 2-field packed key `(tag_id, snapshot)` and a 40 B value tail
//!   ([`TagIndexValue`]) per entry. Together with the key bytes the value
//!   reproduces the spec's 48 B [`TagIndexLeafEntry`] image.
//! - The directory entry's `store_root: BlockRef` points at the head
//!   [`TagBitmapPage`](crate::TagBitmapPage) of a singly-linked chain
//!   carrying the tag's roaring bitmap.
//!
//! In memory:
//!
//! - [`TagIndex`] = `HashMap<TagId, TagStore>` per IMPL §13.
//!
//! ## Persistence (R1c-A3.3) — Simple stores only
//!
//! - Directory: native packed sorted run, `value_size_kind = FIXED`,
//!   40 B per value tail.
//! - Bitmap pages: linked chain of 4 KiB `TagBitmapPage`s allocated
//!   sequentially within a per-pool *bitmap area* whose absolute byte
//!   offset is supplied by the engine. Each page holds ≤ 4 036 B of the
//!   tag's roaring portable serialisation; chains terminate at the page
//!   with `next_page == BlockRef::zeroed()`.
//! - Ordered / Ranked stores: flush returns
//!   [`IndexError::UnsupportedStoreKind`]; A3.4 / A3.5 will land the
//!   §8.3 `OrderedStore` / `RankedStore` root-block paths.

use std::collections::HashMap;

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{
        BLOCK_SIZE, BlockDevice, BlockRef, BtreeKind, BtreeRegion, FieldHints, LoadedNode,
        PackError, PackableKey, SortedRun,
    },
    mimisbrunnr_types::{ObjectId, TagId},
    roaring::RoaringBitmap,
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::tag_bitmap_page::{TAG_BITMAP_PAGE_MAX_BYTES, TagBitmapPage};

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

// ---------- TagIndexValue (40 B value tail) ----------

/// Size in bytes of [`TagIndexValue`]'s on-disk byte image (40 = the spec's
/// 48 B [`TagIndexLeafEntry`] minus the 8 B `(tag_id, snapshot)` packed
/// key).
pub const TAG_INDEX_VALUE_SIZE: usize = 40;

/// Fixed-size 40-byte value tail for the [`TagIndex`] B+ tree. Together
/// with the 2-field packed key `(tag_id, snapshot)` (§1.5.6) this
/// reproduces the spec's 48 B [`TagIndexLeafEntry`] layout. Field order
/// matches the leaf entry sans the two key fields.
///
/// ```text
/// [0..8]   last_modify_lsn u64
/// [8..24]  store_root      BlockRef (16 B)   ← head TagBitmapPage
/// [24..28] cardinality     u32  (members count, ≤ u32::MAX)
/// [28..32] generation      u32  (bumped on bitmap rewrite)
/// [32..33] store_kind      u8
/// [33..40] _pad            [u8; 7]
/// ```
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct TagIndexValue {
    pub last_modify_lsn: u64, // [0..8]
    pub store_root: BlockRef, // [8..24]
    pub cardinality: u32,     // [24..28]
    pub generation: u32,      // [28..32]
    pub store_kind: u8,       // [32..33]
    pub _pad: [u8; 7],        // [33..40]
}

const_assert_eq!(core::mem::size_of::<TagIndexValue>(), TAG_INDEX_VALUE_SIZE);

impl AsRef<[u8]> for TagIndexValue {
    fn as_ref(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

impl From<Vec<u8>> for TagIndexValue {
    fn from(v: Vec<u8>) -> Self {
        let mut buf = [0u8; TAG_INDEX_VALUE_SIZE];
        let n = v.len().min(TAG_INDEX_VALUE_SIZE);
        buf[..n].copy_from_slice(&v[..n]);
        *bytemuck::from_bytes(&buf)
    }
}

// `TagIndexValue` needs `Serialize + Deserialize` to satisfy the
// `BtreeRegion::read_packed` bounds (the CBOR fallback path). We always
// write packed runs so the CBOR codec is never exercised for this type.
impl Serialize for TagIndexValue {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_bytes(self.as_ref())
    }
}

impl<'de> Deserialize<'de> for TagIndexValue {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let bytes: Vec<u8> = serde_bytes_helper::deserialize_bytes(de)?;
        Ok(Self::from(bytes))
    }
}

mod serde_bytes_helper {
    use serde::de::{Error, SeqAccess, Visitor};

    pub fn deserialize_bytes<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<u8>, D::Error> {
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
    // R1c-A3.3: §1.5 B+ tree persistence (packed-codec native leaf +
    // chained TagBitmapPages).
    // ----------------------------------------------------------------

    /// Restore the in-memory state from a [`LoadedNode`] of native leaf
    /// entries plus a function that resolves each `store_root` BlockRef
    /// to a [`TagStore::Simple`] bitmap. Tests use this directly; the
    /// production path goes through [`Self::load_from_region`].
    pub fn from_leaf_entries<F>(
        entries: &[(TagIndexKey, TagIndexValue)],
        mut resolve_bitmap: F,
    ) -> Result<Self, IndexError>
    where
        F: FnMut(BlockRef) -> Result<RoaringBitmap, IndexError>,
    {
        let mut stores: HashMap<TagId, TagStore> = HashMap::new();
        for (k, v) in entries {
            // R1c-A3.3 only persists Simple; non-Simple flushes errored at
            // write time, so any on-disk leaf must be Simple.
            if v.store_kind != TagStoreKind::Simple as u8 {
                return Err(IndexError::UnsupportedStoreKind(v.store_kind));
            }
            let bm = resolve_bitmap(v.store_root)?;
            stores.insert(TagId::new(k.tag_id), TagStore::Simple(bm));
        }
        Ok(Self { stores })
    }

    /// Write the in-memory state to disk under the A3.3 layout:
    ///
    /// - `dir_offset` is the byte offset of the §1.5 directory region.
    /// - `bitmap_area_offset` is the byte offset of the start of the
    ///   bitmap-page area; pages are allocated sequentially from slot 0.
    /// - `bitmap_area_cap_pages` is the maximum number of 4 KiB pages
    ///   permitted in the bitmap area; flush errors with
    ///   [`IndexError::BitmapAreaExhausted`] when a chain would overflow it.
    ///
    /// Returns the count of bitmap pages written so callers can record
    /// the live extent.
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        dir_offset: u64,
        bitmap_area_offset: u64,
        bitmap_area_cap_pages: usize,
    ) -> Result<usize, IndexError> {
        // 1. Sort entries by (tag_id, snapshot) so the packed sorted run is
        //    monotonic.
        let mut sorted: Vec<(TagId, &TagStore)> =
            self.stores.iter().map(|(k, v)| (*k, v)).collect();
        sorted.sort_by_key(|(t, _)| t.raw());

        // 2. Reject Ordered/Ranked stores up front (S2: A3.3 ships
        //    Simple-only; A3.4/A3.5 will lift this).
        for (_, store) in &sorted {
            match store {
                TagStore::Simple(_) => {}
                TagStore::Ordered { .. } => {
                    return Err(IndexError::UnsupportedStoreKind(
                        TagStoreKind::Ordered as u8,
                    ));
                }
                TagStore::Ranked { .. } => {
                    return Err(IndexError::UnsupportedStoreKind(TagStoreKind::Ranked as u8));
                }
            }
        }

        // 3. Walk Simple stores, write each bitmap chain into the bitmap
        //    area, and build the corresponding leaf-value record.
        let mut leaves: Vec<(TagIndexKey, TagIndexValue)> = Vec::with_capacity(sorted.len());
        let mut next_page_slot: usize = 0;
        for (tag, store) in sorted {
            let bitmap = match store {
                TagStore::Simple(b) => b,
                _ => unreachable!("validated above"),
            };

            let cardinality = u32::try_from(bitmap.len())
                .map_err(|_| IndexError::CardinalityOverflow(bitmap.len()))?;

            let store_root = write_bitmap_chain(
                device,
                bitmap_area_offset,
                bitmap_area_cap_pages,
                &mut next_page_slot,
                bitmap,
            )?;

            leaves.push((
                TagIndexKey {
                    tag_id: tag.raw(),
                    snapshot: 0,
                },
                TagIndexValue {
                    last_modify_lsn: 0, // R1c stub; engine wires LSN later.
                    store_root,
                    cardinality,
                    generation: 1, // bumped on rewrite (R1c stub).
                    store_kind: TagStoreKind::Simple as u8,
                    _pad: [0; 7],
                },
            ));
        }

        // 4. Write the directory's sorted run via the packed codec.
        let mut node: LoadedNode<TagIndexKey, TagIndexValue> =
            LoadedNode::new(BtreeKind::TagDirectory, 0, REGION_SIZE_LOG2);
        if !leaves.is_empty() {
            let run = SortedRun::from_sorted(0, 0, leaves);
            node.sorted_runs.push(run);
            node.header.sorted_run_count = 1;
        }
        BtreeRegion::write_full::<TagIndexKey, TagIndexValue>(device, dir_offset, &mut node)?;

        Ok(next_page_slot)
    }

    /// Read the in-memory state from disk. Reads the directory at
    /// `dir_offset`, then for each leaf entry walks the
    /// [`TagBitmapPage`] chain rooted at `store_root` to reconstruct the
    /// membership bitmap.
    ///
    /// An all-zero directory region is treated as "empty index" and
    /// returns [`Self::default`].
    pub fn load_from_region<D: BlockDevice>(
        device: &mut D,
        dir_offset: u64,
    ) -> Result<Self, IndexError> {
        let mut probe = [0u8; 8];
        device.read_at(dir_offset, &mut probe)?;
        if probe.iter().all(|&b| b == 0) {
            return Ok(Self::default());
        }
        let node =
            BtreeRegion::read_as_loaded_node::<TagIndexKey, TagIndexValue>(device, dir_offset)?;
        let mut entries: Vec<(TagIndexKey, TagIndexValue)> = Vec::new();
        for (k, v) in node.merge_iter() {
            entries.push((*k, *v));
        }
        Self::from_leaf_entries(&entries, |head| read_bitmap_chain(device, head))
    }
}

// ---------- TagBitmapPage chain read/write ----------

/// Serialise `bitmap` to the portable Roaring image and split it across a
/// linked chain of [`TagBitmapPage`] blocks within the bitmap area. Pages
/// are appended at sequential slots starting from `*next_slot`; the chain
/// is terminated by a tail page with `next_page == BlockRef::zeroed()`.
/// Returns the head page's [`BlockRef`].
///
/// `bitmap_area_offset` is the absolute byte offset of slot 0 on `device`;
/// `cap_pages` is the maximum number of slots in the area.
fn write_bitmap_chain<D: BlockDevice>(
    device: &D,
    bitmap_area_offset: u64,
    cap_pages: usize,
    next_slot: &mut usize,
    bitmap: &RoaringBitmap,
) -> Result<BlockRef, IndexError> {
    // 1. Serialise to a contiguous byte image.
    let mut bytes = Vec::with_capacity(bitmap.serialized_size());
    bitmap
        .serialize_into(&mut bytes)
        .map_err(|e| IndexError::Roaring(e.to_string()))?;

    // 2. Split into 4036-byte chunks. The chain is built tail-first so each
    //    page knows its successor's BlockRef at write time.
    let chunks: Vec<&[u8]> = bytes.chunks(TAG_BITMAP_PAGE_MAX_BYTES).collect();
    let chunks = if chunks.is_empty() {
        // Empty bitmap → single page carrying zero bytes (decoder rebuilds
        // an empty roaring image).
        vec![&bytes[..]]
    } else {
        chunks
    };

    let needed_end = *next_slot + chunks.len();
    if needed_end > cap_pages {
        return Err(IndexError::BitmapAreaExhausted {
            needed: needed_end,
            cap: cap_pages,
        });
    }

    // 3. Assign slot indices left-to-right (head first) so the head sits at
    //    the lowest slot; then write tail-first to fill `next_page` links.
    let first_slot = *next_slot;
    let assigned: Vec<usize> = (first_slot..first_slot + chunks.len()).collect();
    *next_slot = first_slot + chunks.len();

    let mut next_link = BlockRef::zeroed();
    // Walk chain in reverse: last chunk written first, with next = ZERO.
    for (chunk_idx, &chunk) in chunks.iter().enumerate().rev() {
        let slot = assigned[chunk_idx];
        let byte_offset = bitmap_area_offset + (slot as u64) * BLOCK_SIZE as u64;
        let mut page = TagBitmapPage::from_bytes_chunk(chunk, next_link);
        page.write(device, byte_offset)
            .map_err(|e| IndexError::Roaring(e.to_string()))?;
        next_link = block_ref_at(byte_offset);
    }

    // Head sits at `assigned[0]`.
    let head_offset = bitmap_area_offset + (assigned[0] as u64) * BLOCK_SIZE as u64;
    Ok(block_ref_at(head_offset))
}

/// Walk the [`TagBitmapPage`] chain rooted at `head`, concatenate each
/// page's `bitmap_bytes[..bitmap_len]` slice, and deserialise the result
/// as a roaring bitmap.
fn read_bitmap_chain<D: BlockDevice>(
    device: &D,
    head: BlockRef,
) -> Result<RoaringBitmap, IndexError> {
    let mut buf: Vec<u8> = Vec::new();
    let mut cur = head;
    let zero = BlockRef::zeroed();
    while cur != zero {
        let offset = (cur.block_no as u64) * BLOCK_SIZE as u64;
        let page =
            TagBitmapPage::read(device, offset).map_err(|e| IndexError::Roaring(e.to_string()))?;
        buf.extend_from_slice(page.bitmap_slice());
        cur = page.next_link();
    }
    if buf.is_empty() {
        return Ok(RoaringBitmap::new());
    }
    RoaringBitmap::deserialize_from(buf.as_slice()).map_err(|e| IndexError::Roaring(e.to_string()))
}

/// Construct a `BlockRef` pointing at the 4 KiB block whose byte offset is
/// `byte_offset`. `generation = 1` is the R1c-D1 placeholder; D3 will
/// bump generation per allocation.
fn block_ref_at(byte_offset: u64) -> BlockRef {
    BlockRef {
        disk_id: 0,
        _pad: 0,
        block_no: (byte_offset / BLOCK_SIZE as u64) as u32,
        generation: 1,
    }
}

// ---------- TagIndexKey (B+ tree wire type) ----------

/// B+ tree key for the tag inverted index: `(tag_id, snapshot)` per IMPL
/// §11.2.
///
/// Snapshots are deferred to R6; every key written today carries
/// `snapshot = 0`. The field is on-disk now so R6 won't need a layout break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TagIndexKey {
    pub tag_id: u32,
    pub snapshot: u32,
}

const TAG_INDEX_KEY_HINTS: [FieldHints; 2] = [FieldHints::unsigned(), FieldHints::unsigned()];

impl PackableKey for TagIndexKey {
    fn nr_fields() -> usize {
        2
    }
    fn key_header_bytes() -> usize {
        0
    }
    fn field_hints() -> &'static [FieldHints] {
        &TAG_INDEX_KEY_HINTS
    }
    fn field_values(&self, out: &mut [u64]) {
        out[0] = self.tag_id as u64;
        out[1] = self.snapshot as u64;
    }
    fn from_components(_header: u32, fields: &[u64]) -> Result<Self, PackError> {
        if fields.len() != 2 {
            return Err(PackError::Malformed("TagIndexKey: wrong field count"));
        }
        if fields[0] > u32::MAX as u64 || fields[1] > u32::MAX as u64 {
            return Err(PackError::Malformed("TagIndexKey: field overflow"));
        }
        Ok(Self {
            tag_id: fields[0] as u32,
            snapshot: fields[1] as u32,
        })
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

    // ----- B+ tree region round-trip (R1c-A3.3 native path) -----

    use {mimisbrunnr_storage::FileBlockDevice, tempfile::TempDir};

    // Test layout: directory at offset 0 (256 KiB §1.5 region); bitmap
    // area starts at 256 KiB with capacity for 32 pages (128 KiB worth).
    // Device is 1 MiB, comfortably more than the layout needs.
    const TEST_DIR_OFFSET: u64 = 0;
    const TEST_BITMAP_AREA_OFFSET: u64 = 256 * 1024;
    const TEST_BITMAP_AREA_PAGES: usize = 64;

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tag_index.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    fn flush(idx: &TagIndex, dev: &FileBlockDevice) -> Result<usize, IndexError> {
        idx.flush_to_region(
            dev,
            TEST_DIR_OFFSET,
            TEST_BITMAP_AREA_OFFSET,
            TEST_BITMAP_AREA_PAGES,
        )
    }

    fn load(dev: &FileBlockDevice) -> Result<TagIndex, IndexError> {
        TagIndex::load_from_region(dev, TEST_DIR_OFFSET)
    }

    #[test]
    fn tag_region_round_trip_empty_returns_default() {
        let (_dir, dev) = fresh_device();
        let idx = load(&dev).unwrap();
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
        let pages = flush(&idx, &dev).unwrap();
        assert_eq!(pages, 10, "one bitmap page per tag (small bitmaps)");
        let back = load(&dev).unwrap();
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
        // Single-entry runs round-trip post-C1 (the prefix bytes are
        // persisted in the descriptor; no template needed).
        let (_dir, dev) = fresh_device();
        let mut idx = TagIndex::new();
        idx.add_member(t(42), oid(100));
        idx.add_member(t(42), oid(200));
        flush(&idx, &dev).unwrap();
        let back = load(&dev).unwrap();
        assert_eq!(back.tag_count(), 1);
        assert!(back.contains(t(42), oid(100)));
        assert!(back.contains(t(42), oid(200)));
    }

    #[test]
    fn tag_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = TagIndex::new();
        first.add_member(t(1), oid(10));
        first.add_member(t(2), oid(20));
        flush(&first, &dev).unwrap();

        let mut second = TagIndex::new();
        second.add_member(t(99), oid(900));
        flush(&second, &dev).unwrap();

        let back = load(&dev).unwrap();
        assert_eq!(back.tag_count(), 1);
        assert!(back.contains(t(99), oid(900)));
        assert!(back.get(t(1)).is_none());
    }

    #[test]
    fn flush_rejects_ordered_store() {
        let (_dir, dev) = fresh_device();
        let mut idx = TagIndex::new();
        idx.add_member(t(1), oid(10));
        idx.upgrade_to_ordered(t(1)).unwrap();
        let err = flush(&idx, &dev).unwrap_err();
        assert!(matches!(
            err,
            IndexError::UnsupportedStoreKind(k) if k == TagStoreKind::Ordered as u8
        ));
    }

    #[test]
    fn flush_rejects_ranked_store() {
        let (_dir, dev) = fresh_device();
        let mut idx = TagIndex::new();
        idx.add_member(t(1), oid(10));
        idx.upgrade_to_ranked(t(1)).unwrap();
        let err = flush(&idx, &dev).unwrap_err();
        assert!(matches!(
            err,
            IndexError::UnsupportedStoreKind(k) if k == TagStoreKind::Ranked as u8
        ));
    }

    #[test]
    fn single_page_bitmap_has_zero_next_link() {
        // Tag with 100 members → small bitmap → single page → next_page
        // must be BlockRef::zeroed().
        let (_dir, dev) = fresh_device();
        let mut idx = TagIndex::new();
        for m in 0u64..100 {
            idx.add_member(t(7), oid(m));
        }
        flush(&idx, &dev).unwrap();
        // Read the head page directly: it's at the start of the bitmap area.
        let page = TagBitmapPage::read(&dev, TEST_BITMAP_AREA_OFFSET).unwrap();
        assert_eq!(page.next_link(), BlockRef::zeroed());
    }

    #[test]
    fn multi_page_bitmap_chain() {
        // Force a bitmap large enough to need multiple pages. A dense
        // population produces a roaring image around 8 KiB+ for ~50k oids.
        let (_dir, dev) = fresh_device();
        let mut idx = TagIndex::new();
        for m in 0u64..50_000 {
            idx.add_member(t(1), oid(m));
        }
        let pages = flush(&idx, &dev).unwrap();
        assert!(pages >= 2, "expected multi-page chain, got {pages}");

        // Walk the chain manually to verify linkage.
        let mut chain_len = 0usize;
        let mut cur = BlockRef {
            disk_id: 0,
            _pad: 0,
            block_no: (TEST_BITMAP_AREA_OFFSET / BLOCK_SIZE as u64) as u32,
            generation: 1,
        };
        let zero = BlockRef::zeroed();
        while cur != zero {
            let page =
                TagBitmapPage::read(&dev, (cur.block_no as u64) * BLOCK_SIZE as u64).unwrap();
            chain_len += 1;
            cur = page.next_link();
            assert!(chain_len <= TEST_BITMAP_AREA_PAGES, "infinite loop guard");
        }
        assert_eq!(chain_len, pages);

        // Round-trip preserves membership.
        let back = load(&dev).unwrap();
        for m in 0u64..50_000 {
            assert!(back.contains(t(1), oid(m)), "missing member {m}");
        }
    }

    #[test]
    fn bitmap_area_exhausted_errors() {
        let (_dir, dev) = fresh_device();
        let mut idx = TagIndex::new();
        // Each tag with even one member writes 1 page. Cap is small.
        for tag_no in 0..(TEST_BITMAP_AREA_PAGES + 1) as u32 {
            idx.add_member(t(tag_no), oid(0));
        }
        let err = idx
            .flush_to_region(
                &dev,
                TEST_DIR_OFFSET,
                TEST_BITMAP_AREA_OFFSET,
                TEST_BITMAP_AREA_PAGES,
            )
            .unwrap_err();
        assert!(matches!(err, IndexError::BitmapAreaExhausted { .. }));
    }

    #[test]
    fn on_disk_directory_uses_packed_codec() {
        use mimisbrunnr_storage::{SORTED_RUN_FLAG_PACKED_KEYS, SortedRunHeader};
        let (_dir, dev) = fresh_device();
        let mut idx = TagIndex::new();
        idx.add_member(t(1), oid(10));
        idx.add_member(t(2), oid(20));
        flush(&idx, &dev).unwrap();

        // Read the first sorted-run header off disk (offset = dir_offset
        // + BLOCK_SIZE), assert the packed-keys flag is set.
        let mut run_header_buf = [0u8; std::mem::size_of::<SortedRunHeader>()];
        dev.read_at(TEST_DIR_OFFSET + BLOCK_SIZE as u64, &mut run_header_buf)
            .unwrap();
        let run_header: SortedRunHeader = *bytemuck::from_bytes(&run_header_buf);
        assert!(({ run_header.flags } & SORTED_RUN_FLAG_PACKED_KEYS) != 0);
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
        flush(&idx, &dev).unwrap();
        let back = load(&dev).unwrap();
        assert_eq!(back.tag_count(), 50);
        for i in 0u32..50 {
            assert!(back.contains(t(i), oid(i as u64 * 100)));
        }
    }
}
