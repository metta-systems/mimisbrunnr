//! Faceted exploration on top of [`crate::QueryExecutor`].
//!
//! Wraps the index crate's `FacetedExplorer` so callers can drive facets
//! from a [`Query`] (rather than a pre-built bitmap). Evaluates the base
//! query, then ranks every tag by `|selection ∩ tag.bitmap|` and returns
//! the top `max_facets`.

use {
    mimisbrunnr_index::FacetedExplorer as IndexFacetedExplorer,
    mimisbrunnr_types::{Query, TagId},
};

use crate::{error::QueryError, executor::QueryExecutor};

/// One facet row: the tag and the cardinality of its intersection with the
/// base selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FacetGroup {
    /// The tag id.
    pub tag: TagId,
    /// `|selection ∩ tag.bitmap|`.
    pub count: u64,
}

/// Faceted-exploration helper. Wraps [`QueryExecutor`] with the
/// "evaluate-then-facet" idiom.
pub struct FacetedExplorer<'a> {
    executor: &'a QueryExecutor<'a>,
}

impl<'a> FacetedExplorer<'a> {
    /// New explorer bound to `executor`.
    pub fn new(executor: &'a QueryExecutor<'a>) -> Self {
        Self { executor }
    }

    /// Evaluate `base`, then return the top `max_facets` tag rows by
    /// `|selection ∩ tag.bitmap|`, descending. Tags with no overlap are
    /// omitted. Ties break by ascending tag id (matches the index crate's
    /// helper).
    pub fn explore(
        &self,
        base: &Query,
        max_facets: usize,
    ) -> Result<Vec<FacetGroup>, QueryError> {
        let selection = self.executor.evaluate(base)?;
        let rows = IndexFacetedExplorer::facets_for(self.executor.tag_index, &selection);
        Ok(rows
            .into_iter()
            .take(max_facets)
            .map(|f| FacetGroup {
                tag: f.tag,
                count: f.count,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        mimisbrunnr_index::{ForwardIndex, KvIndex, RangeIndex, TagIndex},
        mimisbrunnr_ontology::OntologyState,
        mimisbrunnr_types::{ObjectId, Query, TagId},
    };

    fn t(id: u32) -> TagId {
        TagId::new(id)
    }
    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0, local)
    }

    #[test]
    fn explore_orders_by_count_desc() {
        let mut tag = TagIndex::new();
        // electronics: 1..=5
        for i in 1..=5u64 {
            tag.add_member(t(1), oid(i));
        }
        // portable: 2,3
        tag.add_member(t(2), oid(2));
        tag.add_member(t(2), oid(3));
        // favourite: 3
        tag.add_member(t(3), oid(3));

        let kv = KvIndex::new();
        let range = RangeIndex::new();
        let fwd = ForwardIndex::new();
        let ont = OntologyState::new();
        let exec = QueryExecutor::new(&tag, &kv, &range, &fwd, &ont);

        let explorer = FacetedExplorer::new(&exec);
        let groups = explorer.explore(&Query::HasTag(t(1)), 10).unwrap();

        // Selection = {1,2,3,4,5}.
        // electronics ∩ sel = 5; portable ∩ sel = 2; favourite ∩ sel = 1.
        assert_eq!(groups[0].tag, t(1));
        assert_eq!(groups[0].count, 5);
        assert_eq!(groups[1].tag, t(2));
        assert_eq!(groups[1].count, 2);
        assert_eq!(groups[2].tag, t(3));
        assert_eq!(groups[2].count, 1);
    }

    #[test]
    fn explore_respects_max_facets() {
        let mut tag = TagIndex::new();
        for i in 1..=10u64 {
            tag.add_member(t(1), oid(i));
        }
        tag.add_member(t(2), oid(1));
        tag.add_member(t(3), oid(1));
        tag.add_member(t(4), oid(1));

        let kv = KvIndex::new();
        let range = RangeIndex::new();
        let fwd = ForwardIndex::new();
        let ont = OntologyState::new();
        let exec = QueryExecutor::new(&tag, &kv, &range, &fwd, &ont);
        let explorer = FacetedExplorer::new(&exec);

        let groups = explorer.explore(&Query::HasTag(t(1)), 2).unwrap();
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn explore_empty_selection() {
        let tag = TagIndex::new();
        let kv = KvIndex::new();
        let range = RangeIndex::new();
        let fwd = ForwardIndex::new();
        let ont = OntologyState::new();
        let exec = QueryExecutor::new(&tag, &kv, &range, &fwd, &ont);
        let explorer = FacetedExplorer::new(&exec);
        let groups = explorer.explore(&Query::HasTag(t(99)), 5).unwrap();
        assert!(groups.is_empty());
    }
}
