//! Translates a parsed `sqlparser::ast::Statement` (always a `SELECT` per the
//! parser's gate) into [`mimisbrunnr_types::Query`] plus optional limit,
//! offset, and aggregate metadata.
//!
//! Translation rules (Phase 5a — `docs/REWRITE_CONTRACT.md` §3 + the prompt
//! describing the surface):
//!
//! - `WHERE tag = 'X'` → `Query::HasTag(resolve_tag(X))`.
//! - `WHERE tag IN ('X','Y')` → `Query::Or(vec![HasTag(X), HasTag(Y)])`.
//! - `WHERE attr = lit` → `Query::HasAttr { key, op: Eq, value }`.
//! - `<, <=, >, >=, !=` → `CmpOp::{Lt, Le, Gt, Ge, Ne}`.
//! - `WHERE name LIKE 'foo%'` → `HasAttr { key, op: Prefix, value: Text("foo") }`.
//! - `WHERE name LIKE '%foo'` → `HasAttr { key, op: Contains, value: Text("foo") }`.
//!   The executor surfaces `Contains` as `UnsupportedCmpOp` today; that's its
//!   responsibility, not the planner's.
//! - `AND` / `OR` / `NOT` → `Query::And` / `Or` / `Not`. AND/OR are flattened.
//! - `isa('X')` (function-style) → `Query::IsA(resolve_tag(X))`.
//! - `related('pred', N)` → `Query::Related { predicate, target: ObjectId::from_u64(N) }`.
//! - `LIMIT N` / `OFFSET M` extracted into [`PlannedQuery::limit`] /
//!   [`PlannedQuery::offset`]; not folded into the `Query`.
//! - `SELECT COUNT(*) FROM objects WHERE …` → `Aggregate::Count`,
//!   `Projection::Count`. The executor returns the bitmap; the SQL layer
//!   returns its cardinality.

use mimisbrunnr_ontology::OntologyState;
use mimisbrunnr_types::{CmpOp, ObjectId, Query, TagId, Value};
use sqlparser::ast as sp;

use crate::error::SqlError;

/// Aggregation directive extracted from the `SELECT` projection list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aggregate {
    /// No aggregation — return the full bitmap as object IDs.
    None,
    /// `COUNT(*)` — return only the cardinality of the bitmap.
    Count,
}

/// Projection directive extracted from the `SELECT` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    /// `SELECT *` — emit every matching object.
    Star,
    /// `SELECT COUNT(*)` — emit a single count scalar.
    Count,
}

/// A successful plan — the bitmap-algebra tree plus the post-execution
/// modifiers the SQL surface has to apply itself.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedQuery {
    /// Bitmap-algebra query handed to [`mimisbrunnr_query::QueryExecutor`].
    pub query: Query,
    /// Optional `LIMIT` extracted from the SQL — applied post-bitmap.
    pub limit: Option<usize>,
    /// Optional `OFFSET` extracted from the SQL — applied post-bitmap.
    pub offset: Option<usize>,
    /// Aggregation directive (from the projection list).
    pub aggregate: Aggregate,
    /// Projection directive (from the projection list).
    pub projection: Projection,
}

/// Translates a SQL `SELECT` AST into a [`PlannedQuery`].
pub struct SqlPlanner;

impl SqlPlanner {
    /// Plan `stmt`, resolving tag names through `ontology`.
    pub fn plan(stmt: &sp::Statement, ontology: &OntologyState) -> Result<PlannedQuery, SqlError> {
        let query = match stmt {
            sp::Statement::Query(q) => q,
            other => {
                return Err(SqlError::Unsupported(format!(
                    "expected a SELECT, got: {other}"
                )));
            }
        };
        plan_query(query, ontology)
    }
}

