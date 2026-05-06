//! Errors emitted by `mimisbrunnr-index`.

use thiserror::Error;

/// Errors returned by the index crate.
#[derive(Debug, Error)]
pub enum IndexError {
    /// Buffer too small for the requested parse.
    #[error("buffer too small: need {need}, have {have}")]
    BufferTooSmall { need: usize, have: usize },

    /// Inline assertion count exceeds the 15-bit ceiling
    /// ([`crate::LEAF_ENTRY_TOTAL_MASK`]).
    #[error("assertion count {count} exceeds 15-bit ceiling (32K)")]
    AssertionCountOverflow { count: usize },

    /// Unknown `PackedAssertion::kind` discriminant.
    #[error("unknown packed-assertion kind: {0}")]
    InvalidAssertionKind(u8),

    /// Unknown `PackedAssertion::origin` discriminant.
    #[error("unknown packed-assertion origin: {0}")]
    InvalidAssertionOrigin(u8),

    /// Attempted operation on a tag that is not present in the index.
    #[error("tag not found")]
    TagNotFound,

    /// Attempted operation on an object that is not present in the index.
    #[error("object not found")]
    ObjectNotFound,

    /// CBOR encode failure.
    #[error("CBOR encode error: {0}")]
    CborEncode(String),

    /// CBOR decode failure.
    #[error("CBOR decode error: {0}")]
    CborDecode(String),

    /// Roaring bitmap (de)serialisation failure.
    #[error("roaring bitmap error: {0}")]
    Roaring(String),
}
