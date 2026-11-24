//! Error type for the WAL crate.

use mimisbrunnr_storage::StorageError;

/// All errors emitted by the WAL.
#[derive(Debug, thiserror::Error)]
pub enum WalError {
    /// Underlying storage error.
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    /// I/O error not yet wrapped by the storage layer.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A payload that is not a whole number of 8-byte words.
    #[error("op {op_kind} payload of {length} bytes is not a multiple of 8")]
    PayloadNotWordAligned { op_kind: u8, length: usize },

    /// A payload whose fields, counts or lengths run past its end.
    #[error("op {op_kind} payload needs {needed} bytes, has {available}")]
    PayloadTruncated {
        op_kind: u8,
        needed: usize,
        available: usize,
    },

    /// Padding inside or after a payload record that is not zero.
    #[error("op {op_kind} payload has non-zero padding at byte {offset}")]
    PayloadPaddingNotZero { op_kind: u8, offset: usize },

    /// A payload field outside its allowed values.
    #[error("invalid payload field {field}")]
    PayloadFieldInvalid { field: &'static str },

    /// A variable part longer than its cap.
    #[error("payload field {field} is {length} long, max {max}")]
    PayloadFieldTooLong {
        field: &'static str,
        length: usize,
        max: usize,
    },

    /// An attribute `Value` tail that does not encode or decode (INDEX-DESIGN§1.1).
    #[error("value: {0}")]
    Value(#[from] mimisbrunnr_types::TypesError),

    /// Unknown WAL op kind discriminant.
    #[error("invalid wal op kind: {0}")]
    InvalidOpKind(u8),

    /// An entry too large for one 256 KiB journal set.
    #[error("payload too large: {size} bytes (max {max})")]
    PayloadTooLarge { size: usize, max: usize },

    /// Every journal bucket still holds jsets replay may need.
    #[error("journal full: all {buckets} buckets hold jsets newer than the trim point")]
    JournalFull { buckets: usize },

    /// A checksummed jset whose entries do not parse.
    #[error("journal set {seq} malformed: {reason}")]
    JsetMalformed { seq: u64, reason: &'static str },

    /// `seq` past 2⁴⁸, where LSNs would overflow.
    #[error("lsn overflow")]
    LsnOverflow,

    /// A journal bucket list or bucket size the journal cannot use.
    #[error("invalid journal geometry: {0}")]
    InvalidGeometry(&'static str),

    /// An encrypted journal set (ENCRYPTION3).
    #[error("encrypted journal sets not yet supported")]
    EncryptionUnsupported,

    /// A compressed journal set (JOURNAL5).
    #[error("compressed journal sets not yet supported")]
    CompressionUnsupported,

    /// Mutating call attempted on a WAL opened read-only.
    #[error("wal is read-only")]
    ReadOnly,
}
