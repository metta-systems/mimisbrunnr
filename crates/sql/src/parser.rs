use crate::ast::*;
use crate::error::SqlError;
use mimisbrunnr_types::Value;
use sqlparser::ast as sp;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

/// Parse a SQL string in Mímisbrunnr's dialect into our internal AST.
pub fn parse_sql(input: &str) -> Result<Statement, SqlError> {
    let preprocessed = preprocess(input);
    let dialect = GenericDialect {};
    let statements = Parser::parse_sql(&dialect, &preprocessed)?;

    if statements.is_empty() {
        return Err(SqlError::Parse("empty statement".into()));
    }
    if statements.len() > 1 {
        return Err(SqlError::Unsupported("multiple statements".into()));
    }

    convert_statement(&statements[0])
}

// ---------------------------------------------------------------------------
// Pre-processing: transform custom keywords into function calls
// ---------------------------------------------------------------------------

fn preprocess(input: &str) -> String {
    let mut s = input.to_string();

    // Order matters: longer patterns first to avoid partial matches.

    // HAS ALL TAGS ('x', 'y') → _HAS_ALL_TAGS('x', 'y')
    s = replace_keyword_with_parens(&s, "HAS ALL TAGS", "_HAS_ALL_TAGS");

    // HAS ANY TAG ('x', 'y') → _HAS_ANY_TAG('x', 'y')
    s = replace_keyword_with_parens(&s, "HAS ANY TAG", "_HAS_ANY_TAG");

    // NOT HAS TAG 'x' → NOT _HAS_TAG('x')
    // HAS TAG 'x' → _HAS_TAG('x')
    s = replace_keyword_with_arg(&s, "HAS TAG", "_HAS_TAG");

    // IS A 'x' → _IS_A('x')
    s = replace_keyword_with_arg(&s, "IS A", "_IS_A");

    // IN CONTEXT 'x' → _IN_CONTEXT('x')
    s = replace_keyword_with_arg(&s, "IN CONTEXT", "_IN_CONTEXT");

    // IN COLLECTION 'x' → _IN_COLLECTION('x')
    s = replace_keyword_with_arg(&s, "IN COLLECTION", "_IN_COLLECTION");

    // ORDER BY POSITION → ORDER BY _POSITION
    s = replace_ci(&s, "ORDER BY POSITION", "ORDER BY _POSITION");

    s
}

