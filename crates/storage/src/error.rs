#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid superblock magic")]
    InvalidMagic,

    #[error("superblock checksum mismatch: expected {expected:#010x}, got {actual:#010x}")]
    ChecksumMismatch { expected: u32, actual: u32 },

    #[error("unsupported version: {0}")]
    UnsupportedVersion(u32),

    #[error("device too small: need {need} bytes, have {have}")]
    DeviceTooSmall { need: u64, have: u64 },

    #[error("offset {offset} + length {length} exceeds device capacity {capacity}")]
    OutOfBounds {
        offset: u64,
        length: u64,
        capacity: u64,
    },

    #[error("no free extents of size {requested} blocks")]
    NoFreeSpace { requested: u64 },

    #[error("capacity exceeded: {message}")]
    CapacityExceeded { message: String },
}
