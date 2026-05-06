//! Faceted exploration helper (DESIGN §5.6).
//!
//! Given a current selection bitmap, returns the set of tags whose
//! intersection with the selection is non-empty along with the cardinality
//! of that intersection. The caller (typically a UI layer) uses the result
//! to render facets.

use {
    mimisbrunnr_types::TagId,
    roaring::RoaringBitmap,
    serde::{Deserialize, Serialize},
};

use crate::tag_index::TagIndex;

/// One row returned by [`FacetedExplorer::facets_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetCount {
    /// Tag id.
    pub tag: TagId,
    /// Cardinality of `selection AND tag.bitmap`.
    pub count: u64,
}

/// Faceted-exploration helper. Currently a thin wrapper over [`TagIndex`];
/// we keep it as a distinct type so the API stays stable as we add an LRU
/// cache (DESIGN §5.6 mentions one).
#[derive(Debug, Default, Clone, Copy)]
pub struct FacetedExplorer;

impl FacetedExplorer {
    /// Return one [`FacetCount`] per tag whose bitmap has a non-empty
    /// intersection with `selection`. Sorted by descending count, then
    /// ascending tag id (deterministic order for stable rendering).
    pub fn facets_for(idx: &TagIndex, selection: &RoaringBitmap) -> Vec<FacetCount> {
        let mut out: Vec<FacetCount> = idx
            .iter()
            .filter_map(|(tag, store)| {
                let inter = selection & store.members();
                let count = inter.len();
                if count == 0 {
                    None
                } else {
                    Some(FacetCount { tag: *tag, count })
                }
            })
            .collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
        out
    }
}

#[cfg(test)]
mod tests {
    use {super::*, mimisbrunnr_types::ObjectId};

    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0, local)
    }
    fn t(id: u32) -> TagId {
        TagId::new(id)
    }

    #[test]
    fn facets_for_overlapping_tags() {
        let mut idx = TagIndex::new();
        // electronics: {1, 2, 3, 4}
        for i in 1..=4u64 {
            idx.add_member(t(1), oid(i));
        }
        // portable: {2, 3, 5}
        for i in [2u64, 3, 5] {
            idx.add_member(t(2), oid(i));
        }
        // pink: {99}
        idx.add_member(t(3), oid(99));

        // selection = {2, 3}
        let mut selection = RoaringBitmap::new();
        selection.insert(2);
        selection.insert(3);

        let facets = FacetedExplorer::facets_for(&idx, &selection);
        // tag(1) and tag(2) overlap with selection (2 each); tag(3) does not.
        assert_eq!(facets.len(), 2);
        assert_eq!(facets[0].count, 2);
        assert_eq!(facets[1].count, 2);
        let mut tags: Vec<u32> = facets.iter().map(|f| f.tag.raw()).collect();
        tags.sort();
        assert_eq!(tags, vec![1, 2]);
    }

    #[test]
    fn facets_for_empty_selection() {
        let mut idx = TagIndex::new();
        idx.add_member(t(1), oid(1));
        let selection = RoaringBitmap::new();
        let facets = FacetedExplorer::facets_for(&idx, &selection);
        assert!(facets.is_empty());
    }

    #[test]
    fn facets_sorted_by_descending_count() {
        let mut idx = TagIndex::new();
        // tag 1: {1, 2, 3, 4, 5}
        for i in 1..=5u64 {
            idx.add_member(t(1), oid(i));
        }
        // tag 2: {1, 2}
        idx.add_member(t(2), oid(1));
        idx.add_member(t(2), oid(2));
        // tag 3: {1}
        idx.add_member(t(3), oid(1));

        let mut selection = RoaringBitmap::new();
        for i in 1..=5u32 {
            selection.insert(i);
        }
        let facets = FacetedExplorer::facets_for(&idx, &selection);
        assert_eq!(facets[0].tag, t(1));
        assert_eq!(facets[0].count, 5);
        assert_eq!(facets[1].tag, t(2));
        assert_eq!(facets[1].count, 2);
        assert_eq!(facets[2].tag, t(3));
        assert_eq!(facets[2].count, 1);
    }
}
