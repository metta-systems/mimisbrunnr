use crate::{ast::*, error::SqlError};

/// Physical execution plan — a tree of operations that the executor walks.
///
/// Leaf nodes fetch bitmaps from indexes. Interior nodes combine them.
/// Post-processing nodes (Project, Sort, GroupBy, Limit) operate on
/// the materialized result set.
#[derive(Debug, Clone, PartialEq)]
pub enum PhysicalOp {
    // -- Leaf: fetch a bitmap from an index --
    /// Tag membership bitmap.
    TagBitmap(String),
    /// KV equality bitmap: `attr = value`.
    KvBitmap(String, crate::ast::CompareOp, mimisbrunnr_types::Value),
    /// Ontology-aware tag membership: IS A.
    IsABitmap(String),
    /// All objects (universal set).
    AllObjects,

    // -- Combine bitmaps --
    /// AND — intersect.
    Intersect(Vec<PhysicalOp>),
    /// OR — union.
    Union(Vec<PhysicalOp>),
    /// NOT — difference from universal set.
    Complement(Box<PhysicalOp>),

    // -- Post-filter --
    /// Post-filter for predicates not directly in an index (e.g. LIKE).
    Filter(Box<PhysicalOp>, FilterPredicate),

    // -- Projection --
    /// Select specific columns from the result set.
    Project(Box<PhysicalOp>, Vec<Projection>),

    // -- Sort --
    /// Sort the result set by a column.
    Sort(Box<PhysicalOp>, String, SortDir),

    // -- Aggregate --
    /// GROUP BY with aggregate functions.
    GroupBy {
        input: Box<PhysicalOp>,
        group_columns: Vec<String>,
        aggregates: Vec<(AggregateFunc, Option<String>)>,
        having: Option<FilterPredicate>,
    },

    /// Scalar aggregate (no GROUP BY, e.g. `SELECT COUNT(*) FROM objects`).
    ScalarAggregate {
        input: Box<PhysicalOp>,
        aggregates: Vec<(AggregateFunc, Option<String>)>,
    },

    // -- Limit/Offset --
    /// LIMIT and OFFSET.
    Limit(Box<PhysicalOp>, usize, usize),
}

/// A predicate for post-filtering (not pushable to index).
#[derive(Debug, Clone, PartialEq)]
pub enum FilterPredicate {
    Like {
        column: String,
        pattern: String,
    },
    In {
        column: String,
        values: Vec<mimisbrunnr_types::Value>,
    },
    Compare {
        column: String,
        op: CompareOp,
        value: mimisbrunnr_types::Value,
    },
    And(Vec<FilterPredicate>),
    Or(Vec<FilterPredicate>),
    Not(Box<FilterPredicate>),
}

/// Plan a SELECT query into a physical execution plan.
pub fn plan(query: &SelectQuery) -> Result<PhysicalOp, SqlError> {
    // 1. Plan the WHERE clause → bitmap operations
    let mut plan = match &query.filter {
        Some(pred) => plan_predicate(pred)?,
        None => PhysicalOp::AllObjects,
    };

    // 2. Check for GROUP BY / aggregates
    let has_aggregates = query.projection.iter().any(is_aggregate_projection);
    let has_group_by = !query.group_by.is_empty();

    if has_group_by || has_aggregates {
        let aggregates = extract_aggregates(&query.projection);

        if has_group_by {
            let having = match &query.having {
                Some(pred) => Some(predicate_to_filter(pred)?),
                None => None,
            };
            plan = PhysicalOp::GroupBy {
                input: Box::new(plan),
                group_columns: query.group_by.clone(),
                aggregates,
                having,
            };
        } else {
            plan = PhysicalOp::ScalarAggregate {
                input: Box::new(plan),
                aggregates,
            };
        }
    } else {
        // 3. Projection (only for non-aggregate queries)
        if !query.projection.is_empty()
            && !query
                .projection
                .iter()
                .all(|p| matches!(p, Projection::Star))
        {
            plan = PhysicalOp::Project(Box::new(plan), query.projection.clone());
        }
    }

    // 4. ORDER BY
    if let Some(order) = query.order_by.first() {
        plan = PhysicalOp::Sort(Box::new(plan), order.column.clone(), order.direction);
    }

    // 5. LIMIT / OFFSET
    if query.limit.is_some() || query.offset.is_some() {
        let limit = query.limit.unwrap_or(usize::MAX);
        let offset = query.offset.unwrap_or(0);
        plan = PhysicalOp::Limit(Box::new(plan), limit, offset);
    }

    Ok(plan)
}

