//! Errors raised by `mimisbrunnr-unix` (DESIGN §12).

use std::io;
use std::path::PathBuf;

use mimisbrunnr_types::TagId;
use thiserror::Error;

/// Errors raised by path-projection / import / export logic.
#[derive(Debug, Error)]
pub enum UnixError {
    /// Tried to register an empty relative path.
    #[error("empty relative path")]
    EmptyPath,

    /// Tried to register an absolute path (must be relative).
    #[error("absolute path not allowed: {0}")]
    AbsolutePath(PathBuf),

    /// Tried to register a path containing a `..` traversal component.
    #[error("path contains parent-directory traversal: {0}")]
    ParentTraversal(PathBuf),

    /// Tried to register a path with a non-UTF-8 component.
    #[error("path contains non-UTF-8 component: {0}")]
    NonUtf8Path(PathBuf),

    /// Path is not allowed to contain interior NUL bytes.
    #[error("path contains NUL byte: {0}")]
    NulInPath(PathBuf),

    /// `create_context` was called with a tag id that already has a
    /// projection.
    #[error("path context {0} already exists")]
    ContextExists(TagId),

    /// `create_context` was called with a tag whose ontology semantics is not
    /// `Grouping`.
    #[error("tag {0} is not a Grouping tag and cannot be a path context")]
    NotAGroupingTag(TagId),

    /// `create_context` was given a tag id that the ontology does not know
    /// about.
    #[error("unknown tag {0}")]
    UnknownTag(TagId),

    /// I/O error during import scan or export materialisation.
    #[error("io: {0}")]
    Io(#[from] io::Error),

    /// CBOR encode / decode error during persistence.
    #[error("cbor: {0}")]
    Cbor(String),

    /// Content provider returned `None` for an object during export.
    #[error("missing blob content for object {0}")]
    MissingContent(u64),
}
