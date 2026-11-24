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

    /// Magic mismatch on a parse.
    #[error("invalid magic: expected {expected:?}, got {actual:?}")]
    InvalidMagic { expected: [u8; 4], actual: [u8; 4] },

    /// CRC mismatch (header, payload, or framing CRC).
    #[error("crc mismatch: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch { expected: u32, actual: u32 },

    /// Unknown WAL op kind discriminant.
    #[error("invalid wal op kind: {0}")]
    InvalidOpKind(u8),

    /// Payload too large to fit in a single 4 KiB sector.
    #[error("payload too large: {size} bytes (max {max})")]
    PayloadTooLarge { size: usize, max: usize },

    /// WAL ring is full — appending would overwrite unreclaimed entries.
    #[error("wal ring full: used {used}, capacity {capacity}")]
    RingFull { used: u64, capacity: u64 },

    /// `next_lsn` overflow — should never happen in practice (ENCRYPTION-DESIGN§1).
    #[error("lsn overflow")]
    LsnOverflow,

    /// Neither A nor B WAL header copy was usable.
    #[error("no valid wal header copy found")]
    NoValidHeader,

    /// Unsupported format version.
    #[error("unsupported wal format version: {0}")]
    UnsupportedFormatVersion(u16),

    /// Configuration error.
    #[error("invalid wal size: {0}")]
    InvalidSize(u64),

    /// Encrypted entry support is not yet implemented in this phase.
    #[error("encrypted wal entries not yet supported")]
    EncryptionUnsupported,

    /// Compressed entry support is not yet implemented in this phase.
    #[error("compressed wal entries not yet supported")]
    CompressionUnsupported,

    /// Mutating call attempted on a WAL opened read-only.
    #[error("wal is read-only")]
    ReadOnly,
}
