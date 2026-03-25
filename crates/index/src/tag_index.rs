use std::collections::HashMap;

use {mimisbrunnr_types::TagId, roaring::RoaringBitmap};

use crate::tag_store::TagStore;

/// The tag inverted index: one bitmap per tag.
///
/// This is the core data structure for all tag queries. Each tag maps to a
/// `TagStore` containing a roaring bitmap of object IDs that have that tag.
///
/// Query evaluation uses bitmap algebra:
/// - AND → bitmap intersection
/// - OR → bitmap union
/// - NOT → bitmap complement (against universal set)
#[derive(Clone)]
pub struct TagIndex {
    stores: HashMap<TagId, TagStore>,
}

impl TagIndex {
    pub fn new() -> Self {
        Self {
            stores: HashMap::new(),
        }
    }

    /// Ensure a tag exists in the index with the given store type.
    pub fn ensure_tag(&mut self, tag: TagId, store: TagStore) {
        self.stores.entry(tag).or_insert(store);
    }

    /// Get the store for a tag.
    pub fn get(&self, tag: TagId) -> Option<&TagStore> {
        self.stores.get(&tag)
    }

    /// Get a mutable store for a tag.
    pub fn get_mut(&mut self, tag: TagId) -> Option<&mut TagStore> {
        self.stores.get_mut(&tag)
    }

    /// Add an object to a tag. Creates the tag with a Simple store if it doesn't exist.
    pub fn tag_object(&mut self, tag: TagId, obj_local: u32) {
        self.stores
            .entry(tag)
            .or_insert_with(TagStore::new_simple)
            .insert(obj_local);
    }

    /// Remove an object from a tag.
    pub fn untag_object(&mut self, tag: TagId, obj_local: u32) -> bool {
        if let Some(store) = self.stores.get_mut(&tag) {
            store.remove(obj_local)
        } else {
            false
        }
    }

    /// Check if an object has a tag.
    pub fn has_tag(&self, tag: TagId, obj_local: u32) -> bool {
        self.stores.get(&tag).is_some_and(|s| s.contains(obj_local))
    }

    /// Get the bitmap for a tag (for bitmap algebra operations).
    pub fn bitmap(&self, tag: TagId) -> Option<&RoaringBitmap> {
        self.stores.get(&tag).map(|s| s.bitmap())
    }

    /// Perform AND intersection of multiple tag bitmaps.
    pub fn intersect(&self, tags: &[TagId]) -> RoaringBitmap {
        let mut iter = tags.iter().filter_map(|t| self.bitmap(*t));
        match iter.next() {
            None => RoaringBitmap::new(),
            Some(first) => {
                let mut result = first.clone();
                for bm in iter {
                    result &= bm;
                }
                result
            }
        }
    }

    /// Perform OR union of multiple tag bitmaps.
    pub fn union(&self, tags: &[TagId]) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        for tag in tags {
            if let Some(bm) = self.bitmap(*tag) {
                result |= bm;
            }
        }
        result
    }

    /// Remove an object from ALL tags (used during deletion).
    /// Returns the list of tags the object was removed from.
    pub fn remove_object_from_all(&mut self, obj_local: u32) -> Vec<TagId> {
        let mut removed_from = Vec::new();
        for (tag_id, store) in &mut self.stores {
            if store.remove(obj_local) {
                removed_from.push(*tag_id);
            }
        }
        removed_from
    }

    /// Get all tags that an object has (linear scan — use forward index for O(1)).
    pub fn tags_for_object(&self, obj_local: u32) -> Vec<TagId> {
        self.stores
            .iter()
            .filter(|(_, store)| store.contains(obj_local))
            .map(|(tag, _)| *tag)
            .collect()
    }

    /// Number of tags in the index.
    pub fn tag_count(&self) -> usize {
        self.stores.len()
    }

    /// All tag IDs in the index.
    pub fn all_tags(&self) -> Vec<TagId> {
        self.stores.keys().copied().collect()
    }

    /// OR a bitmap into an existing tag's bitmap (used by materializer).
    pub fn bitmap_or(&mut self, tag: TagId, other: &RoaringBitmap) {
        let store = self.stores.entry(tag).or_insert_with(TagStore::new_simple);
        *store.bitmap_mut() |= other;
    }
}

