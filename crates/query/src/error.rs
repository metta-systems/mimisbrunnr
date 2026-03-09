#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("parse error at position {position}: {message}")]
    Parse { position: usize, message: String },

    #[error("unknown tag name: {0}")]
    UnknownTag(String),

    #[error("empty query")]
    EmptyQuery,
}
