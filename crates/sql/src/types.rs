use mimisbrunnr_types::{ObjectId, Value};

/// A single row in a query result.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: ObjectId,
    pub columns: Vec<(String, Value)>,
}

/// Aggregate result row (for GROUP BY queries).
#[derive(Debug, Clone, PartialEq)]
pub struct GroupRow {
    pub key: Value,
    pub columns: Vec<(String, Value)>,
}

/// The result of executing a SQL query.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryResult {
    /// SELECT query result: column names + rows.
    Select {
        columns: Vec<String>,
        rows: Vec<Row>,
    },
    /// Aggregate query result (GROUP BY).
    Aggregate {
        columns: Vec<String>,
        rows: Vec<GroupRow>,
    },
    /// COUNT(*) or other scalar aggregate.
    Scalar(Value),
}

impl QueryResult {
    pub fn row_count(&self) -> usize {
        match self {
            QueryResult::Select { rows, .. } => rows.len(),
            QueryResult::Aggregate { rows, .. } => rows.len(),
            QueryResult::Scalar(_) => 1,
        }
    }
}