/// Replace `KEYWORD 'value'` → `FUNC('value')` (case-insensitive).
fn replace_keyword_with_arg(input: &str, keyword: &str, func: &str) -> String {
    let upper = input.to_uppercase();
    let kw_upper = keyword.to_uppercase();
    let mut result = String::with_capacity(input.len());
    let mut i = 0;
    let bytes = input.as_bytes();
    let upper_bytes = upper.as_bytes();
    let kw_bytes = kw_upper.as_bytes();

    while i < bytes.len() {
        if i + kw_bytes.len() <= upper_bytes.len()
            && &upper_bytes[i..i + kw_bytes.len()] == kw_bytes
        {
            // Check word boundary before the keyword
            if i > 0 && upper_bytes[i - 1].is_ascii_alphanumeric() {
                result.push(bytes[i] as char);
                i += 1;
                continue;
            }

            let after = i + kw_bytes.len();

            // Skip whitespace after keyword
            let mut j = after;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }

            // Next token should be a quoted string
            if j < bytes.len() && bytes[j] == b'\'' {
                // Find closing quote
                let start = j;
                let end = find_closing_quote(input, j);
                if let Some(end) = end {
                    let quoted = &input[start..=end];
                    result.push_str(func);
                    result.push('(');
                    result.push_str(quoted);
                    result.push(')');
                    i = end + 1;
                    continue;
                }
            }

            // Not followed by a string literal — leave as-is
            result.push_str(&input[i..after]);
            i = after;
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    result
}

/// Replace `KEYWORD ('x', 'y')` → `FUNC('x', 'y')` (case-insensitive).
fn replace_keyword_with_parens(input: &str, keyword: &str, func: &str) -> String {
    let upper = input.to_uppercase();
    let kw_upper = keyword.to_uppercase();
    let mut result = String::with_capacity(input.len());
    let mut i = 0;
    let bytes = input.as_bytes();
    let upper_bytes = upper.as_bytes();
    let kw_bytes = kw_upper.as_bytes();

    while i < bytes.len() {
        if i + kw_bytes.len() <= upper_bytes.len()
            && &upper_bytes[i..i + kw_bytes.len()] == kw_bytes
        {
            if i > 0 && upper_bytes[i - 1].is_ascii_alphanumeric() {
                result.push(bytes[i] as char);
                i += 1;
                continue;
            }

            let after = i + kw_bytes.len();

            // Skip whitespace after keyword
            let mut j = after;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }

            // Next token should be '('
            if j < bytes.len() && bytes[j] == b'(' {
                // Find matching ')'
                if let Some(close) = find_matching_paren(input, j) {
                    let parens_content = &input[j..=close];
                    result.push_str(func);
                    result.push_str(parens_content);
                    i = close + 1;
                    continue;
                }
            }

            result.push_str(&input[i..after]);
            i = after;
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    result
}

/// Case-insensitive replacement.
fn replace_ci(input: &str, pattern: &str, replacement: &str) -> String {
    let upper = input.to_uppercase();
    let pat_upper = pattern.to_uppercase();

    if let Some(pos) = upper.find(&pat_upper) {
        let mut result = String::with_capacity(input.len());
        result.push_str(&input[..pos]);
        result.push_str(replacement);
        result.push_str(&input[pos + pattern.len()..]);
        result
    } else {
        input.to_string()
    }
}

fn find_closing_quote(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            // Check for escaped quote ''
            if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                i += 2;
            } else {
                return Some(i);
            }
        } else {
            i += 1;
        }
    }
    None
}

