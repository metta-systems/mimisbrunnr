//! Ontology surface types (DESIGN §3).
//!
//! These describe the *shape* of tag definitions and tag-to-tag relations.
//! The implication DAG, materialiser, and policy resolver live in
//! `mimisbrunnr-ontology`.

use serde::{Deserialize, Serialize};

use crate::{ids::TagId, storage_policy::StoragePolicy};

/// Permitted value type for an `Attribute` tag (DESIGN §3.1). Maps 1:1 to
/// the non-`Scoped` variants of [`crate::Value`]; `Scoped` is transparent
/// to type-checking per IMPL §4.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ValueType {
    Text,
    Int,
    Float,
    Timestamp,
    Blob,
}

/// Semantics carried by a tag (DESIGN §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TagSemantics {
    /// Bare label: `electronic`, `favorite`.
    Label,
    /// Key-value attribute: `artist = "Aphex Twin"`.
    Attribute { value_type: ValueType },
    /// Unordered group (also the only legal context for [`crate::Value::Scoped`]
    /// per IMPL §4.3).
    Grouping,
    /// Ordered collection: playlists, albums, recipe books.
    OrderedCollection { element_constraint: Option<TagId> },
    /// Hierarchical: `location:europe/france/paris`.
    Hierarchical,
}

/// Definition of a single tag in the ontology (DESIGN §3.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TagDefinition {
    pub id: TagId,
    pub name: String,
    pub semantics: TagSemantics,
    pub implies: Vec<TagId>,
    /// Per-tag storage policy (DESIGN §3.5). Most user-facing tags omit
    /// this; only tags meant to drive storage behaviour declare one.
    pub storage: Option<StoragePolicy>,
}

/// Tag-to-tag relation (DESIGN §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TagRelation {
    /// `child` "is-a" `parent`. (`car` ⇒ `vehicle`.)
    ImpliedBy,
    /// Cannot coexist on the same object. (`active` vs `discontinued`.)
    MutuallyExclusive,
    /// Presence demands the other tag be present too. (`usb-c` requires
    /// `electronics`.)
    Requires,
    /// Synonym: `laptop` = `notebook`.
    Alias,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantics_round_trip() {
        for s in [
            TagSemantics::Label,
            TagSemantics::Attribute { value_type: ValueType::Int },
            TagSemantics::Grouping,
            TagSemantics::OrderedCollection { element_constraint: Some(TagId::new(7)) },
            TagSemantics::Hierarchical,
        ] {
            let mut buf = Vec::new();
            ciborium::ser::into_writer(&s, &mut buf).unwrap();
            let back: TagSemantics = ciborium::de::from_reader(buf.as_slice()).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn definition_round_trip() {
        let def = TagDefinition {
            id: TagId::new(42),
            name: "vm-disk".into(),
            semantics: TagSemantics::Label,
            implies: vec![TagId::new(1), TagId::new(2)],
            storage: None,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&def, &mut buf).unwrap();
        let back: TagDefinition = ciborium::de::from_reader(buf.as_slice()).unwrap();
        assert_eq!(def, back);
    }

    #[test]
    fn relation_variants_distinct() {
        let rs = [
            TagRelation::ImpliedBy,
            TagRelation::MutuallyExclusive,
            TagRelation::Requires,
            TagRelation::Alias,
        ];
        for (i, a) in rs.iter().enumerate() {
            for (j, b) in rs.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }
}
