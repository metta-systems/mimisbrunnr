//! Error types for the storage crate.

/// All errors emitted by the storage layer.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid magic: expected {expected:?}, got {actual:?}")]
    InvalidMagic { expected: [u8; 4], actual: [u8; 4] },

    #[error("invalid full magic for superblock")]
    InvalidFullMagic,

    #[error("invalid block kind: {0}")]
    InvalidBlockKind(u16),

    #[error("invalid btree kind: {0}")]
    InvalidBtreeKind(u16),

    #[error("invalid bucket data type: {0}")]
    InvalidBucketDataType(u8),

    #[error("invalid media type discriminant: {0}")]
    InvalidMediaType(u8),

    #[error("invalid storage tier discriminant: {0}")]
    InvalidStorageTier(u8),

    #[error("crc mismatch: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch { expected: u32, actual: u32 },

    #[error("unsupported format version: {0}")]
    UnsupportedFormatVersion(u32),

    #[error("buffer too small: need {need} bytes, have {have}")]
    BufferTooSmall { need: usize, have: usize },

    #[error("device too small: need {need} bytes, have {have}")]
    DeviceTooSmall { need: u64, have: u64 },

    #[error(
        "offset {offset} + length {length} exceeds device capacity {capacity}"
    )]
    OutOfBounds { offset: u64, length: u64, capacity: u64 },

    #[error("no valid superblock copy found")]
    NoValidSuperblock,
}
