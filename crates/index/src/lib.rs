mod tag_index;
mod tag_store;
mod forward_index;
mod kv_index;
mod error;

pub use tag_index::TagIndex;
pub use tag_store::TagStore;
pub use forward_index::{ForwardIndex, ForwardEntry};
pub use kv_index::KvIndex;
pub use error::IndexError;

// Re-export roaring for use by downstream crates
pub use roaring::RoaringBitmap;