fn find_matching_paren(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'\'' => {
                // Skip quoted strings
                if let Some(end) = find_closing_quote(s, i) {
                    i = end;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Convert sqlparser AST → our AST
// ---------------------------------------------------------------------------

fn convert_statement(stmt: &sp::Statement) -> Result<Statement, SqlError> {
    match stmt {
        sp::Statement::Query(query) => convert_query(query),
        _ => Err(SqlError::Unsupported(format!(
            "only SELECT queries are supported, got: {}",
            stmt
        ))),
    }
}

fn convert_query(query: &sp::Query) -> Result<Statement, SqlError> {
    let sp::SetExpr::Select(select) = query.body.as_ref() else {
        return Err(SqlError::Unsupported("only SELECT queries supported".into()));
    };

    let mut context = None;
    let mut collection = None;

    // Check for _IN_CONTEXT / _IN_COLLECTION in the selection (WHERE clause)
    // These are modifiers, not real predicates.

    let projection = convert_projection(&select.projection)?;

    let mut filter = convert_selection(&select.selection)?;

    // Extract context/collection modifiers from the filter tree
    if let Some(ref mut pred) = filter {
        extract_modifiers(pred, &mut context, &mut collection);
        // If the predicate became empty after extraction, set to None
        if is_empty_predicate(pred) {
            filter = None;
        }
    }

    let group_by = convert_group_by(&select.group_by)?;

    let having = match &select.having {
        Some(expr) => convert_expr(expr).map(Some)?,
        None => None,
    };

    let order_by = convert_order_by(&query.order_by)?;

    let (limit, offset) = match &query.limit_clause {
        Some(sp::LimitClause::LimitOffset { limit, offset, .. }) => {
            let l = match limit {
                Some(expr) => Some(expr_to_usize(expr)?),
                None => None,
            };
            let o = match offset {
                Some(sp::Offset { value, .. }) => Some(expr_to_usize(value)?),
                None => None,
            };
            (l, o)
        }
        Some(sp::LimitClause::OffsetCommaLimit { offset, limit }) => {
            (Some(expr_to_usize(limit)?), Some(expr_to_usize(offset)?))
        }
        None => (None, None),
    };

    Ok(Statement::Select(SelectQuery {
        projection,
        filter,
        group_by,
        having,
        order_by,
        limit,
        offset,
        context,
        collection,
    }))
}

fn convert_projection(items: &[sp::SelectItem]) -> Result<Vec<Projection>, SqlError> {
    let mut result = Vec::new();
    for item in items {
        match item {
            sp::SelectItem::Wildcard(_) => result.push(Projection::Star),
            sp::SelectItem::UnnamedExpr(expr) => {
                result.push(convert_projection_expr(expr)?);
            }
            sp::SelectItem::ExprWithAlias { expr, alias } => {
                let inner = convert_projection_expr(expr)?;
                result.push(Projection::Aliased {
                    expr: Box::new(inner),
                    alias: alias.value.clone(),
                });
            }
            _ => return Err(SqlError::Unsupported(format!("select item: {}", item))),
        }
    }
    Ok(result)
}

fn convert_projection_expr(expr: &sp::Expr) -> Result<Projection, SqlError> {
    match expr {
        sp::Expr::Identifier(ident) => Ok(Projection::Column(ident.value.clone())),
        sp::Expr::CompoundIdentifier(parts) => {
            // e.g., src.name → just use the column name
            let name = parts.last().map(|p| p.value.clone()).unwrap_or_default();
            Ok(Projection::Column(name))
        }
        sp::Expr::Function(func) => convert_aggregate_func(func),
        _ => {
            // Fall back to treating it as a column name via its string form
            Ok(Projection::Column(expr.to_string()))
        }
    }
}

fn convert_aggregate_func(func: &sp::Function) -> Result<Projection, SqlError> {
    let name = func.name.to_string().to_uppercase();
    let args = &func.args;

    match name.as_str() {
        "COUNT" => {
            match args {
                sp::FunctionArguments::List(arg_list) => {
                    if arg_list.args.is_empty() {
                        return Ok(Projection::Aggregate(AggregateFunc::Count));
                    }
                    // Check for COUNT(DISTINCT col)
                    if let Some(sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Expr(expr))) =
                        arg_list.args.first()
                    {
                        let col = expr.to_string();
                        if arg_list.duplicate_treatment == Some(sp::DuplicateTreatment::Distinct) {
                            return Ok(Projection::Aggregate(AggregateFunc::CountDistinct(col)));
                        }
                        // COUNT(col) → treat as COUNT(*)
                        if col == "*" {
                            return Ok(Projection::Aggregate(AggregateFunc::Count));
                        }
                    }
                    // Check for COUNT(*) via Wildcard
                    if let Some(sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Wildcard)) =
                        arg_list.args.first()
                    {
                        return Ok(Projection::Aggregate(AggregateFunc::Count));
                    }
                    Ok(Projection::Aggregate(AggregateFunc::Count))
                }
                sp::FunctionArguments::None => Ok(Projection::Aggregate(AggregateFunc::Count)),
                _ => Ok(Projection::Aggregate(AggregateFunc::Count)),
            }
        }
        "SUM" | "AVG" | "MIN" | "MAX" => {
            let col = extract_single_arg(args)?;
            let agg = match name.as_str() {
                "SUM" => AggregateFunc::Sum(col),
                "AVG" => AggregateFunc::Avg(col),
                "MIN" => AggregateFunc::Min(col),
                "MAX" => AggregateFunc::Max(col),
                _ => unreachable!(),
            };
            Ok(Projection::Aggregate(agg))
        }
        _ => Err(SqlError::Unsupported(format!("function: {}", name))),
    }
}

fn extract_single_arg(args: &sp::FunctionArguments) -> Result<String, SqlError> {
    match args {
        sp::FunctionArguments::List(arg_list) => {
            if let Some(sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Expr(expr))) =
                arg_list.args.first()
            {
                Ok(expr.to_string())
            } else {
                Err(SqlError::Parse("expected single argument".into()))
            }
        }
        _ => Err(SqlError::Parse("expected function arguments".into())),
    }
}

