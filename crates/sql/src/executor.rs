use {
    crate::{
        ast::*,
        error::SqlError,
        planner::{FilterPredicate, PhysicalOp},
        types::{GroupRow, QueryResult, Row},
    },
    mimisbrunnr_index::{ForwardIndex, KvIndex, RoaringBitmap, TagIndex},
    mimisbrunnr_ontology::ImplicationDag,
    mimisbrunnr_types::{Assertion, ObjectId, TagId, Value},
    std::collections::HashMap,
};

/// Executes a physical plan against Mímisbrunnr's index layer.
pub struct SqlExecutor<'a> {
    tag_index: &'a TagIndex,
    kv_index: &'a KvIndex,
    forward_index: &'a ForwardIndex,
    dag: &'a ImplicationDag,
}

impl<'a> SqlExecutor<'a> {
    pub fn new(
        tag_index: &'a TagIndex,
        kv_index: &'a KvIndex,
        forward_index: &'a ForwardIndex,
        dag: &'a ImplicationDag,
    ) -> Self {
        Self {
            tag_index,
            kv_index,
            forward_index,
            dag,
        }
    }

    /// Execute a physical plan and return the query result.
    pub fn execute(&self, plan: &PhysicalOp) -> Result<QueryResult, SqlError> {
        match plan {
            PhysicalOp::GroupBy {
                input,
                group_columns,
                aggregates,
                having,
            } => self.execute_group_by(input, group_columns, aggregates, having),

            PhysicalOp::ScalarAggregate { input, aggregates } => {
                self.execute_scalar_aggregate(input, aggregates)
            }

            PhysicalOp::Limit(inner, limit, offset) => {
                let result = self.execute(inner)?;
                Ok(apply_limit(result, *limit, *offset))
            }

            PhysicalOp::Sort(inner, column, direction) => {
                let result = self.execute(inner)?;
                Ok(self.apply_sort(result, column, *direction))
            }

            // Everything else produces a Select result
            _ => {
                let bitmap = self.execute_bitmap(plan)?;
                let (columns, rows) = self.materialize(plan, &bitmap);
                Ok(QueryResult::Select { columns, rows })
            }
        }
    }

    /// Execute a plan node that produces a bitmap (the bitmap algebra layer).
    fn execute_bitmap(&self, plan: &PhysicalOp) -> Result<RoaringBitmap, SqlError> {
        match plan {
            PhysicalOp::TagBitmap(tag_name) => {
                let tag_id = self.resolve_tag(tag_name)?;
                Ok(self.tag_index.bitmap(tag_id).cloned().unwrap_or_default())
            }

            PhysicalOp::KvBitmap(column, op, value) => {
                let key = self.resolve_tag(column)?;
                self.execute_kv(key, *op, value)
            }

            PhysicalOp::IsABitmap(tag_name) => {
                let tag_id = self.resolve_tag(tag_name)?;
                let mut result = self.tag_index.bitmap(tag_id).cloned().unwrap_or_default();
                for desc in self.dag.descendants(tag_id) {
                    if let Some(bm) = self.tag_index.bitmap(desc) {
                        result |= bm;
                    }
                }
                Ok(result)
            }

            PhysicalOp::AllObjects => Ok(self.universal_set()),

            PhysicalOp::Intersect(ops) => {
                if ops.is_empty() {
                    return Ok(RoaringBitmap::new());
                }
                // Execute smallest-first for short-circuit optimization
                let mut results: Vec<RoaringBitmap> = ops
                    .iter()
                    .map(|op| self.execute_bitmap(op))
                    .collect::<Result<_, _>>()?;
                results.sort_by_key(|r| r.len());

                let mut result = results.swap_remove(0);
                for other in results {
                    result &= &other;
                    if result.is_empty() {
                        break;
                    }
                }
                Ok(result)
            }

            PhysicalOp::Union(ops) => {
                let mut result = RoaringBitmap::new();
                for op in ops {
                    result |= self.execute_bitmap(op)?;
                }
                Ok(result)
            }

            PhysicalOp::Complement(inner) => {
                let universal = self.universal_set();
                let excluded = self.execute_bitmap(inner)?;
                Ok(universal - excluded)
            }

            PhysicalOp::Filter(inner, filter) => {
                let bitmap = self.execute_bitmap(inner)?;
                Ok(self.apply_filter(&bitmap, filter))
            }

            PhysicalOp::Project(inner, _) => {
                // Project doesn't change the bitmap — it only affects materialization.
                self.execute_bitmap(inner)
            }

            PhysicalOp::Sort(inner, _, _) => self.execute_bitmap(inner),
            PhysicalOp::Limit(inner, _, _) => self.execute_bitmap(inner),
            PhysicalOp::GroupBy { input, .. } => self.execute_bitmap(input),
            PhysicalOp::ScalarAggregate { input, .. } => self.execute_bitmap(input),
        }
    }

