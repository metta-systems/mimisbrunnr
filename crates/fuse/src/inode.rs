//! Session-local inode identifier.
//!
//! Per IMPLEMENTATION.md §13.2, inodes are **never persisted** — they are
//! allocated lazily as the `TagVfs` is traversed and discarded when the
//! mount is unmounted.

/// Session-local inode identifier.
///
/// FUSE-side inodes are 64-bit; the kernel uses `1` for the mount root by
/// convention. The two well-known children of the root (`tags` and `ctx`)
/// take fixed inodes `2` and `3`. All other inodes are allocated lazily in
/// strictly increasing order from [`InodeId(4)`](InodeId) onwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InodeId(pub u64);

impl InodeId {
    /// Underlying `u64` value (the FUSE-facing wire form).
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl From<u64> for InodeId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<InodeId> for u64 {
    fn from(value: InodeId) -> Self {
        value.0
    }
}

/// Inode of the mount root (`/`).
pub const INODE_ROOT: InodeId = InodeId(1);

/// Inode of `/tags`.
pub const INODE_TAGS_ROOT: InodeId = InodeId(2);

/// Inode of `/ctx`.
pub const INODE_CTX_ROOT: InodeId = InodeId(3);

/// First freely-allocatable inode. Reserved values `0..=3` are well-known
/// (`0` is FUSE's "no inode" sentinel; the others are listed above).
pub(crate) const INODE_FIRST_DYNAMIC: u64 = 4;
