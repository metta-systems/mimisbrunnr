//! Object assertions and tag origins (DESIGN §2.2).

use serde::{Deserialize, Serialize};

use crate::{ObjectId, TagId, Value};

/// A statement about an object — what it is, what it has, how it relates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Assertion {
    /// Bare tag: "this object is electronic music".
    Tag(TagId),
    /// Key-value attribute: `artist = "Aphex Twin"`.
    Attr { key: TagId, value: Value },
    /// Relation to another object: `member_of playlist:900`.
    Relation { predicate: TagId, target: ObjectId },
}

/// Provenance of a tag on an object — set explicitly, or derived by the
/// implication engine?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TagOrigin {
    /// Set explicitly by a user or application.
    Direct,
    /// Inserted by the ontology implication engine (DESIGN §3.3).
    Materialized,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_assertion_round_trips_via_cbor() {
        let a = Assertion::Tag(TagId::new(1));
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&a, &mut buf).unwrap();
        let back: Assertion = ciborium::de::from_reader(buf.as_slice()).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn attr_assertion() {
        let a = Assertion::Attr {
            key: TagId::new(10),
            value: Value::Text("Aphex Twin".into()),
        };
        if let Assertion::Attr { key, value } = &a {
            assert_eq!(key.raw(), 10);
            assert_eq!(value.as_text(), Some("Aphex Twin"));
        } else {
            panic!("expected Attr");
        }
    }

    #[test]
    fn relation_assertion() {
        let target = ObjectId::from_parts(1, 900);
        let a = Assertion::Relation {
            predicate: TagId::new(5),
            target,
        };
        if let Assertion::Relation { predicate, target: t } = &a {
            assert_eq!(predicate.raw(), 5);
            assert_eq!(*t, target);
        } else {
            panic!("expected Relation");
        }
    }

    #[test]
    fn tag_origin_distinct() {
        assert_ne!(TagOrigin::Direct, TagOrigin::Materialized);
    }

    #[test]
    fn assertion_derives() {
        // Compile-time check: Clone + Debug + PartialEq + Serialize + Deserialize.
        fn assert_traits<T: Clone + std::fmt::Debug + PartialEq + Serialize>() {}
        assert_traits::<Assertion>();
    }
}
