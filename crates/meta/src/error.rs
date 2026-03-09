#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    #[error("storage error: {0}")]
    Storage(#[from] mimisbrunnr_storage::StorageError),

    #[error("object table full (capacity {capacity})")]
    TableFull { capacity: u64 },

    #[error("object slot {0} is cleared/invalid")]
    InvalidSlot(u64),

    #[error("record deserialization failed at slot {slot}: {reason}")]
    Corrupt { slot: u64, reason: String },
}
