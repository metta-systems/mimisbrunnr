//! Query algebra (DESIGN §2.3). Pure data — the executor lives in
//! `mimisbrunnr-query`.

use serde::{Deserialize, Serialize};

use crate::{ObjectId, TagId, Value};

/// Comparison operators for [`Query::HasAttr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Lexicographic prefix match (text only).
    Prefix,
    /// Substring match (text only).
    Contains,
}

/// A query over the object space, evaluated via bitmap algebra.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Query {
    /// Objects that bear this tag.
    HasTag(TagId),
    /// Objects whose attribute `key` compares against `value` via `op`.
    HasAttr { key: TagId, op: CmpOp, value: Value },
    /// Objects related to `target` via `predicate`.
    Related { predicate: TagId, target: ObjectId },
    /// Intersection.
    And(Vec<Query>),
    /// Union.
    Or(Vec<Query>),
    /// Complement.
    Not(Box<Query>),
    /// Ontology-aware: matches the tag and all its descendants in the
    /// implication DAG.
    IsA(TagId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_derives() {
        // Compile-time check the contract-mandated derives.
        fn assert_traits<T: Clone + std::fmt::Debug + PartialEq + Serialize>() {}
        assert_traits::<Query>();
        assert_traits::<CmpOp>();
    }

    #[test]
    fn compound_query_serde_round_trip() {
        let q = Query::And(vec![
            Query::HasTag(TagId::new(1)),
            Query::HasAttr {
                key: TagId::new(10),
                op: CmpOp::Eq,
                value: Value::Int(2024),
            },
            Query::Not(Box::new(Query::HasTag(TagId::new(3)))),
            Query::IsA(TagId::new(99)),
            Query::Or(vec![
                Query::HasTag(TagId::new(20)),
                Query::Related {
                    predicate: TagId::new(7),
                    target: ObjectId::from_parts(1, 42),
                },
            ]),
        ]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&q, &mut buf).unwrap();
        let back: Query = ciborium::de::from_reader(buf.as_slice()).unwrap();
        assert_eq!(q, back);
    }

    #[test]
    fn cmp_op_variants_distinct() {
        let ops = [
            CmpOp::Eq, CmpOp::Ne, CmpOp::Lt, CmpOp::Le,
            CmpOp::Gt, CmpOp::Ge, CmpOp::Prefix, CmpOp::Contains,
        ];
        for (i, a) in ops.iter().enumerate() {
            for (j, b) in ops.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }
}
