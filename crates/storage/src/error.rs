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

    /// A region's structure is inconsistent: a field out of range or a
    /// layout that disagrees with itself.
    #[error("corrupt region: {0}")]
    RegionCorrupt(&'static str),

    /// A 64-bit block checksum (FORMAT-DESIGN§1.4) did not match.
    #[error("checksum mismatch: expected {expected:#018x}, got {actual:#018x}")]
    ChecksumMismatch { expected: u64, actual: u64 },

    /// A region belongs to another tree than the one reading it.
    #[error("region kind mismatch: expected {expected}, found {actual}")]
    RegionKindMismatch { expected: u16, actual: u16 },

    /// A run is marked encrypted; runs are not encrypted in this format.
    #[error("run is encrypted; encrypted runs are not supported")]
    RunEncrypted,

    /// Entries given to a run were not in strictly ascending key order.
    #[error("run entries are not in strictly ascending key order")]
    UnsortedRunEntries,

    /// One index block cannot describe the run.
    #[error("run too large for one index block")]
    RunTooLarge,

    /// A header whose preamble names no structure this format knows — a
    /// newer layout, another program's data, or corruption.
    #[error("unknown structure: magic {magic:#010x}, version {version}, kind {kind}")]
    UnknownStructure { magic: u32, version: u16, kind: u16 },

    #[error("invalid bucket kind: {0}")]
    InvalidBucketKind(u8),

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

    #[error("journal bucket list of {0} buckets exceeds the superblock's 256 slots")]
    JournalBucketListTooLong(usize),

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

    #[error("region payload exhausted: payload_used={used} would exceed region_size={size}")]
    RegionFull { used: u64, size: u64 },

    /// Allocator could not find a free bucket of the requested kind.
    #[error("bucket allocator exhausted (requested kind {requested})")]
    AllocatorExhausted { requested: u8 },

    /// `BlockRef.generation` did not match the live
    /// `BucketAllocEntry.generation` — the reference is stale.
    #[error("stale BlockRef: expected generation {expected}, actual {actual}")]
    StaleBlockRef { expected: u32, actual: u32 },

    /// A byte offset given for a `BlockRef` / `BlobRef` is not 4 KiB aligned.
    #[error("byte offset {offset} is not 4 KiB aligned")]
    UnalignedBlockOffset { offset: u64 },

    /// A byte offset cannot be named bucket-relatively: the bucket size is
    /// outside 4 KiB..=256 MiB, or the bucket number does not fit `u32`.
    #[error("byte offset {offset} is not addressable with 2^{bucket_size_log2}-byte buckets")]
    BlockOffsetUnaddressable { offset: u64, bucket_size_log2: u8 },

    /// A `BlockRef`'s reserved word is not zero.
    #[error("BlockRef reserved word is {reserved:#010x}, expected zero")]
    BlockRefReservedNonZero { reserved: u32 },

    /// A `NodePtr` replica count outside `1..=4`.
    #[error("node pointer replica count {replicas} is outside 1..=4")]
    InvalidReplicaCount { replicas: usize },

    /// Two copies of one `NodePtr` name the same disk.
    #[error("node pointer names disk {disk_id} twice")]
    DuplicateReplicaDisk { disk_id: u16 },

    /// An encoded `NodePtr` is not `16 × R` bytes.
    #[error("node pointer encoding is {actual} bytes, expected {expected}")]
    NodePtrLengthMismatch { expected: usize, actual: usize },

    /// No copy of a node verified: every copy failed to read or to check.
    #[error("none of the node's {replicas} copies verified")]
    NoReplicaVerified { replicas: usize },

    /// A disk id the pool does not know.
    #[error("unknown disk {disk_id}")]
    UnknownDisk { disk_id: u16 },

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

    /// A region larger than a bucket, or smaller than a block, was requested.
    #[error("region of 2^{region_size_log2} bytes does not fit a 2^{bucket_size_log2}-byte bucket")]
    RegionDoesNotFitBucket {
        region_size_log2: u8,
        bucket_size_log2: u8,
    },

    /// Live-sector accounting named a bucket the alloc table has no entry for.
    #[error("unknown bucket {bucket_no}")]
    UnknownBucket { bucket_no: u32 },

    /// Adding live sectors would exceed the bucket's sector count.
    #[error(
        "bucket {bucket_no}: {live} live + {added} sectors exceeds capacity {capacity}"
    )]
    LiveSectorsOverflow {
        bucket_no: u32,
        live: u32,
        added: u32,
        capacity: u32,
    },

    /// Removing live sectors would take the count below zero.
    #[error("bucket {bucket_no}: removing {removed} sectors from {live} live")]
    LiveSectorsUnderflow {
        bucket_no: u32,
        live: u32,
        removed: u32,
    },
}
