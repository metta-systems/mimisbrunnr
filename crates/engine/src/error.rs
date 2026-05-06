//! Engine-level error type.
//!
//! Wraps the per-crate errors of every dependency the engine talks to, plus a
//! few engine-internal cases.

use mimisbrunnr_index::IndexError;
use mimisbrunnr_meta::MetaError;
use mimisbrunnr_ontology::OntologyError;
use mimisbrunnr_pool::PoolError;
use mimisbrunnr_query::QueryError;
use mimisbrunnr_storage::StorageError;
use mimisbrunnr_transform::TransformError;
use mimisbrunnr_types::ObjectId;
use mimisbrunnr_unix::UnixError;
use mimisbrunnr_wal::WalError;
use mimisbrunnr_watch::WatchError;
use thiserror::Error;

/// All errors emitted by the engine layer.
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("storage: {0}")]
    Storage(#[from] StorageError),

    #[error("wal: {0}")]
    Wal(#[from] WalError),

    #[error("meta: {0}")]
    Meta(#[from] MetaError),

    #[error("index: {0}")]
    Index(#[from] IndexError),

    #[error("ontology: {0}")]
    Ontology(#[from] OntologyError),

    #[error("pool: {0}")]
    Pool(#[from] PoolError),

    #[error("transform: {0}")]
    Transform(#[from] TransformError),

    #[error("query: {0}")]
    Query(#[from] QueryError),

    #[error("watch: {0}")]
    Watch(#[from] WatchError),

    #[error("unix: {0}")]
    Unix(#[from] UnixError),

    #[error("cbor: {0}")]
    Cbor(String),

    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    #[error("object not found: {0}")]
    ObjectNotFound(ObjectId),

    #[error("LSN {0} already applied")]
    LsnAlreadyApplied(u64),

    #[error("engine is read-only")]
    ReadOnly,

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl<T> From<ciborium::ser::Error<T>> for EngineError
where
    T: std::fmt::Debug,
{
    fn from(value: ciborium::ser::Error<T>) -> Self {
        EngineError::Cbor(format!("encode: {value:?}"))
    }
}

impl<T> From<ciborium::de::Error<T>> for EngineError
where
    T: std::fmt::Debug,
{
    fn from(value: ciborium::de::Error<T>) -> Self {
        EngineError::Cbor(format!("decode: {value:?}"))
    }
}