impl Default for TagIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    #[test]
    fn tag_and_query() {
        let mut idx = TagIndex::new();
        idx.tag_object(tag(1), 10);
        idx.tag_object(tag(1), 20);
        idx.tag_object(tag(1), 30);
        idx.tag_object(tag(2), 20);
        idx.tag_object(tag(2), 30);
        idx.tag_object(tag(2), 40);

        // AND: tag 1 AND tag 2 → {20, 30}
        let result = idx.intersect(&[tag(1), tag(2)]);
        assert_eq!(result.len(), 2);
        assert!(result.contains(20));
        assert!(result.contains(30));
    }

    #[test]
    fn union_query() {
        let mut idx = TagIndex::new();
        idx.tag_object(tag(1), 10);
        idx.tag_object(tag(2), 20);

        let result = idx.union(&[tag(1), tag(2)]);
        assert_eq!(result.len(), 2);
        assert!(result.contains(10));
        assert!(result.contains(20));
    }

    #[test]
    fn has_tag() {
        let mut idx = TagIndex::new();
        idx.tag_object(tag(1), 5);
        assert!(idx.has_tag(tag(1), 5));
        assert!(!idx.has_tag(tag(1), 6));
        assert!(!idx.has_tag(tag(99), 5));
    }

    #[test]
    fn untag_object() {
        let mut idx = TagIndex::new();
        idx.tag_object(tag(1), 5);
        assert!(idx.untag_object(tag(1), 5));
        assert!(!idx.has_tag(tag(1), 5));
        assert!(!idx.untag_object(tag(1), 5)); // already removed
    }

    #[test]
    fn remove_from_all() {
        let mut idx = TagIndex::new();
        idx.tag_object(tag(1), 5);
        idx.tag_object(tag(2), 5);
        idx.tag_object(tag(3), 5);
        idx.tag_object(tag(1), 6);

        let removed = idx.remove_object_from_all(5);
        assert_eq!(removed.len(), 3);
        assert!(!idx.has_tag(tag(1), 5));
        assert!(!idx.has_tag(tag(2), 5));
        assert!(idx.has_tag(tag(1), 6)); // obj 6 unaffected
    }

    #[test]
    fn tags_for_object() {
        let mut idx = TagIndex::new();
        idx.tag_object(tag(1), 10);
        idx.tag_object(tag(2), 10);
        idx.tag_object(tag(3), 10);

        let mut tags = idx.tags_for_object(10);
        tags.sort();
        assert_eq!(tags, vec![tag(1), tag(2), tag(3)]);
    }

    #[test]
    fn intersect_empty() {
        let idx = TagIndex::new();
        let result = idx.intersect(&[tag(1), tag(2)]);
        assert!(result.is_empty());
    }

    #[test]
    fn bitmap_or_materializer() {
        let mut idx = TagIndex::new();
        idx.tag_object(tag(1), 10); // "car" → {10}

        // Materialize: tag 2 ("vehicle") should include all of tag 1 ("car")
        let car_bm = idx.bitmap(tag(1)).unwrap().clone();
        idx.bitmap_or(tag(2), &car_bm);

        assert!(idx.has_tag(tag(2), 10));
    }

    #[test]
    fn large_bitmap_intersection() {
        let mut idx = TagIndex::new();

        // 10K objects with tag 1
        for i in 0..10_000u32 {
            idx.tag_object(tag(1), i);
        }
        // 5K objects with tag 2 (even numbers)
        for i in (0..10_000u32).step_by(2) {
            idx.tag_object(tag(2), i);
        }
        // 2K objects with tag 3 (multiples of 5)
        for i in (0..10_000u32).step_by(5) {
            idx.tag_object(tag(3), i);
        }

        // tag1 AND tag2 AND tag3 → multiples of 10
        let result = idx.intersect(&[tag(1), tag(2), tag(3)]);
        assert_eq!(result.len(), 1000); // 0, 10, 20, ..., 9990
    }
}
