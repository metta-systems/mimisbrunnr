//! Errors emitted by the SQL crate's public surface.

use thiserror::Error;

use mimisbrunnr_query::QueryError;

/// All errors emitted by the SQL crate's public surface.
#[derive(Debug, Error)]
pub enum SqlError {
    /// `sqlparser` could not consume the input.
    #[error("parse error: {0}")]
    ParseError(String),

    /// The statement is syntactically valid SQL but its shape is outside the
    /// Phase 5a subset (joins, subqueries, ORDER BY, GROUP BY beyond
    /// `COUNT(*)`, INSERT/UPDATE/DELETE, multiple statements, …).
    #[error("unsupported SQL construct: {0}")]
    Unsupported(String),

    /// A tag name in the WHERE clause does not exist in the supplied
    /// [`mimisbrunnr_ontology::OntologyState`].
    #[error("unknown tag `{0}`")]
    UnknownTag(String),

    /// A `LIKE` pattern uses wildcard semantics outside the Phase 5a subset
    /// (only `'foo%'` prefix and `'%foo'` contains are accepted).
    #[error("unsupported LIKE pattern `{0}`")]
    UnsupportedPattern(String),

    /// Propagated from `mimisbrunnr_query::QueryExecutor`.
    #[error("query execution error: {0}")]
    Execute(#[from] QueryError),
}

impl From<sqlparser::parser::ParserError> for SqlError {
    fn from(e: sqlparser::parser::ParserError) -> Self {
        SqlError::ParseError(e.to_string())
    }
}
