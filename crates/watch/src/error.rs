//! Errors raised by the watch / subscription engine.

use mimisbrunnr_storage::StorageError;
use mimisbrunnr_types::SubscriptionId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WatchError {
    #[error("subscription {0} not found")]
    UnknownSubscription(SubscriptionId),

    #[error("CBOR encode error: {0}")]
    CborEncode(String),

    #[error("CBOR decode error: {0}")]
    CborDecode(String),

    #[error("roaring bitmap codec error: {0}")]
    Bitmap(String),

    /// Underlying storage error (B+ tree region read/write).
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}