    fn execute_kv(
        &self,
        key: TagId,
        op: CompareOp,
        value: &Value,
    ) -> Result<RoaringBitmap, SqlError> {
        match op {
            CompareOp::Eq => Ok(self.kv_index.lookup_eq(key, value)),
            CompareOp::Ne => {
                let eq_set = self.kv_index.lookup_eq(key, value);
                let all_with_key = self.all_objects_with_key(key);
                Ok(all_with_key - eq_set)
            }
            // Range comparisons: scan the forward index
            CompareOp::Lt | CompareOp::Le | CompareOp::Gt | CompareOp::Ge => {
                let all_with_key = self.all_objects_with_key(key);
                let mut result = RoaringBitmap::new();
                for obj_local in all_with_key.iter() {
                    let oid = ObjectId::new(0, obj_local as u64);
                    if let Some(obj_value) = self.get_attr(oid, key)
                        && compare_values(&obj_value, value, op)
                    {
                        result.insert(obj_local);
                    }
                }
                Ok(result)
            }
        }
    }

    fn all_objects_with_key(&self, key: TagId) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        for _hash in self.kv_index.values_for_key(key) {
            // We can't look up by hash directly, so union via values_for_key
            // is already done by the KvIndex's internal structure.
        }
        // Fallback: scan the forward index for objects with this key
        // This is the correct approach since KvIndex doesn't expose lookup by hash.
        // For correctness, scan through the bitmap of all tags.
        for tag_id in self.tag_index.all_tags() {
            if let Some(bm) = self.tag_index.bitmap(tag_id) {
                for obj_local in bm.iter() {
                    let oid = ObjectId::new(0, obj_local as u64);
                    if self.get_attr(oid, key).is_some() {
                        result.insert(obj_local);
                    }
                }
            }
        }
        result
    }

    /// Resolve a tag/attribute name to a TagId via the ontology DAG.
    fn resolve_tag(&self, name: &str) -> Result<TagId, SqlError> {
        self.dag
            .lookup(name)
            .ok_or_else(|| SqlError::UnknownTag(name.to_string()))
    }

    /// Build the universal set from all bitmaps in the tag index.
    fn universal_set(&self) -> RoaringBitmap {
        let mut universal = RoaringBitmap::new();
        for tag in self.tag_index.all_tags() {
            if let Some(bm) = self.tag_index.bitmap(tag) {
                universal |= bm;
            }
        }
        universal
    }

    /// Get an attribute value from the forward index.
    fn get_attr(&self, oid: ObjectId, key: TagId) -> Option<Value> {
        for entry in self.forward_index.get(oid) {
            if let Assertion::Attr {
                key: k,
                value: ref v,
            } = entry.assertion
                && k == key
            {
                return Some(v.clone());
            }
        }
        None
    }

    /// Get a tag name from the DAG.
    fn tag_name(&self, tag_id: TagId) -> String {
        self.dag
            .get(tag_id)
            .map(|def| def.name.clone())
            .unwrap_or_else(|| format!("tag_{}", tag_id.raw()))
    }

    /// Materialize a bitmap into rows with projected columns.
    fn materialize(&self, plan: &PhysicalOp, bitmap: &RoaringBitmap) -> (Vec<String>, Vec<Row>) {
        let columns = self.extract_column_names(plan);
        let rows: Vec<Row> = bitmap
            .iter()
            .map(|obj_local| {
                let oid = ObjectId::new(0, obj_local as u64);
                let cols = if columns.is_empty() || columns.contains(&"*".to_string()) {
                    // Return all attributes
                    self.all_attrs(oid)
                } else {
                    columns
                        .iter()
                        .filter_map(|col| {
                            if col == "id" {
                                Some(("id".to_string(), Value::Int(oid.local() as i64)))
                            } else {
                                let tag_id = self.dag.lookup(col)?;
                                let value = self.get_attr(oid, tag_id)?;
                                Some((col.clone(), value))
                            }
                        })
                        .collect()
                };
                Row {
                    id: oid,
                    columns: cols,
                }
            })
            .collect();

        let col_names = if columns.is_empty() || columns.contains(&"*".to_string()) {
            vec!["id".to_string()]
        } else {
            columns
        };

        (col_names, rows)
    }

    fn extract_column_names(&self, plan: &PhysicalOp) -> Vec<String> {
        match plan {
            PhysicalOp::Project(_, projections) => projections
                .iter()
                .filter_map(|p| match p {
                    Projection::Star => Some("*".to_string()),
                    Projection::Column(name) => Some(name.clone()),
                    Projection::Aliased { expr, alias } => match expr.as_ref() {
                        Projection::Column(name) => Some(name.clone()),
                        _ => Some(alias.clone()),
                    },
                    Projection::Aggregate(_) => None,
                })
                .collect(),
            _ => vec!["*".to_string()],
        }
    }

    fn all_attrs(&self, oid: ObjectId) -> Vec<(String, Value)> {
        let mut attrs = vec![("id".to_string(), Value::Int(oid.local() as i64))];
        for entry in self.forward_index.get(oid) {
            match &entry.assertion {
                Assertion::Attr { key, value } => {
                    attrs.push((self.tag_name(*key), value.clone()));
                }
                Assertion::Tag(tag_id) => {
                    attrs.push(("tag".to_string(), Value::Text(self.tag_name(*tag_id))));
                }
                _ => {}
            }
        }
        attrs
    }

    /// Apply a post-filter to a bitmap.
    fn apply_filter(&self, bitmap: &RoaringBitmap, filter: &FilterPredicate) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        for obj_local in bitmap.iter() {
            let oid = ObjectId::new(0, obj_local as u64);
            if self.eval_filter(oid, filter) {
                result.insert(obj_local);
            }
        }
        result
    }

    fn eval_filter(&self, oid: ObjectId, filter: &FilterPredicate) -> bool {
        match filter {
            FilterPredicate::Like { column, pattern } => {
                if let Some(tag_id) = self.dag.lookup(column)
                    && let Some(Value::Text(s)) = self.get_attr(oid, tag_id)
                {
                    return like_match(&s, pattern);
                }
                false
            }
            FilterPredicate::In { column, values } => {
                if let Some(tag_id) = self.dag.lookup(column)
                    && let Some(val) = self.get_attr(oid, tag_id)
                {
                    return values.contains(&val);
                }
                false
            }
            FilterPredicate::Compare { column, op, value } => {
                if let Some(tag_id) = self.dag.lookup(column)
                    && let Some(val) = self.get_attr(oid, tag_id)
                {
                    return compare_values(&val, value, *op);
                }
                false
            }
            FilterPredicate::And(terms) => terms.iter().all(|t| self.eval_filter(oid, t)),
            FilterPredicate::Or(terms) => terms.iter().any(|t| self.eval_filter(oid, t)),
            FilterPredicate::Not(inner) => !self.eval_filter(oid, inner),
        }
    }

    /// Sort a query result by a column.
    fn apply_sort(&self, result: QueryResult, column: &str, direction: SortDir) -> QueryResult {
        match result {
            QueryResult::Select { columns, mut rows } => {
                rows.sort_by(|a, b| {
                    let va = a.columns.iter().find(|(k, _)| k == column).map(|(_, v)| v);
                    let vb = b.columns.iter().find(|(k, _)| k == column).map(|(_, v)| v);
                    let cmp = compare_option_values(va, vb);
                    match direction {
                        SortDir::Asc => cmp,
                        SortDir::Desc => cmp.reverse(),
                    }
                });
                QueryResult::Select { columns, rows }
            }
            QueryResult::Aggregate { columns, mut rows } => {
                rows.sort_by(|a, b| {
                    let va = a.columns.iter().find(|(k, _)| k == column).map(|(_, v)| v);
                    let vb = b.columns.iter().find(|(k, _)| k == column).map(|(_, v)| v);
                    let cmp = compare_option_values(va, vb);
                    match direction {
                        SortDir::Asc => cmp,
                        SortDir::Desc => cmp.reverse(),
                    }
                });
                QueryResult::Aggregate { columns, rows }
            }
            other => other,
        }
    }

    /// Execute GROUP BY with aggregates.
    fn execute_group_by(
        &self,
        input: &PhysicalOp,
        group_columns: &[String],
        aggregates: &[(AggregateFunc, Option<String>)],
        having: &Option<FilterPredicate>,
    ) -> Result<QueryResult, SqlError> {
        let bitmap = self.execute_bitmap(input)?;

        // Only single-column GROUP BY for now.
        let group_col = group_columns
            .first()
            .ok_or_else(|| SqlError::Execution("GROUP BY requires at least one column".into()))?;

        let group_tag = self.resolve_tag(group_col)?;

        // Build groups: value → bitmap of objects with that value
        let mut groups: HashMap<String, RoaringBitmap> = HashMap::new();
        for obj_local in bitmap.iter() {
            let oid = ObjectId::new(0, obj_local as u64);
            if let Some(value) = self.get_attr(oid, group_tag) {
                let key = value_to_string(&value);
                groups.entry(key).or_default().insert(obj_local);
            }
        }

        // Compute aggregates for each group
        let mut col_names = vec![group_col.clone()];
        for (func, alias) in aggregates {
            col_names.push(alias.clone().unwrap_or_else(|| aggregate_name(func)));
        }

        let mut rows: Vec<GroupRow> = Vec::new();
        for (key_str, group_bitmap) in &groups {
            let key_value = Value::Text(key_str.clone());

            let mut cols = vec![(group_col.clone(), key_value.clone())];
            for (func, alias) in aggregates {
                let agg_name = alias.clone().unwrap_or_else(|| aggregate_name(func));
                let agg_value = self.compute_aggregate(func, group_bitmap)?;
                cols.push((agg_name, agg_value));
            }

            // Apply HAVING filter
            if let Some(filter) = having
                && !self.eval_having_filter(&cols, filter)
            {
                continue;
            }

            rows.push(GroupRow {
                key: key_value,
                columns: cols,
            });
        }

        Ok(QueryResult::Aggregate {
            columns: col_names,
            rows,
        })
    }

    /// Execute a scalar aggregate (no GROUP BY).
    fn execute_scalar_aggregate(
        &self,
        input: &PhysicalOp,
        aggregates: &[(AggregateFunc, Option<String>)],
    ) -> Result<QueryResult, SqlError> {
        let bitmap = self.execute_bitmap(input)?;

        if aggregates.len() == 1 {
            let value = self.compute_aggregate(&aggregates[0].0, &bitmap)?;
            return Ok(QueryResult::Scalar(value));
        }

        // Multiple aggregates → return as a single-row Select result
        let mut cols = Vec::new();
        let mut col_names = Vec::new();
        for (func, alias) in aggregates {
            let name = alias.clone().unwrap_or_else(|| aggregate_name(func));
            let value = self.compute_aggregate(func, &bitmap)?;
            col_names.push(name.clone());
            cols.push((name, value));
        }

        Ok(QueryResult::Select {
            columns: col_names,
            rows: vec![Row {
                id: ObjectId::new(0, 0),
                columns: cols,
            }],
        })
    }

    fn compute_aggregate(
        &self,
        func: &AggregateFunc,
        bitmap: &RoaringBitmap,
    ) -> Result<Value, SqlError> {
        match func {
            AggregateFunc::Count => Ok(Value::Int(bitmap.len() as i64)),

            AggregateFunc::CountDistinct(col) => {
                let tag_id = self.resolve_tag(col)?;
                let mut distinct: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for obj_local in bitmap.iter() {
                    let oid = ObjectId::new(0, obj_local as u64);
                    if let Some(val) = self.get_attr(oid, tag_id) {
                        distinct.insert(value_to_string(&val));
                    }
                }
                Ok(Value::Int(distinct.len() as i64))
            }

            AggregateFunc::Sum(col) => {
                let tag_id = self.resolve_tag(col)?;
                let mut sum: i64 = 0;
                for obj_local in bitmap.iter() {
                    let oid = ObjectId::new(0, obj_local as u64);
                    if let Some(Value::Int(v)) = self.get_attr(oid, tag_id) {
                        sum += v;
                    } else if let Some(Value::Float(v)) = self.get_attr(oid, tag_id) {
                        sum += v as i64;
                    }
                }
                Ok(Value::Int(sum))
            }

            AggregateFunc::Avg(col) => {
                let tag_id = self.resolve_tag(col)?;
                let mut sum: f64 = 0.0;
                let mut count: usize = 0;
                for obj_local in bitmap.iter() {
                    let oid = ObjectId::new(0, obj_local as u64);
                    if let Some(val) = self.get_attr(oid, tag_id) {
                        match val {
                            Value::Int(v) => {
                                sum += v as f64;
                                count += 1;
                            }
                            Value::Float(v) => {
                                sum += v;
                                count += 1;
                            }
                            _ => {}
                        }
                    }
                }
                let avg = if count > 0 { sum / count as f64 } else { 0.0 };
                Ok(Value::Float(avg))
            }

            AggregateFunc::Min(col) | AggregateFunc::Max(col) => {
                let tag_id = self.resolve_tag(col)?;
                let is_max = matches!(func, AggregateFunc::Max(_));
                let mut best: Option<Value> = None;
                for obj_local in bitmap.iter() {
                    let oid = ObjectId::new(0, obj_local as u64);
                    if let Some(val) = self.get_attr(oid, tag_id) {
                        best = Some(match best {
                            None => val,
                            Some(current) => {
                                let cmp = compare_option_values(Some(&current), Some(&val));
                                if is_max {
                                    if cmp == std::cmp::Ordering::Less {
                                        val
                                    } else {
                                        current
                                    }
                                } else if cmp == std::cmp::Ordering::Greater {
                                    val
                                } else {
                                    current
                                }
                            }
                        });
                    }
                }
                Ok(best.unwrap_or(Value::Int(0)))
            }
        }
    }

    fn eval_having_filter(&self, cols: &[(String, Value)], filter: &FilterPredicate) -> bool {
        match filter {
            FilterPredicate::Compare { column, op, value } => {
                if let Some((_, col_val)) = cols.iter().find(|(k, _)| k == column) {
                    compare_values(col_val, value, *op)
                } else {
                    false
                }
            }
            FilterPredicate::And(terms) => terms.iter().all(|t| self.eval_having_filter(cols, t)),
            FilterPredicate::Or(terms) => terms.iter().any(|t| self.eval_having_filter(cols, t)),
            FilterPredicate::Not(inner) => !self.eval_having_filter(cols, inner),
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn apply_limit(result: QueryResult, limit: usize, offset: usize) -> QueryResult {
    match result {
        QueryResult::Select { columns, rows } => {
            let rows: Vec<Row> = rows.into_iter().skip(offset).take(limit).collect();
            QueryResult::Select { columns, rows }
        }
        QueryResult::Aggregate { columns, rows } => {
            let rows: Vec<GroupRow> = rows.into_iter().skip(offset).take(limit).collect();
            QueryResult::Aggregate { columns, rows }
        }
        other => other,
    }
}

fn compare_values(a: &Value, b: &Value, op: CompareOp) -> bool {
    let ord = compare_option_values(Some(a), Some(b));
    match op {
        CompareOp::Eq => ord == std::cmp::Ordering::Equal,
        CompareOp::Ne => ord != std::cmp::Ordering::Equal,
        CompareOp::Lt => ord == std::cmp::Ordering::Less,
        CompareOp::Le => ord != std::cmp::Ordering::Greater,
        CompareOp::Gt => ord == std::cmp::Ordering::Greater,
        CompareOp::Ge => ord != std::cmp::Ordering::Less,
    }
}

fn compare_option_values(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(a), Some(b)) => compare_value_pair(a, b),
    }
}

