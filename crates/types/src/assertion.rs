use crate::{ObjectId, TagId, Value};

/// A statement about an object — what it is, what it has, how it relates.
#[derive(Debug, Clone, PartialEq)]
pub enum Assertion {
    /// A bare tag: "this object is electronic music"
    Tag(TagId),
    /// A key-value attribute: "artist = Aphex Twin"
    Attr { key: TagId, value: Value },
    /// A relation to another object: "member_of playlist:900"
    Relation { predicate: TagId, target: ObjectId },
}

/// Distinguishes how a tag was applied to an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagOrigin {
    /// Explicitly set by user or application.
    Direct,
    /// Automatically added by the ontology implication engine.
    Materialized,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_assertion() {
        let a = Assertion::Tag(TagId::new(1));
        match &a {
            Assertion::Tag(id) => assert_eq!(id.raw(), 1),
            _ => panic!("expected Tag"),
        }
    }

    #[test]
    fn attr_assertion() {
        let a = Assertion::Attr {
            key: TagId::new(10),
            value: Value::Text("Aphex Twin".into()),
        };
        match &a {
            Assertion::Attr { key, value } => {
                assert_eq!(key.raw(), 10);
                assert_eq!(value.as_text(), Some("Aphex Twin"));
            }
            _ => panic!("expected Attr"),
        }
    }

    #[test]
    fn relation_assertion() {
        let target = ObjectId::new(1, 900);
        let a = Assertion::Relation {
            predicate: TagId::new(5),
            target,
        };
        match &a {
            Assertion::Relation {
                predicate,
                target: t,
            } => {
                assert_eq!(predicate.raw(), 5);
                assert_eq!(*t, target);
            }
            _ => panic!("expected Relation"),
        }
    }

    #[test]
    fn tag_origin_equality() {
        assert_ne!(TagOrigin::Direct, TagOrigin::Materialized);
    }
}
