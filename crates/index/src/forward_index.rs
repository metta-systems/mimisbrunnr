use std::collections::HashMap;

use mimisbrunnr_types::{Assertion, ObjectId, TagId, TagOrigin};

/// Entry in the forward index for a single tag on an object.
#[derive(Debug, Clone, PartialEq)]
pub struct ForwardEntry {
    pub assertion: Assertion,
    pub origin: TagOrigin,
}

/// Forward index: Object → all its assertions.
///
/// Used for:
/// - Listing all tags on an object
/// - Distinguishing direct from materialized tags
/// - Efficient deletion (know exactly which bitmaps to update)
/// - Sync (replicate the full assertion set)
#[derive(Clone)]
pub struct ForwardIndex {
    entries: HashMap<u64, Vec<ForwardEntry>>,
}

impl ForwardIndex {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Add an assertion to an object.
    pub fn add(&mut self, oid: ObjectId, assertion: Assertion, origin: TagOrigin) {
        self.entries
            .entry(oid.raw_value())
            .or_default()
            .push(ForwardEntry { assertion, origin });
    }

    /// Remove a specific assertion from an object.
    pub fn remove(&mut self, oid: ObjectId, assertion: &Assertion) -> bool {
        if let Some(entries) = self.entries.get_mut(&oid.raw_value()) {
            let len_before = entries.len();
            entries.retain(|e| &e.assertion != assertion);
            entries.len() < len_before
        } else {
            false
        }
    }

    /// Get all assertions for an object.
    pub fn get(&self, oid: ObjectId) -> &[ForwardEntry] {
        self.entries
            .get(&oid.raw_value())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get only the tag IDs for an object (both direct and materialized).
    pub fn tag_ids(&self, oid: ObjectId) -> Vec<TagId> {
        self.get(oid)
            .iter()
            .filter_map(|e| match &e.assertion {
                Assertion::Tag(id) => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// Get only direct tags for an object.
    pub fn direct_tags(&self, oid: ObjectId) -> Vec<TagId> {
        self.get(oid)
            .iter()
            .filter(|e| e.origin == TagOrigin::Direct)
            .filter_map(|e| match &e.assertion {
                Assertion::Tag(id) => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// Get only materialized tags for an object.
    pub fn materialized_tags(&self, oid: ObjectId) -> Vec<TagId> {
        self.get(oid)
            .iter()
            .filter(|e| e.origin == TagOrigin::Materialized)
            .filter_map(|e| match &e.assertion {
                Assertion::Tag(id) => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// Remove all assertions for an object (used during deletion).
    pub fn remove_object(&mut self, oid: ObjectId) -> Vec<ForwardEntry> {
        self.entries.remove(&oid.raw_value()).unwrap_or_default()
    }

    /// Number of objects in the forward index.
    pub fn object_count(&self) -> usize {
        self.entries.len()
    }
}

impl Default for ForwardIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use {super::*, arbitrary_int::u48, mimisbrunnr_types::Value};

    fn oid(local: u64) -> ObjectId {
        ObjectId::new(0, u48::from_u64(local))
    }

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    #[test]
    fn add_and_get() {
        let mut fi = ForwardIndex::new();
        fi.add(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        fi.add(oid(1), Assertion::Tag(tag(20)), TagOrigin::Materialized);

        let entries = fi.get(oid(1));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn tag_ids() {
        let mut fi = ForwardIndex::new();
        fi.add(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        fi.add(oid(1), Assertion::Tag(tag(20)), TagOrigin::Materialized);
        fi.add(
            oid(1),
            Assertion::Attr {
                key: tag(30),
                value: Value::Text("hello".into()),
            },
            TagOrigin::Direct,
        );

        let tags = fi.tag_ids(oid(1));
        assert_eq!(tags.len(), 2);
    }

    #[test]
    fn direct_vs_materialized() {
        let mut fi = ForwardIndex::new();
        fi.add(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        fi.add(oid(1), Assertion::Tag(tag(20)), TagOrigin::Materialized);

        assert_eq!(fi.direct_tags(oid(1)), vec![tag(10)]);
        assert_eq!(fi.materialized_tags(oid(1)), vec![tag(20)]);
    }

    #[test]
    fn remove_assertion() {
        let mut fi = ForwardIndex::new();
        fi.add(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        fi.add(oid(1), Assertion::Tag(tag(20)), TagOrigin::Direct);

        assert!(fi.remove(oid(1), &Assertion::Tag(tag(10))));
        assert_eq!(fi.get(oid(1)).len(), 1);
        assert!(!fi.remove(oid(1), &Assertion::Tag(tag(10)))); // already removed
    }

    #[test]
    fn remove_object() {
        let mut fi = ForwardIndex::new();
        fi.add(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        fi.add(oid(1), Assertion::Tag(tag(20)), TagOrigin::Direct);

        let removed = fi.remove_object(oid(1));
        assert_eq!(removed.len(), 2);
        assert_eq!(fi.get(oid(1)).len(), 0);
    }

    #[test]
    fn empty_object() {
        let fi = ForwardIndex::new();
        assert_eq!(fi.get(oid(999)).len(), 0);
        assert_eq!(fi.object_count(), 0);
    }
}
