#[derive(Debug, thiserror::Error)]
pub enum WalError {
    #[error("storage error: {0}")]
    Storage(#[from] mimisbrunnr_storage::StorageError),

    #[error("WAL is full (capacity {capacity}, used {used})")]
    Full { capacity: u64, used: u64 },

    #[error("entry too large: {size} bytes (max {max})")]
    EntryTooLarge { size: usize, max: usize },

    #[error("corrupted WAL entry at offset {offset}: {reason}")]
    Corrupted { offset: u64, reason: String },

    #[error("LSN {requested} not found in WAL (oldest: {oldest})")]
    LsnNotFound { requested: u64, oldest: u64 },
}
