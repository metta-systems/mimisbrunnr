//! Block device abstraction.

use crate::error::StorageError;

/// Abstraction over a block-addressable storage device.
///
/// Offsets and lengths are in **bytes**. Implementations must serialise
/// concurrent access — reads and writes are expected to be atomic at the
/// byte level (file system semantics or real disk semantics).
pub trait BlockDevice: Send + Sync {
    /// Read `buf.len()` bytes starting at `offset`.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError>;

    /// Write `buf` starting at `offset`.
    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), StorageError>;

    /// Total capacity of the device in bytes.
    fn capacity(&self) -> u64;

    /// Flush all pending writes to stable storage.
    fn sync(&self) -> Result<(), StorageError>;
}
