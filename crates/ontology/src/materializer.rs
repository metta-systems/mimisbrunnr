use mimisbrunnr_types::{ObjectId, TagId, Assertion, TagOrigin};
use mimisbrunnr_index::{TagIndex, ForwardIndex};

use crate::ImplicationDag;

/// The materializer automatically adds implied tags to objects and indexes.
///
/// When an object is tagged "car", the materializer:
/// 1. Computes the transitive closure: car → vehicle → physical_object
/// 2. Adds materialized tags to the forward index
/// 3. Updates the tag bitmaps
///
/// This trades write-time cost for read-time speed: querying for "vehicle"
/// is a single bitmap lookup instead of needing query expansion.
pub struct Materializer;

impl Materializer {
    /// Materialize all implied tags for an object that just received a direct tag.
    ///
    /// Returns the list of materialized tags that were added.
    pub fn materialize_tag(
        dag: &ImplicationDag,
        tag_index: &mut TagIndex,
        forward_index: &mut ForwardIndex,
        oid: ObjectId,
        direct_tag: TagId,
    ) -> Vec<TagId> {
        let implied = dag.transitive_closure(direct_tag);
        let obj_local = oid.local().value() as u32;
        let mut added = Vec::new();

        for implied_tag in implied {
            // Only add if not already present (avoid duplicating)
            if !tag_index.has_tag(implied_tag, obj_local) {
                tag_index.tag_object(implied_tag, obj_local);
                forward_index.add(
                    oid,
                    Assertion::Tag(implied_tag),
                    TagOrigin::Materialized,
                );
                added.push(implied_tag);
            }
        }

        added
    }

    /// When a new implication is added (e.g., "flac → audio"), materialize it
    /// across all existing objects.
    ///
    /// This is a bitmap OR: `bitmap_audio |= bitmap_flac` — microseconds even
    /// for millions of objects.
    pub fn materialize_implication(
        tag_index: &mut TagIndex,
        from: TagId,
        to: TagId,
    ) {
        if let Some(from_bm) = tag_index.bitmap(from) {
            let from_bm = from_bm.clone();
            tag_index.bitmap_or(to, &from_bm);
        }
    }

    /// When a direct tag is removed from an object, remove any materialized
    /// tags that are no longer justified by any other direct tag.
    pub fn dematerialize_tag(
        dag: &ImplicationDag,
        tag_index: &mut TagIndex,
        forward_index: &mut ForwardIndex,
        oid: ObjectId,
        removed_tag: TagId,
    ) -> Vec<TagId> {
        let obj_local = oid.local().value() as u32;

        // Get all tags that were implied by the removed tag
        let was_implied = dag.transitive_closure(removed_tag);

        // Get all remaining direct tags on this object
        let remaining_direct = forward_index.direct_tags(oid);

        // Compute which implied tags are still justified by remaining direct tags
        let mut still_justified = std::collections::HashSet::new();
        for dt in &remaining_direct {
            for implied in dag.transitive_closure(*dt) {
                still_justified.insert(implied);
            }
        }

        // Remove materialized tags that are no longer justified
        let mut removed = Vec::new();
        for implied_tag in was_implied {
            if !still_justified.contains(&implied_tag) {
                tag_index.untag_object(implied_tag, obj_local);
                forward_index.remove(oid, &Assertion::Tag(implied_tag));
                removed.push(implied_tag);
            }
        }

        removed
    }
}

#[cfg(test)]
mod tests {
    use {super::*, arbitrary_int::u48};
    use crate::tag_def::{TagDefinition, TagSemantics};

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    fn oid(local: u64) -> ObjectId {
        ObjectId::new(0, u48::from_u64(local))
    }

    fn label(id: u32, name: &str) -> TagDefinition {
        TagDefinition::new(tag(id), name, TagSemantics::Label)
    }

    fn setup_vehicle_ontology() -> ImplicationDag {
        let mut dag = ImplicationDag::new();
        dag.register_tag(label(1, "car")).unwrap();
        dag.register_tag(label(2, "truck")).unwrap();
        dag.register_tag(label(3, "vehicle")).unwrap();
        dag.register_tag(label(4, "physical_object")).unwrap();

        dag.add_implication(tag(1), tag(3)).unwrap(); // car → vehicle
        dag.add_implication(tag(2), tag(3)).unwrap(); // truck → vehicle
        dag.add_implication(tag(3), tag(4)).unwrap(); // vehicle → physical_object
        dag
    }

