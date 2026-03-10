use mimisbrunnr_types::Value;

/// A parsed SQL statement in Mímisbrunnr's dialect.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    Select(SelectQuery),
}

/// A SELECT query.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectQuery {
    /// Columns to project (empty = all / `*`).
    pub projection: Vec<Projection>,
    /// WHERE clause predicates.
    pub filter: Option<Predicate>,
    /// GROUP BY columns.
    pub group_by: Vec<String>,
    /// HAVING clause (post-aggregation filter).
    pub having: Option<Predicate>,
    /// ORDER BY clauses.
    pub order_by: Vec<OrderBy>,
    /// LIMIT.
    pub limit: Option<usize>,
    /// OFFSET.
    pub offset: Option<usize>,
    /// Context modifier: IN CONTEXT 'name'.
    pub context: Option<String>,
    /// Collection modifier: IN COLLECTION 'name'.
    pub collection: Option<String>,
}

/// A projection column in SELECT.
#[derive(Debug, Clone, PartialEq)]
pub enum Projection {
    /// `*` — all attributes.
    Star,
    /// A named column: `name`, `artist`, `id`.
    Column(String),
    /// Aliased column: `COUNT(*) AS total`.
    Aliased {
        expr: Box<Projection>,
        alias: String,
    },
    /// Aggregate function: COUNT(*), SUM(size), AVG(bpm), MIN(x), MAX(x).
    Aggregate(AggregateFunc),
}

/// Aggregate functions.
#[derive(Debug, Clone, PartialEq)]
pub enum AggregateFunc {
    Count,
    CountDistinct(String),
    Sum(String),
    Avg(String),
    Min(String),
    Max(String),
}

/// A predicate in WHERE or HAVING.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    /// `HAS TAG 'x'`
    HasTag(String),
    /// `HAS ALL TAGS ('x', 'y')`
    HasAllTags(Vec<String>),
    /// `HAS ANY TAG ('x', 'y')`
    HasAnyTag(Vec<String>),
    /// `IS A 'x'` — ontology-aware.
    IsA(String),
    /// `attr = value`, `attr > value`, etc.
    Compare {
        column: String,
        op: CompareOp,
        value: Value,
    },
    /// `attr LIKE pattern`
    Like {
        column: String,
        pattern: String,
    },
    /// `attr IN (v1, v2, ...)`
    In {
        column: String,
        values: Vec<Value>,
    },
    /// `AND`
    And(Vec<Predicate>),
    /// `OR`
    Or(Vec<Predicate>),
    /// `NOT`
    Not(Box<Predicate>),
}

/// Comparison operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// ORDER BY clause.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderBy {
    pub column: String,
    pub direction: SortDir,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}
