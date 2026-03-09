#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("subscription not found: {0}")]
    NotFound(u64),

    #[error("subscription already exists: {0}")]
    AlreadyExists(String),

    #[error("query error: {0}")]
    Query(#[from] mimisbrunnr_query::QueryError),
}