fn convert_selection(
    selection: &Option<sp::Expr>,
) -> Result<Option<Predicate>, SqlError> {
    match selection {
        Some(expr) => convert_expr(expr).map(Some),
        None => Ok(None),
    }
}

fn convert_expr(expr: &sp::Expr) -> Result<Predicate, SqlError> {
    match expr {
        sp::Expr::BinaryOp { left, op, right } => convert_binary_op(left, op, right),
        sp::Expr::UnaryOp {
            op: sp::UnaryOperator::Not,
            expr: inner,
        } => Ok(Predicate::Not(Box::new(convert_expr(inner)?))),
        sp::Expr::Nested(inner) => convert_expr(inner),
        sp::Expr::Function(func) => convert_predicate_func(func),
        sp::Expr::Like {
            expr,
            pattern,
            negated,
            ..
        } => {
            let col = expr.to_string();
            let pat = extract_string_value(pattern)?;
            let pred = Predicate::Like {
                column: col,
                pattern: pat,
            };
            if *negated {
                Ok(Predicate::Not(Box::new(pred)))
            } else {
                Ok(pred)
            }
        }
        sp::Expr::InList {
            expr,
            list,
            negated,
        } => {
            let col = expr.to_string();
            let values: Vec<Value> = list
                .iter()
                .map(expr_to_value)
                .collect::<Result<_, _>>()?;
            let pred = Predicate::In {
                column: col,
                values,
            };
            if *negated {
                Ok(Predicate::Not(Box::new(pred)))
            } else {
                Ok(pred)
            }
        }
        _ => Err(SqlError::Unsupported(format!("expression: {}", expr))),
    }
}

fn convert_binary_op(
    left: &sp::Expr,
    op: &sp::BinaryOperator,
    right: &sp::Expr,
) -> Result<Predicate, SqlError> {
    match op {
        sp::BinaryOperator::And => {
            let l = convert_expr(left)?;
            let r = convert_expr(right)?;
            // Flatten nested ANDs
            let mut terms = Vec::new();
            flatten_and(l, &mut terms);
            flatten_and(r, &mut terms);
            Ok(Predicate::And(terms))
        }
        sp::BinaryOperator::Or => {
            let l = convert_expr(left)?;
            let r = convert_expr(right)?;
            let mut terms = Vec::new();
            flatten_or(l, &mut terms);
            flatten_or(r, &mut terms);
            Ok(Predicate::Or(terms))
        }
        sp::BinaryOperator::Eq
        | sp::BinaryOperator::NotEq
        | sp::BinaryOperator::Lt
        | sp::BinaryOperator::LtEq
        | sp::BinaryOperator::Gt
        | sp::BinaryOperator::GtEq => {
            let col = left.to_string();
            let value = expr_to_value(right)?;
            let cmp = match op {
                sp::BinaryOperator::Eq => CompareOp::Eq,
                sp::BinaryOperator::NotEq => CompareOp::Ne,
                sp::BinaryOperator::Lt => CompareOp::Lt,
                sp::BinaryOperator::LtEq => CompareOp::Le,
                sp::BinaryOperator::Gt => CompareOp::Gt,
                sp::BinaryOperator::GtEq => CompareOp::Ge,
                _ => unreachable!(),
            };
            Ok(Predicate::Compare {
                column: col,
                op: cmp,
                value,
            })
        }
        _ => Err(SqlError::Unsupported(format!("operator: {}", op))),
    }
}

