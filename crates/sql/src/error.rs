#[derive(Debug, thiserror::Error)]
pub enum SqlError {
    #[error("parse error: {0}")]
    Parse(String),

    #[error("unsupported SQL: {0}")]
    Unsupported(String),

    #[error("unknown tag: {0}")]
    UnknownTag(String),

    #[error("unknown attribute: {0}")]
    UnknownAttr(String),

    #[error("type error: {0}")]
    TypeError(String),

    #[error("execution error: {0}")]
    Execution(String),
}

impl From<sqlparser::parser::ParserError> for SqlError {
    fn from(e: sqlparser::parser::ParserError) -> Self {
        SqlError::Parse(e.to_string())
    }
}
