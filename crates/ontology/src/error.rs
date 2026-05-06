//! Error type for the ontology crate.

use mimisbrunnr_types::{ModuleId, TagId};

/// Storage axis used in policy-resolution diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageAxis {
    Chunking,
    Compression,
    Encryption,
}

impl std::fmt::Display for StorageAxis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageAxis::Chunking => f.write_str("chunking"),
            StorageAxis::Compression => f.write_str("compression"),
            StorageAxis::Encryption => f.write_str("encryption"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OntologyError {
    #[error("tag not found: {0}")]
    TagNotFound(TagId),

    #[error("tag name not found: {0}")]
    TagNameNotFound(String),

    #[error("duplicate tag name: {0}")]
    DuplicateTagName(String),

    #[error("cycle detected in implication DAG: {from} → {to}")]
    CycleDetected { from: TagId, to: TagId },

    #[error("unknown tag: {0}")]
    UnknownTag(String),

    #[error(
        "axis {axis} declared by unrelated tags `{a_name}` (id {a}) and `{b_name}` (id {b}); \
         neither implies the other"
    )]
    AxisConflict {
        axis: StorageAxis,
        a: TagId,
        a_name: String,
        b: TagId,
        b_name: String,
    },

    #[error("module already installed: {0}")]
    ModuleAlreadyInstalled(ModuleId),

    #[error("module not installed: {0}")]
    ModuleNotInstalled(ModuleId),

    #[error("module parse error: {0}")]
    ModuleParse(String),

    #[error("module serialise error: {0}")]
    ModuleSerialise(String),

    #[error("CBOR error: {0}")]
    Cbor(String),
}