fn compare_value_pair(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => a.cmp(b),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
        (Value::Int(a), Value::Float(b)) => (*a as f64)
            .partial_cmp(b)
            .unwrap_or(std::cmp::Ordering::Equal),
        (Value::Float(a), Value::Int(b)) => a
            .partial_cmp(&(*b as f64))
            .unwrap_or(std::cmp::Ordering::Equal),
        (Value::Text(a), Value::Text(b)) => a.cmp(b),
        (Value::Timestamp(a), Value::Timestamp(b)) => a.cmp(b),
        // Different types: order by type discriminant
        _ => std::mem::discriminant(a)
            .hash_code()
            .cmp(&std::mem::discriminant(b).hash_code()),
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Timestamp(t) => t.to_string(),
        Value::Blob(b) => format!("<blob:{}>", b.len()),
    }
}

fn aggregate_name(func: &AggregateFunc) -> String {
    match func {
        AggregateFunc::Count => "COUNT(*)".to_string(),
        AggregateFunc::CountDistinct(col) => format!("COUNT(DISTINCT {})", col),
        AggregateFunc::Sum(col) => format!("SUM({})", col),
        AggregateFunc::Avg(col) => format!("AVG({})", col),
        AggregateFunc::Min(col) => format!("MIN({})", col),
        AggregateFunc::Max(col) => format!("MAX({})", col),
    }
}