fn plan_query(
    query: &sp::Query,
    ontology: &OntologyState,
) -> Result<PlannedQuery, SqlError> {
    if query.with.is_some() {
        return Err(SqlError::Unsupported("WITH (CTE) clause".into()));
    }
    if query.fetch.is_some() {
        return Err(SqlError::Unsupported("FETCH clause".into()));
    }
    if query.order_by.is_some() {
        return Err(SqlError::Unsupported(
            "ORDER BY (TODO: requires OrderedCollection plumbing)".into(),
        ));
    }

    let select = match query.body.as_ref() {
        sp::SetExpr::Select(s) => s.as_ref(),
        other => {
            return Err(SqlError::Unsupported(format!(
                "set expression: {other}"
            )));
        }
    };

    match &select.group_by {
        sp::GroupByExpr::Expressions(exprs, _modifiers) if exprs.is_empty() => {}
        sp::GroupByExpr::Expressions(_, _) => {
            return Err(SqlError::Unsupported("GROUP BY".into()));
        }
        sp::GroupByExpr::All(_) => {
            return Err(SqlError::Unsupported("GROUP BY ALL".into()));
        }
    }
    if select.having.is_some() {
        return Err(SqlError::Unsupported("HAVING".into()));
    }
    if select.distinct.is_some() {
        return Err(SqlError::Unsupported("SELECT DISTINCT".into()));
    }

    // Validate FROM is exactly `objects` (Phase 5a — single virtual relation).
    validate_from(&select.from)?;

    let (projection, aggregate) = plan_projection(&select.projection)?;
    let where_query = match &select.selection {
        Some(expr) => plan_expr(expr, ontology)?,
        None => {
            // No WHERE → match the universe. We pick a tautology shape: an
            // empty `Or` would match nothing; instead we use `Not(Or([]))` so
            // the executor's `universe()` is returned. (Empty OR returns an
            // empty bitmap; Not against universe inverts it.)
            Query::Not(Box::new(Query::Or(Vec::new())))
        }
    };

    let (limit, offset) = plan_limit_offset(&query.limit_clause)?;

    Ok(PlannedQuery {
        query: where_query,
        limit,
        offset,
        aggregate,
        projection,
    })
}

fn validate_from(from: &[sp::TableWithJoins]) -> Result<(), SqlError> {
    if from.is_empty() {
        return Err(SqlError::Unsupported("missing FROM clause".into()));
    }
    if from.len() != 1 {
        return Err(SqlError::Unsupported(
            "multiple FROM relations (no JOIN support yet)".into(),
        ));
    }
    let twj = &from[0];
    if !twj.joins.is_empty() {
        return Err(SqlError::Unsupported(
            "JOIN (no relational joins yet)".into(),
        ));
    }
    let name = match &twj.relation {
        sp::TableFactor::Table { name, .. } => name.to_string(),
        other => {
            return Err(SqlError::Unsupported(format!(
                "unsupported FROM relation: {other}"
            )));
        }
    };
    if !name.eq_ignore_ascii_case("objects") {
        return Err(SqlError::Unsupported(format!(
            "FROM `{name}` — only the `objects` virtual relation is supported"
        )));
    }
    Ok(())
}

fn plan_projection(
    items: &[sp::SelectItem],
) -> Result<(Projection, Aggregate), SqlError> {
    if items.len() != 1 {
        return Err(SqlError::Unsupported(
            "multi-column projections (only `*` and `COUNT(*)` supported)".into(),
        ));
    }
    match &items[0] {
        sp::SelectItem::Wildcard(_) => Ok((Projection::Star, Aggregate::None)),
        sp::SelectItem::UnnamedExpr(expr) | sp::SelectItem::ExprWithAlias { expr, .. } => {
            if is_count_star(expr) {
                Ok((Projection::Count, Aggregate::Count))
            } else {
                Err(SqlError::Unsupported(format!(
                    "projection `{expr}` (only `*` and `COUNT(*)` supported)"
                )))
            }
        }
        other => Err(SqlError::Unsupported(format!("select item: {other}"))),
    }
}

fn is_count_star(expr: &sp::Expr) -> bool {
    let sp::Expr::Function(func) = expr else {
        return false;
    };
    if !func.name.to_string().eq_ignore_ascii_case("count") {
        return false;
    }
    let sp::FunctionArguments::List(arg_list) = &func.args else {
        return false;
    };
    if arg_list.args.len() != 1 {
        return false;
    }
    matches!(
        arg_list.args[0],
        sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Wildcard)
    )
}

