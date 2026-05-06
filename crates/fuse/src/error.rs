//! Error type for the FUSE bridge.

use thiserror::Error;

/// Errors returned by [`crate::TagVfs`] and the [`crate::MimisbrunnrFs`]
/// adapter.
#[derive(Debug, Error)]
pub enum FuseError {
    /// An interior-mutability lock was poisoned (a thread panicked while
    /// holding it). Treated as fatal.
    #[error("internal lock poisoned: {0}")]
    LockPoisoned(&'static str),
}
