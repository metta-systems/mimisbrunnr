#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("storage error: {0}")]
    Storage(#[from] mimisbrunnr_storage::StorageError),

    #[error("WAL error: {0}")]
    Wal(#[from] mimisbrunnr_wal::WalError),

    #[error("metadata error: {0}")]
    Meta(#[from] mimisbrunnr_meta::MetaError),

    #[error("ontology error: {0}")]
    Ontology(#[from] mimisbrunnr_ontology::OntologyError),

    #[error("transform error: {0}")]
    Transform(#[from] mimisbrunnr_transform::TransformError),

    #[error("query error: {0}")]
    Query(#[from] mimisbrunnr_query::QueryError),

    #[error("object not found: {0}")]
    ObjectNotFound(mimisbrunnr_types::ObjectId),

    #[error("object already deleted: {0}")]
    ObjectDeleted(mimisbrunnr_types::ObjectId),

    #[error("engine not initialized")]
    NotInitialized,

    #[error("I/O error: {0}")]
    Io(std::io::Error),
}