fn convert_predicate_func(func: &sp::Function) -> Result<Predicate, SqlError> {
    let name = func.name.to_string().to_uppercase();
    let args = &func.args;

    match name.as_str() {
        "_HAS_TAG" => {
            let tag = extract_single_string_arg(args)?;
            Ok(Predicate::HasTag(tag))
        }
        "_HAS_ALL_TAGS" => {
            let tags = extract_string_args(args)?;
            Ok(Predicate::HasAllTags(tags))
        }
        "_HAS_ANY_TAG" => {
            let tags = extract_string_args(args)?;
            Ok(Predicate::HasAnyTag(tags))
        }
        "_IS_A" => {
            let tag = extract_single_string_arg(args)?;
            Ok(Predicate::IsA(tag))
        }
        "_IN_CONTEXT" => {
            // This is a modifier, not a real predicate. We'll extract it later.
            let ctx = extract_single_string_arg(args)?;
            Ok(Predicate::Compare {
                column: "__context__".into(),
                op: CompareOp::Eq,
                value: Value::Text(ctx),
            })
        }
        "_IN_COLLECTION" => {
            let coll = extract_single_string_arg(args)?;
            Ok(Predicate::Compare {
                column: "__collection__".into(),
                op: CompareOp::Eq,
                value: Value::Text(coll),
            })
        }
        _ => Err(SqlError::Unsupported(format!("function: {}", name))),
    }
}

fn extract_single_string_arg(args: &sp::FunctionArguments) -> Result<String, SqlError> {
    match args {
        sp::FunctionArguments::List(arg_list) => {
            if let Some(sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Expr(expr))) =
                arg_list.args.first()
            {
                extract_string_value(expr)
            } else {
                Err(SqlError::Parse("expected string argument".into()))
            }
        }
        _ => Err(SqlError::Parse("expected function arguments".into())),
    }
}

fn extract_string_args(args: &sp::FunctionArguments) -> Result<Vec<String>, SqlError> {
    match args {
        sp::FunctionArguments::List(arg_list) => {
            arg_list
                .args
                .iter()
                .map(|arg| {
                    if let sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Expr(expr)) = arg {
                        extract_string_value(expr)
                    } else {
                        Err(SqlError::Parse("expected string argument".into()))
                    }
                })
                .collect()
        }
        _ => Err(SqlError::Parse("expected function arguments".into())),
    }
}

fn extract_string_value(expr: &sp::Expr) -> Result<String, SqlError> {
    match expr {
        sp::Expr::Value(val) => match &val.value {
            sp::Value::SingleQuotedString(s) => Ok(s.clone()),
            sp::Value::DoubleQuotedString(s) => Ok(s.clone()),
            _ => Err(SqlError::Parse(format!("expected string, got: {}", val))),
        },
        _ => Err(SqlError::Parse(format!("expected string literal, got: {}", expr))),
    }
}

fn expr_to_value(expr: &sp::Expr) -> Result<Value, SqlError> {
    match expr {
        sp::Expr::Value(val) => match &val.value {
            sp::Value::SingleQuotedString(s) | sp::Value::DoubleQuotedString(s) => {
                Ok(Value::Text(s.clone()))
            }
            sp::Value::Number(n, _) => {
                if let Ok(i) = n.parse::<i64>() {
                    Ok(Value::Int(i))
                } else if let Ok(f) = n.parse::<f64>() {
                    Ok(Value::Float(f))
                } else {
                    Err(SqlError::Parse(format!("invalid number: {}", n)))
                }
            }
            _ => Err(SqlError::Parse(format!("unsupported value: {}", val))),
        },
        sp::Expr::UnaryOp {
            op: sp::UnaryOperator::Minus,
            expr: inner,
        } => {
            let v = expr_to_value(inner)?;
            match v {
                Value::Int(i) => Ok(Value::Int(-i)),
                Value::Float(f) => Ok(Value::Float(-f)),
                _ => Err(SqlError::TypeError("cannot negate non-numeric value".into())),
            }
        }
        _ => Err(SqlError::Parse(format!(
            "expected value literal, got: {}",
            expr
        ))),
    }
}

fn expr_to_usize(expr: &sp::Expr) -> Result<usize, SqlError> {
    match expr_to_value(expr)? {
        Value::Int(i) if i >= 0 => Ok(i as usize),
        _ => Err(SqlError::TypeError("expected non-negative integer".into())),
    }
}