/// Simple SQL LIKE pattern matching.
/// Supports `%` (any sequence) and `_` (single char).
fn like_match(text: &str, pattern: &str) -> bool {
    let text = text.as_bytes();
    let pattern = pattern.as_bytes();
    like_match_inner(text, pattern)
}

fn like_match_inner(text: &[u8], pattern: &[u8]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }

    match pattern[0] {
        b'%' => {
            // Skip consecutive %
            let mut p = 1;
            while p < pattern.len() && pattern[p] == b'%' {
                p += 1;
            }
            let rest = &pattern[p..];
            // Try matching rest at every position in text
            for i in 0..=text.len() {
                if like_match_inner(&text[i..], rest) {
                    return true;
                }
            }
            false
        }
        b'_' => {
            if text.is_empty() {
                false
            } else {
                like_match_inner(&text[1..], &pattern[1..])
            }
        }
        ch => {
            if text.is_empty() || text[0] != ch {
                false
            } else {
                like_match_inner(&text[1..], &pattern[1..])
            }
        }
    }
}

// We need a hash_code equivalent for discriminant comparison
trait HashCode {
    fn hash_code(&self) -> u64;
}

impl<T: std::hash::Hash> HashCode for T {
    fn hash_code(&self) -> u64 {
        use std::hash::Hasher;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        mimisbrunnr_index::TagIndex,
        mimisbrunnr_ontology::{TagDefinition, TagSemantics},
        mimisbrunnr_types::TagOrigin,
    };

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    fn label(id: u32, name: &str) -> TagDefinition {
        TagDefinition::new(tag(id), name, TagSemantics::Label)
    }

