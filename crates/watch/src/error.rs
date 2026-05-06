//! Errors raised by the watch / subscription engine.

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
}
