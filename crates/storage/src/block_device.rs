use crate::StorageError;

/// Abstraction over a block-addressable storage device.
///
/// All offsets and lengths are in bytes. Implementations must handle
/// alignment internally if the underlying device requires it.
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