fn plan_limit_offset(
    clause: &Option<sp::LimitClause>,
) -> Result<(Option<usize>, Option<usize>), SqlError> {
    match clause {
        None => Ok((None, None)),
        Some(sp::LimitClause::LimitOffset { limit, offset, .. }) => {
            let l = match limit {
                Some(expr) => Some(expr_to_usize(expr)?),
                None => None,
            };
            let o = match offset {
                Some(sp::Offset { value, .. }) => Some(expr_to_usize(value)?),
                None => None,
            };
            Ok((l, o))
        }
        Some(sp::LimitClause::OffsetCommaLimit { offset, limit }) => {
            Ok((Some(expr_to_usize(limit)?), Some(expr_to_usize(offset)?)))
        }
    }
}

// ---------------------------------------------------------------------------
// WHERE-clause translation
// ---------------------------------------------------------------------------

fn plan_expr(expr: &sp::Expr, ontology: &OntologyState) -> Result<Query, SqlError> {
    match expr {
        sp::Expr::Nested(inner) => plan_expr(inner, ontology),
        sp::Expr::UnaryOp {
            op: sp::UnaryOperator::Not,
            expr: inner,
        } => Ok(Query::Not(Box::new(plan_expr(inner, ontology)?))),
        sp::Expr::BinaryOp { left, op, right } => plan_binary_op(left, op, right, ontology),
        sp::Expr::Like {
            expr,
            pattern,
            negated,
            ..
        } => {
            let column = identifier_name(expr)?;
            let pat = literal_text(pattern)?;
            let q = plan_like(&column, &pat, ontology)?;
            if *negated {
                Ok(Query::Not(Box::new(q)))
            } else {
                Ok(q)
            }
        }
        sp::Expr::InList {
            expr,
            list,
            negated,
        } => {
            let column = identifier_name(expr)?;
            let q = plan_in_list(&column, list, ontology)?;
            if *negated {
                Ok(Query::Not(Box::new(q)))
            } else {
                Ok(q)
            }
        }
        sp::Expr::Function(func) => plan_function(func, ontology),
        other => Err(SqlError::Unsupported(format!(
            "WHERE expression: {other}"
        ))),
    }
}

fn plan_binary_op(
    left: &sp::Expr,
    op: &sp::BinaryOperator,
    right: &sp::Expr,
    ontology: &OntologyState,
) -> Result<Query, SqlError> {
    match op {
        sp::BinaryOperator::And => {
            let l = plan_expr(left, ontology)?;
            let r = plan_expr(right, ontology)?;
            let mut terms = Vec::new();
            flatten_and(l, &mut terms);
            flatten_and(r, &mut terms);
            Ok(Query::And(terms))
        }
        sp::BinaryOperator::Or => {
            let l = plan_expr(left, ontology)?;
            let r = plan_expr(right, ontology)?;
            let mut terms = Vec::new();
            flatten_or(l, &mut terms);
            flatten_or(r, &mut terms);
            Ok(Query::Or(terms))
        }
        sp::BinaryOperator::Eq
        | sp::BinaryOperator::NotEq
        | sp::BinaryOperator::Lt
        | sp::BinaryOperator::LtEq
        | sp::BinaryOperator::Gt
        | sp::BinaryOperator::GtEq => {
            let column = identifier_name(left)?;
            // Special-case `tag = 'X'` to a tag-membership query rather than
            // an attribute equality.
            if column.eq_ignore_ascii_case("tag") {
                if !matches!(op, sp::BinaryOperator::Eq) {
                    return Err(SqlError::Unsupported(format!(
                        "operator `{op}` against `tag` column (only `=` and `IN` supported)"
                    )));
                }
                let name = literal_text(right)?;
                let id = resolve_tag(&name, ontology)?;
                return Ok(Query::HasTag(id));
            }

            let value = expr_to_value(right)?;
            let cmp = match op {
                sp::BinaryOperator::Eq => CmpOp::Eq,
                sp::BinaryOperator::NotEq => CmpOp::Ne,
                sp::BinaryOperator::Lt => CmpOp::Lt,
                sp::BinaryOperator::LtEq => CmpOp::Le,
                sp::BinaryOperator::Gt => CmpOp::Gt,
                sp::BinaryOperator::GtEq => CmpOp::Ge,
                _ => unreachable!(),
            };
            let key = resolve_tag(&column, ontology)?;
            Ok(Query::HasAttr {
                key,
                op: cmp,
                value,
            })
        }
        other => Err(SqlError::Unsupported(format!("binary operator: {other}"))),
    }
}

