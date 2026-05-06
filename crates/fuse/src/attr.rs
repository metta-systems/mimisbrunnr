//! Minimal stat-like info returned by [`crate::TagVfs::getattr`].
//!
//! Stays decoupled from `fuser::FileAttr` so the VFS layer can be unit-tested
//! without the FUSE feature enabled. The [`crate::MimisbrunnrFs`] adapter
//! converts `VfsAttr` → `fuser::FileAttr` when the `fuse` feature is on.

/// File-kind discriminant — the only two shapes the FUSE bridge currently
/// presents (read-only directories and read-only regular files).
///
/// TODO(rewrite-phase-N): add `Symlink` once `/ctx/` projections start
/// emitting symlinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VfsAttrKind {
    /// A directory (lookup / readdir target).
    Directory,
    /// A regular file (read target).
    RegularFile,
}

/// Minimal stat-like info for a VFS entry.
///
/// Phase 5b is read-only, so most POSIX timestamps and ownership fields are
/// not modelled. The engine keeps these in `ObjectRecord` (per IMPL §5) and
/// will plumb them through in a later phase.
///
/// TODO(rewrite-phase-N): widen with `mtime`, `mode`, `uid`, `gid` once the
/// engine exposes per-object metadata to the FUSE bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsAttr {
    /// Directory or regular file.
    pub kind: VfsAttrKind,
    /// File size in bytes (0 for directories or files with no content
    /// provider attached).
    pub size: u64,
    /// Hard-link count.
    pub nlink: u32,
}

impl VfsAttr {
    /// Build a directory attr with the standard `nlink = 2` for an empty
    /// directory.
    pub const fn directory() -> Self {
        Self {
            kind: VfsAttrKind::Directory,
            size: 0,
            nlink: 2,
        }
    }

    /// Build a regular-file attr with the given size.
    pub const fn regular_file(size: u64) -> Self {
        Self {
            kind: VfsAttrKind::RegularFile,
            size,
            nlink: 1,
        }
    }
}