    fn attr_def(id: u32, name: &str) -> TagDefinition {
        TagDefinition::new(
            tag(id),
            name,
            TagSemantics::Attribute {
                value_type: mimisbrunnr_ontology::ValueType::Text,
            },
        )
    }

    struct TestFixture {
        tag_index: TagIndex,
        kv_index: KvIndex,
        forward_index: ForwardIndex,
        dag: ImplicationDag,
    }

    impl TestFixture {
        fn new() -> Self {
            Self {
                tag_index: TagIndex::new(),
                kv_index: KvIndex::new(),
                forward_index: ForwardIndex::new(),
                dag: ImplicationDag::new(),
            }
        }

        fn executor(&self) -> SqlExecutor<'_> {
            SqlExecutor::new(
                &self.tag_index,
                &self.kv_index,
                &self.forward_index,
                &self.dag,
            )
        }

        fn register_tag(&mut self, id: u32, name: &str) -> TagId {
            self.dag.register_tag(label(id, name)).unwrap()
        }

        fn register_attr(&mut self, id: u32, name: &str) -> TagId {
            self.dag.register_tag(attr_def(id, name)).unwrap()
        }

        fn add_object_with_tag(&mut self, obj_local: u32, tag_id: TagId) {
            self.tag_index.tag_object(tag_id, obj_local);
            let oid = ObjectId::new(0, obj_local as u64);
            self.forward_index
                .add(oid, Assertion::Tag(tag_id), TagOrigin::Direct);
        }