fn plan_like(
    column: &str,
    pattern: &str,
    ontology: &OntologyState,
) -> Result<Query, SqlError> {
    // Phase 5a: only `'foo%'` (prefix) and `'%foo'` (contains) are accepted.
    // Anything else (mid-string `%`, `_`, escape characters) is rejected.
    let has_lead = pattern.starts_with('%');
    let has_trail = pattern.ends_with('%');
    let middle = if pattern.len() >= 2 {
        &pattern[1..pattern.len() - 1]
    } else {
        ""
    };
    let any_underscore = pattern.contains('_');
    let any_internal_pct = middle.contains('%');

    if any_underscore || any_internal_pct {
        return Err(SqlError::UnsupportedPattern(pattern.into()));
    }

    let key = resolve_tag(column, ontology)?;
    let (op, needle) = match (has_lead, has_trail) {
        (false, true) => (CmpOp::Prefix, &pattern[..pattern.len() - 1]),
        (true, false) => (CmpOp::Contains, &pattern[1..]),
        (true, true) => {
            // `'%foo%'` is also a contains pattern conceptually, but the
            // prompt explicitly lists only the two forms — flag as
            // unsupported so the surface stays small and predictable.
            return Err(SqlError::UnsupportedPattern(pattern.into()));
        }
        (false, false) => {
            // No wildcards at all → equality, which is fine but not LIKE
            // semantics; route to Eq so the planner stays principled.
            return Ok(Query::HasAttr {
                key,
                op: CmpOp::Eq,
                value: Value::Text(pattern.to_string()),
            });
        }
    };

    Ok(Query::HasAttr {
        key,
        op,
        value: Value::Text(needle.to_string()),
    })
}

fn plan_in_list(
    column: &str,
    list: &[sp::Expr],
    ontology: &OntologyState,
) -> Result<Query, SqlError> {
    if column.eq_ignore_ascii_case("tag") {
        let mut terms = Vec::with_capacity(list.len());
        for expr in list {
            let name = literal_text(expr)?;
            let id = resolve_tag(&name, ontology)?;
            terms.push(Query::HasTag(id));
        }
        return Ok(Query::Or(terms));
    }

    let key = resolve_tag(column, ontology)?;
    let mut terms = Vec::with_capacity(list.len());
    for expr in list {
        terms.push(Query::HasAttr {
            key,
            op: CmpOp::Eq,
            value: expr_to_value(expr)?,
        });
    }
    Ok(Query::Or(terms))
}