/// Plan a predicate into physical operations.
///
/// Pushes tag and KV predicates into index lookups. Non-indexable predicates
/// become post-filters.
fn plan_predicate(pred: &Predicate) -> Result<PhysicalOp, SqlError> {
    match pred {
        Predicate::HasTag(tag) => Ok(PhysicalOp::TagBitmap(tag.clone())),

        Predicate::HasAllTags(tags) => {
            let ops: Vec<PhysicalOp> = tags
                .iter()
                .map(|t| PhysicalOp::TagBitmap(t.clone()))
                .collect();
            Ok(PhysicalOp::Intersect(ops))
        }

        Predicate::HasAnyTag(tags) => {
            let ops: Vec<PhysicalOp> = tags
                .iter()
                .map(|t| PhysicalOp::TagBitmap(t.clone()))
                .collect();
            Ok(PhysicalOp::Union(ops))
        }

        Predicate::IsA(tag) => Ok(PhysicalOp::IsABitmap(tag.clone())),

        Predicate::Compare { column, op, value } => {
            // Equality comparisons can use the KV index directly.
            // Range comparisons also go through KvBitmap and executor handles them.
            Ok(PhysicalOp::KvBitmap(column.clone(), *op, value.clone()))
        }

        Predicate::Like { column, pattern } => {
            // LIKE can't use the index — post-filter on all objects.
            Ok(PhysicalOp::Filter(
                Box::new(PhysicalOp::AllObjects),
                FilterPredicate::Like {
                    column: column.clone(),
                    pattern: pattern.clone(),
                },
            ))
        }

        Predicate::In { column, values } => {
            // IN can be expanded to OR of equalities, or post-filtered.
            // For small IN lists, expand to Union of KvBitmaps.
            if values.len() <= 32 {
                let ops: Vec<PhysicalOp> = values
                    .iter()
                    .map(|v| PhysicalOp::KvBitmap(column.clone(), CompareOp::Eq, v.clone()))
                    .collect();
                Ok(PhysicalOp::Union(ops))
            } else {
                Ok(PhysicalOp::Filter(
                    Box::new(PhysicalOp::AllObjects),
                    FilterPredicate::In {
                        column: column.clone(),
                        values: values.clone(),
                    },
                ))
            }
        }

        Predicate::And(terms) => {
            let mut index_ops = Vec::new();
            let mut filter_preds = Vec::new();

            for term in terms {
                let op = plan_predicate(term)?;
                // If the planned op is a Filter on AllObjects, it's a post-filter.
                // Otherwise, it's an index operation.
                match op {
                    PhysicalOp::Filter(ref inner, _)
                        if matches!(inner.as_ref(), PhysicalOp::AllObjects) =>
                    {
                        // Extract the FilterPredicate from the Filter variant
                        if let PhysicalOp::Filter(_, fp) = op {
                            filter_preds.push(fp);
                        }
                    }
                    _ => index_ops.push(op),
                }
            }

            let base = if index_ops.is_empty() {
                PhysicalOp::AllObjects
            } else if index_ops.len() == 1 {
                index_ops.into_iter().next().unwrap()
            } else {
                PhysicalOp::Intersect(index_ops)
            };

            if filter_preds.is_empty() {
                Ok(base)
            } else {
                let filter = if filter_preds.len() == 1 {
                    filter_preds.into_iter().next().unwrap()
                } else {
                    FilterPredicate::And(filter_preds)
                };
                Ok(PhysicalOp::Filter(Box::new(base), filter))
            }
        }

        Predicate::Or(terms) => {
            let ops: Vec<PhysicalOp> =
                terms.iter().map(plan_predicate).collect::<Result<_, _>>()?;
            Ok(PhysicalOp::Union(ops))
        }

        Predicate::Not(inner) => {
            let op = plan_predicate(inner)?;
            Ok(PhysicalOp::Complement(Box::new(op)))
        }
    }
}

