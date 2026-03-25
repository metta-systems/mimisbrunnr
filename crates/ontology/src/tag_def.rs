use mimisbrunnr_types::TagId;

/// What kind of value an attribute tag expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    Text,
    Int,
    Float,
    Timestamp,
    Blob,
}

/// The semantic meaning of a tag — drives storage behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagSemantics {
    /// Simple label: "electronic", "favorite"
    Label,
    /// Key-value: "artist=Aphex Twin"
    Attribute { value_type: ValueType },
    /// Unordered group: "genre:ambient"
    Grouping,
    /// Ordered collection: "playlist:workout", "album:SAW-II"
    OrderedCollection { element_constraint: Option<TagId> },
    /// Hierarchical: "location:europe/france/paris"
    Hierarchical,
}

/// Relationships between tags themselves (not between objects).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagRelation {
    /// "car" implies "vehicle"
    ImpliedBy,
    /// "active" vs "discontinued"
    MutuallyExclusive,
    /// "usb-c" requires "electronics"
    Requires,
    /// "laptop" = "notebook"
    Alias,
}

/// A tag definition in the ontology.
#[derive(Debug, Clone)]
pub struct TagDefinition {
    pub id: TagId,
    pub name: String,
    pub semantics: TagSemantics,
    pub implies: Vec<TagId>,
}

impl TagDefinition {
    pub fn new(id: TagId, name: impl Into<String>, semantics: TagSemantics) -> Self {
        Self {
            id,
            name: name.into(),
            semantics,
            implies: Vec::new(),
        }
    }

    pub fn with_implies(mut self, implies: Vec<TagId>) -> Self {
        self.implies = implies;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_tag() {
        let def = TagDefinition::new(TagId::new(1), "electronic", TagSemantics::Label);
        assert_eq!(def.name, "electronic");
        assert_eq!(def.semantics, TagSemantics::Label);
        assert!(def.implies.is_empty());
    }

    #[test]
    fn attribute_tag() {
        let def = TagDefinition::new(
            TagId::new(2),
            "artist",
            TagSemantics::Attribute {
                value_type: ValueType::Text,
            },
        );
        assert!(matches!(
            def.semantics,
            TagSemantics::Attribute {
                value_type: ValueType::Text
            }
        ));
    }

    #[test]
    fn ordered_collection_tag() {
        let audio_tag = TagId::new(100);
        let def = TagDefinition::new(
            TagId::new(3),
            "playlist",
            TagSemantics::OrderedCollection {
                element_constraint: Some(audio_tag),
            },
        );
        assert!(matches!(
            def.semantics,
            TagSemantics::OrderedCollection {
                element_constraint: Some(_)
            }
        ));
    }

    #[test]
    fn with_implies() {
        let vehicle = TagId::new(10);
        let physical = TagId::new(11);
        let def = TagDefinition::new(TagId::new(1), "car", TagSemantics::Label)
            .with_implies(vec![vehicle, physical]);
        assert_eq!(def.implies, vec![vehicle, physical]);
    }
}