fn plan_function(func: &sp::Function, ontology: &OntologyState) -> Result<Query, SqlError> {
    let name = func.name.to_string();
    let args = match &func.args {
        sp::FunctionArguments::List(arg_list) => &arg_list.args[..],
        sp::FunctionArguments::None => &[],
        other => {
            return Err(SqlError::Unsupported(format!(
                "function arguments: {other:?}"
            )));
        }
    };

    if name.eq_ignore_ascii_case("isa") {
        if args.len() != 1 {
            return Err(SqlError::Unsupported(format!(
                "isa() takes 1 argument, got {}",
                args.len()
            )));
        }
        let tag_name = arg_as_text(&args[0])?;
        let id = resolve_tag(&tag_name, ontology)?;
        return Ok(Query::IsA(id));
    }

    if name.eq_ignore_ascii_case("related") {
        if args.len() != 2 {
            return Err(SqlError::Unsupported(format!(
                "related() takes 2 arguments, got {}",
                args.len()
            )));
        }
        let predicate_name = arg_as_text(&args[0])?;
        let predicate = resolve_tag(&predicate_name, ontology)?;
        let target_raw = arg_as_int(&args[1])?;
        let target = ObjectId::from_u64(target_raw as u64);
        return Ok(Query::Related { predicate, target });
    }

    Err(SqlError::Unsupported(format!("function `{name}`")))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn flatten_and(q: Query, out: &mut Vec<Query>) {
    match q {
        Query::And(qs) => {
            for sub in qs {
                flatten_and(sub, out);
            }
        }
        other => out.push(other),
    }
}

fn flatten_or(q: Query, out: &mut Vec<Query>) {
    match q {
        Query::Or(qs) => {
            for sub in qs {
                flatten_or(sub, out);
            }
        }
        other => out.push(other),
    }
}

fn identifier_name(expr: &sp::Expr) -> Result<String, SqlError> {
    match expr {
        sp::Expr::Identifier(ident) => Ok(ident.value.clone()),
        sp::Expr::CompoundIdentifier(parts) => parts
            .last()
            .map(|p| p.value.clone())
            .ok_or_else(|| SqlError::Unsupported("empty compound identifier".into())),
        other => Err(SqlError::Unsupported(format!(
            "expected identifier, got: {other}"
        ))),
    }
}

fn literal_text(expr: &sp::Expr) -> Result<String, SqlError> {
    match expr {
        sp::Expr::Value(v) => match &v.value {
            sp::Value::SingleQuotedString(s) | sp::Value::DoubleQuotedString(s) => Ok(s.clone()),
            other => Err(SqlError::Unsupported(format!(
                "expected string literal, got: {other}"
            ))),
        },
        other => Err(SqlError::Unsupported(format!(
            "expected string literal, got: {other}"
        ))),
    }
}

fn expr_to_value(expr: &sp::Expr) -> Result<Value, SqlError> {
    match expr {
        sp::Expr::Value(v) => match &v.value {
            sp::Value::SingleQuotedString(s) | sp::Value::DoubleQuotedString(s) => {
                Ok(Value::Text(s.clone()))
            }
            sp::Value::Number(n, _) => {
                if let Ok(i) = n.parse::<i64>() {
                    Ok(Value::Int(i))
                } else if let Ok(f) = n.parse::<f64>() {
                    Ok(Value::Float(f))
                } else {
                    Err(SqlError::ParseError(format!("invalid number `{n}`")))
                }
            }
            sp::Value::Boolean(b) => Ok(Value::Int(*b as i64)),
            other => Err(SqlError::Unsupported(format!(
                "literal value: {other}"
            ))),
        },
        sp::Expr::UnaryOp {
            op: sp::UnaryOperator::Minus,
            expr: inner,
        } => match expr_to_value(inner)? {
            Value::Int(i) => Ok(Value::Int(-i)),
            Value::Float(f) => Ok(Value::Float(-f)),
            other => Err(SqlError::Unsupported(format!(
                "cannot negate value: {other:?}"
            ))),
        },
        other => Err(SqlError::Unsupported(format!(
            "expected value literal, got: {other}"
        ))),
    }
}

fn expr_to_usize(expr: &sp::Expr) -> Result<usize, SqlError> {
    match expr_to_value(expr)? {
        Value::Int(i) if i >= 0 => Ok(i as usize),
        other => Err(SqlError::Unsupported(format!(
            "LIMIT/OFFSET expects a non-negative integer, got: {other:?}"
        ))),
    }
}

fn arg_as_text(arg: &sp::FunctionArg) -> Result<String, SqlError> {
    match arg {
        sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Expr(expr)) => literal_text(expr),
        other => Err(SqlError::Unsupported(format!(
            "function arg (expected text literal): {other}"
        ))),
    }
}

fn arg_as_int(arg: &sp::FunctionArg) -> Result<i64, SqlError> {
    match arg {
        sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Expr(expr)) => match expr_to_value(expr)? {
            Value::Int(i) => Ok(i),
            other => Err(SqlError::Unsupported(format!(
                "expected integer arg, got: {other:?}"
            ))),
        },
        other => Err(SqlError::Unsupported(format!(
            "function arg (expected int literal): {other}"
        ))),
    }
}

fn resolve_tag(name: &str, ontology: &OntologyState) -> Result<TagId, SqlError> {
    ontology
        .names
        .get(name)
        .copied()
        .ok_or_else(|| SqlError::UnknownTag(name.into()))
}

#[cfg(test)]
mod tests {
    use mimisbrunnr_ontology::{IdAllocator, OntologyModule, OntologyState};
    use mimisbrunnr_types::{TagDefinition, TagSemantics};

    use super::*;
    use crate::parser::SqlParser;

