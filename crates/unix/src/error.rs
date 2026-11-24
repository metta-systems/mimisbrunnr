//! Errors raised by `mimisbrunnr-unix` (SPECIFICATION-DESIGN§12).

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Errors raised by path-projection and import logic.
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

    /// I/O error during import scan.
    #[error("io: {0}")]
    Io(#[from] io::Error),

    /// CBOR encode / decode error during persistence.
    #[error("cbor: {0}")]
    Cbor(String),
}
