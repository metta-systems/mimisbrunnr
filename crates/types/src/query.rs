use crate::{ObjectId, TagId, Value};

/// Comparison operators for attribute queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Prefix,
    Contains,
}

/// A query over the object space, evaluated via bitmap algebra.
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    /// Objects that have this tag.
    HasTag(TagId),
    /// Objects with an attribute matching a comparison.
    HasAttr { key: TagId, op: CmpOp, value: Value },
    /// Objects related to a target via a predicate.
    Related { predicate: TagId, target: ObjectId },
    /// Intersection of all sub-queries.
    And(Vec<Query>),
    /// Union of all sub-queries.
    Or(Vec<Query>),
    /// Complement of a sub-query.
    Not(Box<Query>),
    /// Ontology-aware: matches tag and all its descendants in the implication DAG.
    IsA(TagId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_tag_query() {
        let q = Query::HasTag(TagId::new(1));
        match &q {
            Query::HasTag(id) => assert_eq!(id.raw(), 1),
            _ => panic!("expected HasTag"),
        }
    }

    #[test]
    fn compound_and_query() {
        let q = Query::And(vec![
            Query::HasTag(TagId::new(1)),
            Query::HasTag(TagId::new(2)),
            Query::HasAttr {
                key: TagId::new(10),
                op: CmpOp::Eq,
                value: Value::Int(2024),
            },
            Query::Not(Box::new(Query::HasTag(TagId::new(3)))),
        ]);
        match &q {
            Query::And(parts) => assert_eq!(parts.len(), 4),
            _ => panic!("expected And"),
        }
    }

    #[test]
    fn or_query() {
        let q = Query::Or(vec![
            Query::HasTag(TagId::new(1)),
            Query::HasTag(TagId::new(2)),
        ]);
        match &q {
            Query::Or(parts) => assert_eq!(parts.len(), 2),
            _ => panic!("expected Or"),
        }
    }

    #[test]
    fn isa_query() {
        let q = Query::IsA(TagId::new(100));
        match &q {
            Query::IsA(id) => assert_eq!(id.raw(), 100),
            _ => panic!("expected IsA"),
        }
    }

    #[test]
    fn related_query() {
        let target = ObjectId::new(1, 42);
        let q = Query::Related {
            predicate: TagId::new(5),
            target,
        };
        match &q {
            Query::Related { predicate, target: t } => {
                assert_eq!(predicate.raw(), 5);
                assert_eq!(*t, target);
            }
            _ => panic!("expected Related"),
        }
    }

    #[test]
    fn all_cmp_ops() {
        let ops = [
            CmpOp::Eq, CmpOp::Ne, CmpOp::Lt, CmpOp::Le,
            CmpOp::Gt, CmpOp::Ge, CmpOp::Prefix, CmpOp::Contains,
        ];
        // Ensure all variants are distinct
        for (i, a) in ops.iter().enumerate() {
            for (j, b) in ops.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }
}