    fn label(name: &str) -> TagDefinition {
        TagDefinition {
            id: TagId::new(0),
            name: name.into(),
            semantics: TagSemantics::Label,
            implies: vec![],
            storage: None,
        }
    }

    fn ontology_with(names: &[&str]) -> OntologyState {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let module = OntologyModule {
            id: "test".into(),
            version: "0.1.0".into(),
            name: "test".into(),
            tags: names.iter().map(|n| label(n)).collect(),
            implications: vec![],
        };
        state.install(module, &mut alloc).unwrap();
        state
    }

    fn plan(sql: &str, ont: &OntologyState) -> Result<PlannedQuery, SqlError> {
        let stmt = SqlParser::parse(sql)?;
        SqlPlanner::plan(&stmt, ont)
    }

    #[test]
    fn tag_eq() {
        let ont = ontology_with(&["electronic"]);
        let p = plan("SELECT * FROM objects WHERE tag = 'electronic'", &ont).unwrap();
        assert_eq!(p.query, Query::HasTag(ont.names["electronic"]));
        assert_eq!(p.projection, Projection::Star);
        assert_eq!(p.aggregate, Aggregate::None);
        assert_eq!(p.limit, None);
        assert_eq!(p.offset, None);
    }

    #[test]
    fn tag_in_list() {
        let ont = ontology_with(&["electronic", "portable"]);
        let p = plan(
            "SELECT * FROM objects WHERE tag IN ('electronic', 'portable')",
            &ont,
        )
        .unwrap();
        let e = ont.names["electronic"];
        let pt = ont.names["portable"];
        assert_eq!(
            p.query,
            Query::Or(vec![Query::HasTag(e), Query::HasTag(pt)])
        );
    }

    #[test]
    fn attr_eq_text() {
        let ont = ontology_with(&["artist"]);
        let p = plan(
            "SELECT * FROM objects WHERE artist = 'Aphex Twin'",
            &ont,
        )
        .unwrap();
        let key = ont.names["artist"];
        assert_eq!(
            p.query,
            Query::HasAttr {
                key,
                op: CmpOp::Eq,
                value: Value::Text("Aphex Twin".into())
            }
        );
    }

    #[test]
    fn attr_compound_range() {
        let ont = ontology_with(&["year"]);
        let p = plan(
            "SELECT * FROM objects WHERE year >= 2024 AND year < 2026",
            &ont,
        )
        .unwrap();
        let year = ont.names["year"];
        let expected = Query::And(vec![
            Query::HasAttr {
                key: year,
                op: CmpOp::Ge,
                value: Value::Int(2024),
            },
            Query::HasAttr {
                key: year,
                op: CmpOp::Lt,
                value: Value::Int(2026),
            },
        ]);
        assert_eq!(p.query, expected);
    }

    #[test]
    fn like_prefix() {
        let ont = ontology_with(&["name"]);
        let p = plan("SELECT * FROM objects WHERE name LIKE 'cargo%'", &ont).unwrap();
        let key = ont.names["name"];
        assert_eq!(
            p.query,
            Query::HasAttr {
                key,
                op: CmpOp::Prefix,
                value: Value::Text("cargo".into())
            }
        );
    }

    #[test]
    fn like_contains() {
        let ont = ontology_with(&["name"]);
        let p = plan("SELECT * FROM objects WHERE name LIKE '%lock'", &ont).unwrap();
        let key = ont.names["name"];
        assert_eq!(
            p.query,
            Query::HasAttr {
                key,
                op: CmpOp::Contains,
                value: Value::Text("lock".into())
            }
        );
    }

    #[test]
    fn like_mid_string_unsupported() {
        let ont = ontology_with(&["name"]);
        let err = plan("SELECT * FROM objects WHERE name LIKE 'mid%dle'", &ont).unwrap_err();
        assert!(matches!(err, SqlError::UnsupportedPattern(_)));
    }

    #[test]
    fn like_underscore_unsupported() {
        let ont = ontology_with(&["name"]);
        let err = plan("SELECT * FROM objects WHERE name LIKE 'a_b'", &ont).unwrap_err();
        assert!(matches!(err, SqlError::UnsupportedPattern(_)));
    }

