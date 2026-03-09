use mimisbrunnr_index::TagIndex;
use mimisbrunnr_types::TagId;
use roaring::RoaringBitmap;

/// Faceted exploration: "Given my current result set, what tags exist on
/// the matching objects?"
///
/// Intersects the result bitmap with each candidate tag bitmap.
/// At ~μs per AND, 5000 tags complete in <10ms.
pub struct FacetedExplorer<'a> {
    tag_index: &'a TagIndex,
}

/// A facet: a tag and how many objects in the result set have it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facet {
    pub tag: TagId,
    pub count: u64,
}

impl<'a> FacetedExplorer<'a> {
    pub fn new(tag_index: &'a TagIndex) -> Self {
        Self { tag_index }
    }

    /// Compute facets for a result set. Returns tags sorted by count descending.
    pub fn facets(&self, result_set: &RoaringBitmap) -> Vec<Facet> {
        if result_set.is_empty() {
            return Vec::new();
        }

        let mut facets: Vec<Facet> = self
            .tag_index
            .all_tags()
            .into_iter()
            .filter_map(|tag| {
                let bm = self.tag_index.bitmap(tag)?;
                let count = (result_set & bm).len();
                if count > 0 {
                    Some(Facet { tag, count })
                } else {
                    None
                }
            })
            .collect();

        facets.sort_by(|a, b| b.count.cmp(&a.count));
        facets
    }

    /// Compute facets only for the given candidate tags (more efficient when
    /// you already know which tags to check).
    pub fn facets_for(
        &self,
        result_set: &RoaringBitmap,
        candidates: &[TagId],
    ) -> Vec<Facet> {
        if result_set.is_empty() {
            return Vec::new();
        }

        let mut facets: Vec<Facet> = candidates
            .iter()
            .filter_map(|&tag| {
                let bm = self.tag_index.bitmap(tag)?;
                let count = (result_set & bm).len();
                if count > 0 {
                    Some(Facet { tag, count })
                } else {
                    None
                }
            })
            .collect();

        facets.sort_by(|a, b| b.count.cmp(&a.count));
        facets
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    #[test]
    fn basic_facets() {
        let mut idx = TagIndex::new();
        // electronic: 1,2,3,4,5
        for i in 1..=5 { idx.tag_object(tag(1), i); }
        // portable: 2,3
        idx.tag_object(tag(2), 2);
        idx.tag_object(tag(2), 3);
        // favorite: 3
        idx.tag_object(tag(3), 3);

        let explorer = FacetedExplorer::new(&idx);

        // Result set: {2, 3}
        let mut result = RoaringBitmap::new();
        result.insert(2);
        result.insert(3);

        let facets = explorer.facets(&result);

        // electronic: 2 (both 2 and 3 have it)
        // portable: 2 (both 2 and 3 have it)
        // favorite: 1 (only 3 has it)
        assert!(facets.len() >= 2);

        let electronic_facet = facets.iter().find(|f| f.tag == tag(1)).unwrap();
        assert_eq!(electronic_facet.count, 2);

        let portable_facet = facets.iter().find(|f| f.tag == tag(2)).unwrap();
        assert_eq!(portable_facet.count, 2);

        let fav_facet = facets.iter().find(|f| f.tag == tag(3)).unwrap();
        assert_eq!(fav_facet.count, 1);
    }

    #[test]
    fn facets_sorted_by_count() {
        let mut idx = TagIndex::new();
        // tag1: 1,2,3 (count=3 in result)
        for i in 1..=3 { idx.tag_object(tag(1), i); }
        // tag2: 1 (count=1 in result)
        idx.tag_object(tag(2), 1);
        // tag3: 1,2 (count=2 in result)
        idx.tag_object(tag(3), 1);
        idx.tag_object(tag(3), 2);

        let explorer = FacetedExplorer::new(&idx);
        let mut result = RoaringBitmap::new();
        for i in 1..=3 { result.insert(i); }

        let facets = explorer.facets(&result);
        assert_eq!(facets[0].count, 3);
        assert_eq!(facets[1].count, 2);
        assert_eq!(facets[2].count, 1);
    }

    #[test]
    fn facets_empty_result() {
        let idx = TagIndex::new();
        let explorer = FacetedExplorer::new(&idx);
        let result = RoaringBitmap::new();
        assert!(explorer.facets(&result).is_empty());
    }

    #[test]
    fn facets_for_candidates() {
        let mut idx = TagIndex::new();
        for i in 1..=5 { idx.tag_object(tag(1), i); }
        for i in 1..=3 { idx.tag_object(tag(2), i); }
        idx.tag_object(tag(3), 1);

        let explorer = FacetedExplorer::new(&idx);
        let mut result = RoaringBitmap::new();
        for i in 1..=5 { result.insert(i); }

        // Only ask about tag(2) and tag(3)
        let facets = explorer.facets_for(&result, &[tag(2), tag(3)]);
        assert_eq!(facets.len(), 2);
        assert!(!facets.iter().any(|f| f.tag == tag(1))); // not asked for
    }

    #[test]
    fn facets_excludes_zero_count() {
        let mut idx = TagIndex::new();
        idx.tag_object(tag(1), 10);
        idx.tag_object(tag(2), 20);

        let explorer = FacetedExplorer::new(&idx);
        let mut result = RoaringBitmap::new();
        result.insert(10);

        let facets = explorer.facets(&result);
        // Only tag(1) should appear — tag(2) has no overlap with result
        assert_eq!(facets.len(), 1);
        assert_eq!(facets[0].tag, tag(1));
    }
}
