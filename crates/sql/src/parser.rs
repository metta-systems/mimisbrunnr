//! Thin wrapper over [`sqlparser`] that validates a single `SELECT` statement
//! and hands the AST node to the planner.

use sqlparser::{ast::Statement, dialect::GenericDialect, parser::Parser};

use crate::error::SqlError;

/// Parser entrypoint. Validates that the input is exactly one statement and
/// that it is a `SELECT`. Returns the underlying `sqlparser` AST node so the
/// planner can walk it directly.
pub struct SqlParser;

impl SqlParser {
    /// Parse `sql` and return the lone [`Statement`].
    pub fn parse(sql: &str) -> Result<Statement, SqlError> {
        let dialect = GenericDialect {};
        let mut statements = Parser::parse_sql(&dialect, sql)?;
        if statements.is_empty() {
            return Err(SqlError::ParseError("empty input".into()));
        }
        if statements.len() > 1 {
            return Err(SqlError::Unsupported(
                "multiple statements in one input".into(),
            ));
        }
        let stmt = statements.swap_remove(0);
        match &stmt {
            Statement::Query(_) => Ok(stmt),
            other => Err(SqlError::Unsupported(format!(
                "only SELECT is supported, got: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_select() {
        let stmt = SqlParser::parse("SELECT * FROM objects").unwrap();
        assert!(matches!(stmt, Statement::Query(_)));
    }

    #[test]
    fn rejects_insert() {
        let err = SqlParser::parse("INSERT INTO objects VALUES (1)").unwrap_err();
        assert!(matches!(err, SqlError::Unsupported(_)));
    }

    #[test]
    fn rejects_multi_statement() {
        let err = SqlParser::parse("SELECT * FROM objects; SELECT * FROM objects;").unwrap_err();
        assert!(matches!(err, SqlError::Unsupported(_)));
    }

    #[test]
    fn rejects_garbage() {
        let err = SqlParser::parse("not sql at all !!!").unwrap_err();
        assert!(matches!(err, SqlError::ParseError(_)));
    }
}
