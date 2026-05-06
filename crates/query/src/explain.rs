//! Tree-shaped pretty-printer for [`mimisbrunnr_types::Query`].
//!
//! Used by `mimir query --explain` to surface the operator structure of a
//! query before execution. The output format is human-readable and *not*
//! machine-parseable — for round-trip parsing use [`crate::to_sexpr`].

use mimisbrunnr_types::{CmpOp, Query};

/// Render `query` as a tree-shaped explain plan.
pub fn explain(query: &Query) -> String {
    let mut out = String::new();
    render(query, 0, &mut out);
    out
}

fn render(query: &Query, depth: usize, out: &mut String) {
    indent(depth, out);
    match query {
        Query::HasTag(t) => out.push_str(&format!("HasTag({})\n", t.raw())),
        Query::IsA(t) => out.push_str(&format!("IsA({})\n", t.raw())),
        Query::HasAttr { key, op, value } => {
            out.push_str(&format!(
                "HasAttr(key={}, op={}, value={})\n",
                key.raw(),
                cmp_to_str(*op),
                value
            ));
        }
        Query::Related { predicate, target } => {
            out.push_str(&format!(
                "Related(predicate={}, target={})\n",
                predicate.raw(),
                target
            ));
        }
        Query::And(qs) => {
            out.push_str(&format!("And [{} children]\n", qs.len()));
            for q in qs {
                render(q, depth + 1, out);
            }
        }
        Query::Or(qs) => {
            out.push_str(&format!("Or [{} children]\n", qs.len()));
            for q in qs {
                render(q, depth + 1, out);
            }
        }
        Query::Not(q) => {
            out.push_str("Not\n");
            render(q, depth + 1, out);
        }
    }
}

fn indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn cmp_to_str(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "=",
        CmpOp::Ne => "!=",
        CmpOp::Lt => "<",
        CmpOp::Le => "<=",
        CmpOp::Gt => ">",
        CmpOp::Ge => ">=",
        CmpOp::Prefix => "prefix",
        CmpOp::Contains => "contains",
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        mimisbrunnr_types::{CmpOp, Query, TagId, Value},
    };

    fn t(id: u32) -> TagId {
        TagId::new(id)
    }

    #[test]
    fn explain_three_levels_deep() {
        // (and (or (tag a) (tag b)) (not (attr year = 2024)))
        let q = Query::And(vec![
            Query::Or(vec![Query::HasTag(t(1)), Query::HasTag(t(2))]),
            Query::Not(Box::new(Query::HasAttr {
                key: t(10),
                op: CmpOp::Eq,
                value: Value::Int(2024),
            })),
        ]);
        let plan = explain(&q);

        // We don't lock the exact text, but we verify the tree shape
        // (indentation depth + each operator name appears).
        assert!(plan.contains("And [2 children]"));
        assert!(plan.contains("Or [2 children]"));
        assert!(plan.contains("HasTag(1)"));
        assert!(plan.contains("HasTag(2)"));
        assert!(plan.contains("Not"));
        assert!(plan.contains("HasAttr"));
        // Indentation: at depth 2 (HasTag inside Or inside And) we expect
        // a 4-space prefix.
        assert!(plan.contains("    HasTag(1)\n"));
        // At depth 1 (Or directly under And) we expect a 2-space prefix.
        assert!(plan.contains("  Or [2 children]\n"));
    }

    #[test]
    fn explain_leaf_no_indent() {
        let q = Query::HasTag(t(7));
        assert_eq!(explain(&q), "HasTag(7)\n");
    }
}
