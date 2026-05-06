//! Error type for the pool crate.

use mimisbrunnr_types::DiskId;

/// All errors emitted by the pool layer.
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("storage error: {0}")]
    Storage(#[from] mimisbrunnr_storage::StorageError),

    #[error("TOML parse error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("TOML serialise error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("CBOR encode error: {0}")]
    CborEncode(String),

    #[error("CBOR decode error: {0}")]
    CborDecode(String),

    #[error("disk path too long: {len} bytes (max {max})")]
    DiskPathTooLong { len: usize, max: usize },

    #[error("invalid disk path UTF-8")]
    InvalidDiskPathUtf8,

    #[error("invalid media type discriminant: {0}")]
    InvalidMediaType(u8),

    #[error("invalid tier discriminant: {0}")]
    InvalidStorageTier(u8),

    #[error("invalid disk state discriminant: {0}")]
    InvalidDiskState(u8),

    #[error("pool config has no disks")]
    EmptyPool,

    #[error("disk id {0} not found in pool")]
    DiskNotFound(DiskId),

    #[error("disk id {0} already present in pool")]
    DuplicateDiskId(DiskId),

    #[error("more than {max} disks: {count}; B+ tree spillover not yet implemented")]
    DiskOverflowUnsupported { count: u32, max: u32 },

    #[error("node id mismatch on disk {disk}: pool says {pool}, disk says {disk_says}")]
    NodeIdMismatch {
        disk: DiskId,
        pool: u16,
        disk_says: u16,
    },

    #[error("disk id mismatch on disk {expected}: superblock says {actual}")]
    DiskIdMismatch { expected: DiskId, actual: DiskId },

    #[error("crc mismatch in pool state root: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch { expected: u32, actual: u32 },

    #[error("invalid pool state root block kind: {0}")]
    InvalidBlockKind(u16),

    #[error("invalid pool state root payload: {0}")]
    InvalidPayload(String),
}
