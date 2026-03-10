#[derive(Debug, thiserror::Error)]
pub enum OntologyError {
    #[error("tag not found: {0}")]
    TagNotFound(mimisbrunnr_types::TagId),

    #[error("tag name not found: {0}")]
    TagNameNotFound(String),

    #[error("duplicate tag name: {0}")]
    DuplicateTagName(String),

    #[error("cycle detected in implication DAG: {0} → {1}")]
    CycleDetected(mimisbrunnr_types::TagId, mimisbrunnr_types::TagId),

    #[error("mutex violation: tags {0} and {1} are mutually exclusive")]
    MutexViolation(mimisbrunnr_types::TagId, mimisbrunnr_types::TagId),

    #[error("unknown tag: {0}")]
    UnknownTag(String),

    #[error("module parse error: {0}")]
    ModuleParse(String),
}