    #[test]
    fn boolean_combination_with_not() {
        let ont = ontology_with(&["a", "b", "c"]);
        let p = plan(
            "SELECT * FROM objects WHERE tag = 'a' AND tag = 'b' AND NOT tag = 'c'",
            &ont,
        )
        .unwrap();
        let a = ont.names["a"];
        let b = ont.names["b"];
        let c = ont.names["c"];
        // AND-flattening pulls all three children up.
        let expected = Query::And(vec![
            Query::HasTag(a),
            Query::HasTag(b),
            Query::Not(Box::new(Query::HasTag(c))),
        ]);
        assert_eq!(p.query, expected);
    }

    #[test]
    fn isa_function() {
        let ont = ontology_with(&["vehicle"]);
        let p = plan("SELECT * FROM objects WHERE isa('vehicle')", &ont).unwrap();
        assert_eq!(p.query, Query::IsA(ont.names["vehicle"]));
    }

    #[test]
    fn related_function() {
        let ont = ontology_with(&["member_of"]);
        let p = plan(
            "SELECT * FROM objects WHERE related('member_of', 900)",
            &ont,
        )
        .unwrap();
        assert_eq!(
            p.query,
            Query::Related {
                predicate: ont.names["member_of"],
                target: ObjectId::from_u64(900)
            }
        );
    }

    #[test]
    fn limit_offset_extracted() {
        let ont = ontology_with(&["song"]);
        let p = plan(
            "SELECT * FROM objects WHERE tag = 'song' LIMIT 5 OFFSET 10",
            &ont,
        )
        .unwrap();
        assert_eq!(p.limit, Some(5));
        assert_eq!(p.offset, Some(10));
    }

    #[test]
    fn count_star_planning() {
        let ont = ontology_with(&["song"]);
        let p = plan(
            "SELECT COUNT(*) FROM objects WHERE tag = 'song'",
            &ont,
        )
        .unwrap();
        assert_eq!(p.projection, Projection::Count);
        assert_eq!(p.aggregate, Aggregate::Count);
        assert_eq!(p.query, Query::HasTag(ont.names["song"]));
    }

    #[test]
    fn unknown_tag_errors() {
        let ont = ontology_with(&["a"]);
        let err = plan(
            "SELECT * FROM objects WHERE tag = 'nonexistent'",
            &ont,
        )
        .unwrap_err();
        assert!(matches!(err, SqlError::UnknownTag(name) if name == "nonexistent"));
    }

    #[test]
    fn group_by_unsupported() {
        let ont = ontology_with(&["a"]);
        let err = plan(
            "SELECT * FROM objects WHERE tag = 'a' GROUP BY tag",
            &ont,
        )
        .unwrap_err();
        assert!(matches!(err, SqlError::Unsupported(_)));
    }

    #[test]
    fn order_by_unsupported() {
        let ont = ontology_with(&["a"]);
        let err = plan(
            "SELECT * FROM objects WHERE tag = 'a' ORDER BY tag",
            &ont,
        )
        .unwrap_err();
        assert!(matches!(err, SqlError::Unsupported(_)));
    }

    #[test]
    fn join_unsupported() {
        let ont = ontology_with(&[]);
        let err = plan(
            "SELECT * FROM objects o JOIN objects p ON o.id = p.id",
            &ont,
        )
        .unwrap_err();
        assert!(matches!(err, SqlError::Unsupported(_)));
    }

    #[test]
    fn from_other_table_unsupported() {
        let ont = ontology_with(&[]);
        let err = plan("SELECT * FROM widgets", &ont).unwrap_err();
        assert!(matches!(err, SqlError::Unsupported(_)));
    }

    #[test]
    fn no_where_yields_universe_complement() {
        // Sanity check that the no-WHERE shape is parseable & planned.
        let ont = ontology_with(&[]);
        let p = plan("SELECT * FROM objects", &ont).unwrap();
        assert_eq!(p.query, Query::Not(Box::new(Query::Or(Vec::new()))));
    }

    #[test]
    fn negative_limit_unsupported() {
        let ont = ontology_with(&["a"]);
        let err = plan(
            "SELECT * FROM objects WHERE tag = 'a' LIMIT -1",
            &ont,
        )
        .unwrap_err();
        assert!(matches!(err, SqlError::Unsupported(_)));
    }
}
