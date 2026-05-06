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

    /// CBOR encoding error.
    #[error("cbor encode: {0}")]
    CborEncode(String),

    /// CBOR decoding error.
    #[error("cbor decode: {0}")]
    CborDecode(String),

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

    /// `next_lsn` overflow — should never happen in practice (IMPL §14).
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

impl<T> From<ciborium::ser::Error<T>> for WalError
where
    T: std::fmt::Debug,
{
    fn from(value: ciborium::ser::Error<T>) -> Self {
        WalError::CborEncode(format!("{value:?}"))
    }
}

impl<T> From<ciborium::de::Error<T>> for WalError
where
    T: std::fmt::Debug,
{
    fn from(value: ciborium::de::Error<T>) -> Self {
        WalError::CborDecode(format!("{value:?}"))
    }
}