fn flatten_and(pred: Predicate, out: &mut Vec<Predicate>) {
    match pred {
        Predicate::And(terms) => {
            for t in terms {
                flatten_and(t, out);
            }
        }
        other => out.push(other),
    }
}

fn flatten_or(pred: Predicate, out: &mut Vec<Predicate>) {
    match pred {
        Predicate::Or(terms) => {
            for t in terms {
                flatten_or(t, out);
            }
        }
        other => out.push(other),
    }
}

/// Extract IN CONTEXT / IN COLLECTION modifiers from the predicate tree.
fn extract_modifiers(pred: &mut Predicate, context: &mut Option<String>, collection: &mut Option<String>) {
    match pred {
        Predicate::Compare { column, value, .. } if column == "__context__" => {
            if let Value::Text(s) = value {
                *context = Some(s.clone());
            }
            // Mark as empty
            *pred = Predicate::And(vec![]);
        }
        Predicate::Compare { column, value, .. } if column == "__collection__" => {
            if let Value::Text(s) = value {
                *collection = Some(s.clone());
            }
            *pred = Predicate::And(vec![]);
        }
        Predicate::And(terms) => {
            for t in terms.iter_mut() {
                extract_modifiers(t, context, collection);
            }
            terms.retain(|t| !is_empty_predicate(t));
        }
        _ => {}
    }
}

fn is_empty_predicate(pred: &Predicate) -> bool {
    matches!(pred, Predicate::And(terms) if terms.is_empty())
}

fn convert_group_by(group_by: &sp::GroupByExpr) -> Result<Vec<String>, SqlError> {
    match group_by {
        sp::GroupByExpr::All(_) => Err(SqlError::Unsupported("GROUP BY ALL".into())),
        sp::GroupByExpr::Expressions(exprs, _modifiers) => {
            exprs.iter().map(|e| Ok(e.to_string())).collect()
        }
    }
}

