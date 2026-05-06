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

    #[error("device opened read-only; writes are not permitted")]
    ReadOnly,

    #[error("CBOR encode error: {0}")]
    CborEncode(String),

    #[error("CBOR decode error: {0}")]
    CborDecode(String),

    #[error("invalid sorted-run magic: expected {expected:#010x}, got {actual:#010x}")]
    InvalidSortedRunMagic { expected: u32, actual: u32 },

    #[error("region payload exhausted: payload_used={used} would exceed region_size={size}")]
    RegionFull { used: u64, size: u64 },

    #[error("packed-key codec error: {0}")]
    Pack(#[from] crate::btree::pack::PackError),

    /// Allocator could not find a free bucket of the requested kind.
    #[error("bucket allocator exhausted (requested kind {requested})")]
    AllocatorExhausted { requested: u8 },

    /// `BlockRef.generation` did not match the live
    /// `BucketAllocEntry.generation` — the reference is stale.
    #[error("stale BlockRef: expected generation {expected}, actual {actual}")]
    StaleBlockRef { expected: u64, actual: u64 },

    /// `BlockRef.disk_id` doesn't match the allocator's disk.
    #[error(
        "cross-disk BlockRef: expected disk {expected_disk}, got {got_disk}"
    )]
    CrossDiskBlockRef {
        expected_disk: u16,
        got_disk: u16,
    },

    /// Free called on a bucket that has no live `BucketAllocEntry`.
    #[error("free of unallocated bucket {bucket_no}")]
    FreeOfUnallocatedBucket { bucket_no: u32 },
}
