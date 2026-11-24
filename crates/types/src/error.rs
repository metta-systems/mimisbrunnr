//! Crate-level error type.
//!
//! Per `doc/agentic/operations-design.md` (cross-cutting choices), every crate exposes a single
//! `thiserror`-derived error enum. Public APIs return `Result<T, TypesError>`.

use thiserror::Error;

/// Errors produced by `mimisbrunnr-types`.
#[derive(Debug, Error)]
pub enum TypesError {
    /// The input ended inside a CBOR item.
    #[error("CBOR input truncated")]
    CborTruncated,

    /// Bytes remained after the item that should have been the whole input.
    #[error("{0} trailing bytes after the CBOR item")]
    CborTrailingBytes(usize),

    /// An item of another type than the format expects; `found` is its
    /// initial byte, `None` for a missing element.
    #[error("CBOR: expected {expected}, found initial byte {found:?}")]
    CborUnexpected {
        expected: &'static str,
        found: Option<u8>,
    },

    /// A head longer than its value needs, an indefinite length, or a float
    /// that is not 64-bit (FORMAT-DESIGN§1.6, INDEX-DESIGN§1.1).
    #[error("CBOR item not in canonical form")]
    CborNonCanonical,

    /// An integer outside the range of the field it decodes into.
    #[error("CBOR integer out of range")]
    CborIntegerRange,

    /// A text string that is not valid UTF-8.
    #[error("CBOR text is not valid UTF-8")]
    CborInvalidUtf8,

    /// A record written by a newer schema version than this reader knows.
    #[error("record schema version {found} is newer than supported version {supported}")]
    RecordVersionTooNew { found: u16, supported: u16 },

    /// A record with more fields than its schema version defines.
    #[error("record carries {0} fields its schema version does not define")]
    RecordExtraFields(usize),

    /// An enum variant index the format does not assign.
    #[error("{type_name}: unassigned variant index {index}")]
    UnknownVariant { type_name: &'static str, index: u64 },

    /// An array with the wrong number of elements for its type.
    #[error("{type_name}: expected {expected} elements, found {found}")]
    WrongElementCount {
        type_name: &'static str,
        expected: usize,
        found: usize,
    },

    /// NaN is not an attribute value (INDEX-DESIGN§1.1).
    #[error("NaN is not a valid attribute value")]
    NanValue,
}
