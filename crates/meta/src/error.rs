//! `MetaError` — public error type for the `mimisbrunnr-meta` crate.
//!
//! This phase only implements the on-disk struct definitions, address-translation
//! helpers, and in-memory placeholder tables; consequently the error variants
//! are limited to decoding / parsing failures. Storage- and journal-level
//! variants will be added in a later rewrite phase along with the live radix
//! tree / B+ tree integration.

use {mimisbrunnr_storage::StorageError, thiserror::Error};

/// Errors produced by `mimisbrunnr-meta`.
#[derive(Debug, Error)]
pub enum MetaError {
    /// Underlying storage-layer failure (B+ tree region read/write, bucket
    /// allocator, raw block device).
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    /// An on-disk `u8` field carried a discriminant that does not map to a
    /// known [`mimisbrunnr_types::ObjectState`] variant.
    #[error("invalid ObjectState discriminant: {0}")]
    InvalidObjectState(u8),

    /// An on-disk `u8` field carried a discriminant that does not map to a
    /// known [`mimisbrunnr_types::CompressionState`] variant.
    #[error("invalid CompressionState discriminant: {0}")]
    InvalidCompressionState(u8),

    /// An on-disk `u8` field carried a discriminant that does not map to a
    /// known [`mimisbrunnr_types::EncryptionState`] variant.
    #[error("invalid EncryptionState discriminant: {0}")]
    InvalidEncryptionState(u8),

    /// An on-disk `u8` field carried a discriminant that does not map to a
    /// known [`crate::backpointer::OwnerKind`] variant.
    #[error("invalid OwnerKind discriminant: {0}")]
    InvalidOwnerKind(u8),

    /// `ObjectLocation::parse` was given a buffer that is shorter than
    /// `LOCATION_HEADER_SIZE` or that does not contain enough trailing bytes
    /// for the declared `replica_count`.
    #[error("buffer too small: needed {needed} bytes, got {got}")]
    BufferTooSmall {
        /// Bytes required by the parser.
        needed: usize,
        /// Bytes actually supplied.
        got: usize,
    },

    /// `ObjectLocation` declared a `replica_count` outside the legal range
    /// `0..=4` (the inline cap; see DESIGN §6.3 / IMPL §6.1).
    #[error("invalid replica_count: {0} (must be 0..=4)")]
    InvalidReplicaCount(u8),

    /// An object id (or local sequence) overflowed the radix tree's
    /// addressable range at the requested `root_level`. The caller should
    /// grow the tree first (IMPL §5 "Tree growth").
    #[error("oid_local {oid_local} exceeds radix tree range at root_level {root_level}")]
    OidOutOfRange {
        /// The local sequence (low 48 bits of `ObjectId`) that did not fit.
        oid_local: u64,
        /// The root-level depth for which the translation was attempted.
        root_level: u8,
    },
}
