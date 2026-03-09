/// Common error types for the Mímisbrunnr filesystem.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("object not found: {0}")]
    ObjectNotFound(crate::ObjectId),

    #[error("tag not found: {0}")]
    TagNotFound(crate::TagId),

    #[error("invalid object state: expected {expected:?}, found {found:?}")]
    InvalidObjectState {
        expected: crate::ObjectState,
        found: crate::ObjectState,
    },

    #[error("storage I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("data corruption: {message}")]
    Corruption { message: String },

    #[error("capacity exceeded: {message}")]
    CapacityExceeded { message: String },

    #[error("invalid argument: {message}")]
    InvalidArgument { message: String },

    #[error("ontology violation: {message}")]
    OntologyViolation { message: String },

    #[error("cycle detected in implication DAG")]
    CycleDetected,

    #[error("context not found: {0}")]
    ContextNotFound(String),

    #[error("context already exists: {0}")]
    ContextAlreadyExists(String),

    #[error("path not found in context {context}: {path}")]
    PathNotFound { context: String, path: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObjectId;

    #[test]
    fn error_display() {
        let e = Error::ObjectNotFound(ObjectId::new(1, 42));
        assert_eq!(format!("{e}"), "object not found: obj:1:42");
    }

    #[test]
    fn io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let e: Error = io_err.into();
        assert!(matches!(e, Error::Io(_)));
    }
}