        fn set_attr(&mut self, obj_local: u32, key: TagId, value: Value) {
            let oid = ObjectId::new(0, obj_local as u64);
            self.kv_index.insert(key, &value, obj_local);
            self.forward_index
                .add(oid, Assertion::Attr { key, value }, TagOrigin::Direct);
        }
    }

    #[test]
    fn execute_simple_tag_query() {
        let mut f = TestFixture::new();
        let electronic = f.register_tag(1, "electronic");
        f.add_object_with_tag(1, electronic);
        f.add_object_with_tag(2, electronic);
        f.add_object_with_tag(3, electronic);

        let bitmap_op = PhysicalOp::TagBitmap("electronic".into());
        let result = f.executor().execute(&bitmap_op).unwrap();

        assert_eq!(result.row_count(), 3);
    }

    #[test]
    fn execute_intersect() {
        let mut f = TestFixture::new();
        let electronic = f.register_tag(1, "electronic");
        let portable = f.register_tag(2, "portable");

        f.add_object_with_tag(1, electronic);
        f.add_object_with_tag(2, electronic);
        f.add_object_with_tag(3, electronic);
        f.add_object_with_tag(2, portable);
        f.add_object_with_tag(3, portable);

        let plan = PhysicalOp::Intersect(vec![
            PhysicalOp::TagBitmap("electronic".into()),
            PhysicalOp::TagBitmap("portable".into()),
        ]);
        let result = f.executor().execute(&plan).unwrap();
        assert_eq!(result.row_count(), 2);
    }

    #[test]
    fn execute_with_projection() {
        let mut f = TestFixture::new();
        let electronic = f.register_tag(1, "electronic");
        let name_attr = f.register_attr(10, "name");

        f.add_object_with_tag(1, electronic);
        f.set_attr(1, name_attr, Value::Text("synth".into()));

        f.add_object_with_tag(2, electronic);
        f.set_attr(2, name_attr, Value::Text("drum machine".into()));

        let plan = PhysicalOp::Project(
            Box::new(PhysicalOp::TagBitmap("electronic".into())),
            vec![
                Projection::Column("id".into()),
                Projection::Column("name".into()),
            ],
        );

        let result = f.executor().execute(&plan).unwrap();
        match result {
            QueryResult::Select { columns, rows } => {
                assert_eq!(columns, vec!["id", "name"]);
                assert_eq!(rows.len(), 2);
                // First row should have name = "synth"
                let name_val = rows[0]
                    .columns
                    .iter()
                    .find(|(k, _)| k == "name")
                    .map(|(_, v)| v);
                assert_eq!(name_val, Some(&Value::Text("synth".into())));
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn execute_count() {
        let mut f = TestFixture::new();
        let electronic = f.register_tag(1, "electronic");
        for i in 1..=5 {
            f.add_object_with_tag(i, electronic);
        }

        let plan = PhysicalOp::ScalarAggregate {
            input: Box::new(PhysicalOp::TagBitmap("electronic".into())),
            aggregates: vec![(AggregateFunc::Count, None)],
        };

        let result = f.executor().execute(&plan).unwrap();
        assert_eq!(result, QueryResult::Scalar(Value::Int(5)));
    }

    #[test]
    fn execute_limit_offset() {
        let mut f = TestFixture::new();
        let electronic = f.register_tag(1, "electronic");
        for i in 1..=10 {
            f.add_object_with_tag(i, electronic);
        }

        let plan = PhysicalOp::Limit(Box::new(PhysicalOp::TagBitmap("electronic".into())), 3, 2);

        let result = f.executor().execute(&plan).unwrap();
        assert_eq!(result.row_count(), 3);
    }

    #[test]
    fn like_matching() {
        assert!(like_match("kernel_main", "kernel%"));
        assert!(like_match("kernel_init", "kernel%"));
        assert!(!like_match("boot_kernel", "kernel%"));
        assert!(like_match("hello", "%llo"));
        assert!(like_match("hello", "%ll%"));
        assert!(like_match("abc", "a_c"));
        assert!(!like_match("abbc", "a_c"));
        assert!(like_match("anything", "%"));
        assert!(like_match("", "%"));
        assert!(!like_match("", "_"));
    }
}