    #[test]
    fn materialize_single_tag() {
        let dag = setup_vehicle_ontology();
        let mut tag_index = TagIndex::new();
        let mut fwd_index = ForwardIndex::new();

        let obj = oid(1);

        // Directly tag object as "car"
        tag_index.tag_object(tag(1), obj.local().value() as u32);
        fwd_index.add(obj, Assertion::Tag(tag(1)), TagOrigin::Direct);

        // Materialize
        let added = Materializer::materialize_tag(
            &dag, &mut tag_index, &mut fwd_index, obj, tag(1),
        );

        // Should have added "vehicle" and "physical_object"
        assert_eq!(added.len(), 2);
        assert!(tag_index.has_tag(tag(3), 1)); // vehicle
        assert!(tag_index.has_tag(tag(4), 1)); // physical_object

        // Forward index should show materialized tags
        let mat_tags = fwd_index.materialized_tags(obj);
        assert_eq!(mat_tags.len(), 2);
    }

    #[test]
    fn materialize_implication_across_objects() {
        let mut tag_index = TagIndex::new();

        // Objects 1, 2, 3 have "flac" tag
        tag_index.tag_object(tag(1), 1);
        tag_index.tag_object(tag(1), 2);
        tag_index.tag_object(tag(1), 3);

        // Object 4 already has "audio" tag
        tag_index.tag_object(tag(2), 4);

        // New implication: flac → audio
        Materializer::materialize_implication(&mut tag_index, tag(1), tag(2));

        // "audio" bitmap should now include objects 1, 2, 3, 4
        assert!(tag_index.has_tag(tag(2), 1));
        assert!(tag_index.has_tag(tag(2), 2));
        assert!(tag_index.has_tag(tag(2), 3));
        assert!(tag_index.has_tag(tag(2), 4));
    }

    #[test]
    fn dematerialize_tag() {
        let dag = setup_vehicle_ontology();
        let mut tag_index = TagIndex::new();
        let mut fwd_index = ForwardIndex::new();

        let obj = oid(1);

        // Tag as "car"
        tag_index.tag_object(tag(1), 1);
        fwd_index.add(obj, Assertion::Tag(tag(1)), TagOrigin::Direct);
        Materializer::materialize_tag(&dag, &mut tag_index, &mut fwd_index, obj, tag(1));

        // Now remove "car"
        tag_index.untag_object(tag(1), 1);
        fwd_index.remove(obj, &Assertion::Tag(tag(1)));

        let removed = Materializer::dematerialize_tag(
            &dag, &mut tag_index, &mut fwd_index, obj, tag(1),
        );

        // Vehicle and physical_object should be removed
        assert_eq!(removed.len(), 2);
        assert!(!tag_index.has_tag(tag(3), 1));
        assert!(!tag_index.has_tag(tag(4), 1));
    }

    #[test]
    fn dematerialize_keeps_justified_tags() {
        let dag = setup_vehicle_ontology();
        let mut tag_index = TagIndex::new();
        let mut fwd_index = ForwardIndex::new();

        let obj = oid(1);

        // Tag as both "car" and "truck"
        tag_index.tag_object(tag(1), 1);
        fwd_index.add(obj, Assertion::Tag(tag(1)), TagOrigin::Direct);
        Materializer::materialize_tag(&dag, &mut tag_index, &mut fwd_index, obj, tag(1));

        tag_index.tag_object(tag(2), 1);
        fwd_index.add(obj, Assertion::Tag(tag(2)), TagOrigin::Direct);
        Materializer::materialize_tag(&dag, &mut tag_index, &mut fwd_index, obj, tag(2));

        // Remove "car" — "vehicle" should remain because "truck" still implies it
        tag_index.untag_object(tag(1), 1);
        fwd_index.remove(obj, &Assertion::Tag(tag(1)));

        let removed = Materializer::dematerialize_tag(
            &dag, &mut tag_index, &mut fwd_index, obj, tag(1),
        );

        // Nothing should be removed — truck still justifies vehicle and physical_object
        assert!(removed.is_empty());
        assert!(tag_index.has_tag(tag(3), 1)); // vehicle still there
        assert!(tag_index.has_tag(tag(4), 1)); // physical_object still there
    }

    #[test]
    fn no_duplicate_materialization() {
        let dag = setup_vehicle_ontology();
        let mut tag_index = TagIndex::new();
        let mut fwd_index = ForwardIndex::new();

        let obj = oid(1);

        // Tag as "car" and materialize
        tag_index.tag_object(tag(1), 1);
        fwd_index.add(obj, Assertion::Tag(tag(1)), TagOrigin::Direct);
        let added1 = Materializer::materialize_tag(
            &dag, &mut tag_index, &mut fwd_index, obj, tag(1),
        );
        assert_eq!(added1.len(), 2);

        // Tag as "truck" and materialize — vehicle is already present, shouldn't duplicate
        tag_index.tag_object(tag(2), 1);
        fwd_index.add(obj, Assertion::Tag(tag(2)), TagOrigin::Direct);
        let added2 = Materializer::materialize_tag(
            &dag, &mut tag_index, &mut fwd_index, obj, tag(2),
        );
        // Vehicle and physical_object already present
        assert!(added2.is_empty());
    }
}
