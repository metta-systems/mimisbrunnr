//! Errors emitted by `mimisbrunnr-index`.

use mimisbrunnr_storage::StorageError;
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

    /// Underlying storage error (B+ tree region read/write).
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    /// Flush attempted on a `TagStore` variant whose native on-disk shape
    /// (§8.3 OrderedStore / RankedStore root blocks) isn't implemented
    /// yet. A3.3 ships Simple-only; A3.4/A3.5 will lift this.
    #[error("tag store kind {0} is not yet supported by the native flush path")]
    UnsupportedStoreKind(u8),

    /// A tag's membership bitmap holds > 2³² entries and can't fit the
    /// 32-bit `cardinality` slot in `TagIndexLeafEntry` (§8.1).
    #[error("tag cardinality {0} exceeds u32::MAX")]
    CardinalityOverflow(u64),

    /// Tag-bitmap-page chain ran beyond the bitmap-area cap (smoke-test
    /// scale: 1 024 pages per A3.3 layout).
    #[error("tag bitmap area exhausted: needed page slot {needed}, cap {cap}")]
    BitmapAreaExhausted { needed: usize, cap: usize },
}