fn predicate_to_filter(pred: &Predicate) -> Result<FilterPredicate, SqlError> {
    match pred {
        Predicate::Compare { column, op, value } => Ok(FilterPredicate::Compare {
            column: column.clone(),
            op: *op,
            value: value.clone(),
        }),
        Predicate::Like { column, pattern } => Ok(FilterPredicate::Like {
            column: column.clone(),
            pattern: pattern.clone(),
        }),
        Predicate::And(terms) => {
            let filters: Vec<FilterPredicate> = terms
                .iter()
                .map(predicate_to_filter)
                .collect::<Result<_, _>>()?;
            Ok(FilterPredicate::And(filters))
        }
        Predicate::Or(terms) => {
            let filters: Vec<FilterPredicate> = terms
                .iter()
                .map(predicate_to_filter)
                .collect::<Result<_, _>>()?;
            Ok(FilterPredicate::Or(filters))
        }
        Predicate::Not(inner) => Ok(FilterPredicate::Not(Box::new(predicate_to_filter(inner)?))),
        _ => Err(SqlError::Unsupported(format!(
            "HAVING predicate: {:?}",
            pred
        ))),
    }
}

fn is_aggregate_projection(p: &Projection) -> bool {
    match p {
        Projection::Aggregate(_) => true,
        Projection::Aliased { expr, .. } => is_aggregate_projection(expr),
        _ => false,
    }
}

fn extract_aggregates(projection: &[Projection]) -> Vec<(AggregateFunc, Option<String>)> {
    let mut aggs = Vec::new();
    for p in projection {
        match p {
            Projection::Aggregate(func) => {
                aggs.push((func.clone(), None));
            }
            Projection::Aliased { expr, alias } => {
                if let Projection::Aggregate(func) = expr.as_ref() {
                    aggs.push((func.clone(), Some(alias.clone())));
                }
            }
            _ => {}
        }
    }
    aggs
}

#[cfg(test)]
mod tests {
    use {super::*, crate::parser::parse_sql};

    #[test]
    fn plan_simple_tag_query() {
        let sql = "SELECT id FROM objects WHERE HAS TAG 'electronic'";
        let Statement::Select(q) = parse_sql(sql).unwrap();
        let plan = plan(&q).unwrap();

        // Should be: Project(TagBitmap("electronic"), [id])
        match &plan {
            PhysicalOp::Project(inner, _) => {
                assert!(matches!(inner.as_ref(), PhysicalOp::TagBitmap(t) if t == "electronic"));
            }
            _ => panic!("expected Project(TagBitmap)"),
        }
    }

    #[test]
    fn plan_and_predicates() {
        let sql = "SELECT * FROM objects WHERE HAS TAG 'source' AND lang = 'rust'";
        let Statement::Select(q) = parse_sql(sql).unwrap();
        let plan = plan(&q).unwrap();

        // Should be: Intersect([TagBitmap("source"), KvBitmap("lang", Eq, "rust")])
        assert!(matches!(plan, PhysicalOp::Intersect(_)));
    }

    #[test]
    fn plan_with_limit() {
        let sql = "SELECT id FROM objects LIMIT 10";
        let Statement::Select(q) = parse_sql(sql).unwrap();
        let plan = plan(&q).unwrap();

        assert!(matches!(plan, PhysicalOp::Limit(_, 10, 0)));
    }

    #[test]
    fn plan_group_by() {
        let sql = "SELECT artist, COUNT(*) FROM objects GROUP BY artist";
        let Statement::Select(q) = parse_sql(sql).unwrap();
        let plan = plan(&q).unwrap();

        assert!(matches!(plan, PhysicalOp::GroupBy { .. }));
    }

    #[test]
    fn plan_predicate_pushdown() {
        let sql = "SELECT name FROM objects WHERE HAS TAG 'source' AND lang = 'rust' AND component LIKE 'kernel%'";
        let Statement::Select(q) = parse_sql(sql).unwrap();
        let plan = plan(&q).unwrap();

        // The LIKE predicate should become a Filter, while tag and kv go to index ops.
        // Structure: Project(Filter(Intersect([TagBitmap, KvBitmap]), Like), [name])
        match plan {
            PhysicalOp::Project(inner, _) => match *inner {
                PhysicalOp::Filter(inner, FilterPredicate::Like { ref column, .. }) => {
                    assert_eq!(column, "component");
                    assert!(matches!(*inner, PhysicalOp::Intersect(_)));
                }
                _ => panic!("expected Filter"),
            },
            _ => panic!("expected Project"),
        }
    }
}