fn convert_order_by(order_by: &Option<sp::OrderBy>) -> Result<Vec<OrderBy>, SqlError> {
    let Some(order_by) = order_by else {
        return Ok(Vec::new());
    };

    match &order_by.kind {
        sp::OrderByKind::Expressions(exprs) => exprs
            .iter()
            .map(|item| {
                let col = item.expr.to_string();
                let direction = match item.options.asc {
                    Some(true) => SortDir::Asc,
                    Some(false) => SortDir::Desc,
                    None => SortDir::Asc,
                };
                Ok(OrderBy {
                    column: col,
                    direction,
                })
            })
            .collect(),
        sp::OrderByKind::All(_) => Err(SqlError::Unsupported("ORDER BY ALL".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocess_has_tag() {
        let input = "SELECT * FROM objects WHERE HAS TAG 'electronic'";
        let result = preprocess(input);
        assert!(result.contains("_HAS_TAG('electronic')"));
    }

    #[test]
    fn preprocess_is_a() {
        let input = "SELECT * FROM objects WHERE IS A 'audio'";
        let result = preprocess(input);
        assert!(result.contains("_IS_A('audio')"));
    }

    #[test]
    fn preprocess_has_all_tags() {
        let input = "WHERE HAS ALL TAGS ('x', 'y')";
        let result = preprocess(input);
        assert!(result.contains("_HAS_ALL_TAGS('x', 'y')"));
    }

    #[test]
    fn preprocess_has_any_tag() {
        let input = "WHERE HAS ANY TAG ('x', 'y')";
        let result = preprocess(input);
        assert!(result.contains("_HAS_ANY_TAG('x', 'y')"));
    }

    #[test]
    fn parse_simple_select() {
        let sql = "SELECT id, name FROM objects WHERE HAS TAG 'electronic' AND year = 2024";
        let stmt = parse_sql(sql).unwrap();
        let Statement::Select(q) = stmt;

        assert_eq!(q.projection.len(), 2);
        assert!(matches!(&q.projection[0], Projection::Column(s) if s == "id"));
        assert!(matches!(&q.projection[1], Projection::Column(s) if s == "name"));

        let filter = q.filter.unwrap();
        match filter {
            Predicate::And(terms) => {
                assert_eq!(terms.len(), 2);
                assert!(matches!(&terms[0], Predicate::HasTag(s) if s == "electronic"));
                assert!(matches!(
                    &terms[1],
                    Predicate::Compare { column, op: CompareOp::Eq, value: Value::Int(2024) }
                    if column == "year"
                ));
            }
            _ => panic!("expected And"),
        }
    }

    #[test]
    fn parse_select_star() {
        let sql = "SELECT * FROM objects";
        let stmt = parse_sql(sql).unwrap();
        let Statement::Select(q) = stmt;
        assert_eq!(q.projection.len(), 1);
        assert!(matches!(&q.projection[0], Projection::Star));
        assert!(q.filter.is_none());
    }

    #[test]
    fn parse_aggregation() {
        let sql = "SELECT artist, COUNT(*) as tracks FROM objects WHERE HAS TAG 'electronic' GROUP BY artist ORDER BY tracks DESC";
        let stmt = parse_sql(sql).unwrap();
        let Statement::Select(q) = stmt;

        assert_eq!(q.projection.len(), 2);
        assert!(matches!(&q.projection[0], Projection::Column(s) if s == "artist"));
        assert!(matches!(
            &q.projection[1],
            Projection::Aliased { alias, .. } if alias == "tracks"
        ));

        assert_eq!(q.group_by, vec!["artist"]);
        assert_eq!(q.order_by.len(), 1);
        assert_eq!(q.order_by[0].column, "tracks");
        assert_eq!(q.order_by[0].direction, SortDir::Desc);
    }

    #[test]
    fn parse_limit_offset() {
        let sql = "SELECT id FROM objects LIMIT 50 OFFSET 100";
        let stmt = parse_sql(sql).unwrap();
        let Statement::Select(q) = stmt;
        assert_eq!(q.limit, Some(50));
        assert_eq!(q.offset, Some(100));
    }

    #[test]
    fn parse_is_a() {
        let sql = "SELECT id FROM objects WHERE IS A 'audio'";
        let stmt = parse_sql(sql).unwrap();
        let Statement::Select(q) = stmt;
        let filter = q.filter.unwrap();
        assert!(matches!(filter, Predicate::IsA(s) if s == "audio"));
    }

    #[test]
    fn parse_not_has_tag() {
        let sql = "SELECT id FROM objects WHERE NOT HAS TAG 'deleted'";
        let stmt = parse_sql(sql).unwrap();
        let Statement::Select(q) = stmt;
        let filter = q.filter.unwrap();
        match filter {
            Predicate::Not(inner) => {
                assert!(matches!(*inner, Predicate::HasTag(s) if s == "deleted"));
            }
            _ => panic!("expected Not"),
        }
    }

    #[test]
    fn parse_complex_where() {
        let sql = "SELECT id FROM objects WHERE HAS TAG 'source' AND lang = 'rust' AND year > 2023";
        let stmt = parse_sql(sql).unwrap();
        let Statement::Select(q) = stmt;
        let filter = q.filter.unwrap();
        match filter {
            Predicate::And(terms) => {
                assert_eq!(terms.len(), 3);
                assert!(matches!(&terms[0], Predicate::HasTag(s) if s == "source"));
                assert!(matches!(
                    &terms[1],
                    Predicate::Compare { column, op: CompareOp::Eq, value: Value::Text(s) }
                    if column == "lang" && s == "rust"
                ));
                assert!(matches!(
                    &terms[2],
                    Predicate::Compare { column, op: CompareOp::Gt, value: Value::Int(2023) }
                    if column == "year"
                ));
            }
            _ => panic!("expected And"),
        }
    }
}
