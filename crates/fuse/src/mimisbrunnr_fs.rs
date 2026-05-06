//! [`MimisbrunnrFs`] — adapter that exposes a [`crate::TagVfs`] as a
//! `fuser::Filesystem`.
//!
//! Phase 5b is **read-only** — every write-flavoured operation returns
//! `EROFS`. The adapter is enabled behind the `fuse` Cargo feature so the
//! base VFS layer (and its tests) can build without `fuser` (and its
//! libfuse C-library dependency).

use std::ffi::OsStr;
use std::time::{Duration, SystemTime};

use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, Generation, INodeNo, LockOwner, OpenFlags,
    ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyWrite, Request, WriteFlags,
};
use log::trace;
use mimisbrunnr_types::{ObjectId, TagId};

use crate::attr::{VfsAttr, VfsAttrKind};
use crate::entry::VfsEntryKind;
use crate::inode::{INODE_CTX_ROOT, INODE_ROOT, INODE_TAGS_ROOT, InodeId};
use crate::tag_vfs::TagVfs;

const TTL: Duration = Duration::from_secs(1);
const BLOCK_SIZE: u32 = 4096;

/// Function type that resolves an object's bytes for `read`. Returns
/// `Some(bytes)` if the engine has the content available, `None` otherwise.
pub type ContentProvider<'a> = Box<dyn Fn(ObjectId) -> Option<Vec<u8>> + Send + Sync + 'a>;

/// What the FUSE mount root (`/`) maps to in the underlying [`TagVfs`].
///
/// - [`MountRoot::Full`] — `/` shows both `/tags` and `/ctx` children
///   (the original mount layout).
/// - [`MountRoot::TagsOnly`] — `/` is the `tags`-root listing.
/// - [`MountRoot::CtxOnly`] — `/` is the `ctx`-root listing.
/// - [`MountRoot::SingleContext`] — `/` is the directory of one named
///   context, i.e. what `<full-mount>/ctx/<context>/` would show.
#[derive(Debug, Clone, Copy)]
pub enum MountRoot {
    /// Full layout: `/tags` and `/ctx` directories.
    Full,
    /// Re-root at the tags-root.
    TagsOnly,
    /// Re-root at the ctx-root.
    CtxOnly,
    /// Re-root at `/ctx/<context-tag>`.
    SingleContext(TagId),
}

/// FUSE adapter — a [`TagVfs`] plus a content-provider callback.
///
/// `MimisbrunnrFs` is the type passed to [`fuser::mount2`]. It owns the
/// underlying `TagVfs` so the `Filesystem` trait's `&self` methods can
/// reach the inode bookkeeping (which is interior-mutable inside
/// [`TagVfs`]).
pub struct MimisbrunnrFs<'a> {
    /// The underlying tag/path VFS.
    pub vfs: TagVfs<'a>,
    /// Pluggable content provider — the engine-side blob fetcher.
    pub content: ContentProvider<'a>,
    /// The session-local inode that the FUSE mount root (`INODE_ROOT`)
    /// transparently aliases. Defaults to `INODE_ROOT` itself for
    /// [`MountRoot::Full`].
    root_inode: InodeId,
}

impl<'a> MimisbrunnrFs<'a> {
    /// Construct from a `TagVfs` and a content provider with the default
    /// full-layout root. Equivalent to
    /// [`Self::new_with_root`] with [`MountRoot::Full`].
    pub fn new(vfs: TagVfs<'a>, content: ContentProvider<'a>) -> Self {
        Self::new_with_root(vfs, content, MountRoot::Full)
    }

    /// Construct with an explicit [`MountRoot`]. For
    /// [`MountRoot::SingleContext`], the requested context must already
    /// have a registered projection in the underlying [`PathContextManager`];
    /// otherwise the mount falls back to the full layout (the alternative
    /// would be returning `Result`, but mounts are configured at the CLI
    /// boundary where the existence check happens upstream).
    pub fn new_with_root(vfs: TagVfs<'a>, content: ContentProvider<'a>, root: MountRoot) -> Self {
        let root_inode = match root {
            MountRoot::Full => INODE_ROOT,
            MountRoot::TagsOnly => INODE_TAGS_ROOT,
            MountRoot::CtxOnly => INODE_CTX_ROOT,
            MountRoot::SingleContext(tag) => vfs.ctx_root_inode(tag).unwrap_or(INODE_ROOT),
        };
        Self {
            vfs,
            content,
            root_inode,
        }
    }

    /// Translate the FUSE-facing root inode (`INODE_ROOT`) to the
    /// configured underlying inode. All other inodes pass through
    /// unchanged.
    fn translate(&self, ino: InodeId) -> InodeId {
        if ino == INODE_ROOT && self.root_inode != INODE_ROOT {
            self.root_inode
        } else {
            ino
        }
    }

    /// Build the FUSE-side `FileAttr` for an `(inode, vfs_attr, optional
    /// content size)` tuple. `nlink`, `kind`, `size` are taken from the
    /// VFS; the rest are sensible defaults for a read-only mount.
    fn make_file_attr(&self, inode: InodeId, vfs_attr: VfsAttr, oid: Option<ObjectId>) -> FileAttr {
        // For files, ask the content provider for actual size; otherwise
        // use the VFS attr's size (which is 0 for files at the moment).
        let size = match (vfs_attr.kind, oid) {
            (VfsAttrKind::RegularFile, Some(oid)) => (self.content)(oid)
                .map(|b| b.len() as u64)
                .unwrap_or(vfs_attr.size),
            _ => vfs_attr.size,
        };
        let kind = match vfs_attr.kind {
            VfsAttrKind::Directory => FileType::Directory,
            VfsAttrKind::RegularFile => FileType::RegularFile,
        };
        let perm = match vfs_attr.kind {
            VfsAttrKind::Directory => 0o555,
            VfsAttrKind::RegularFile => 0o444,
        };
        FileAttr {
            ino: INodeNo(inode.raw()),
            size,
            blocks: size.div_ceil(512),
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
            crtime: SystemTime::UNIX_EPOCH,
            kind,
            perm,
            nlink: vfs_attr.nlink,
            // TODO(rewrite-phase-N): plumb real uid/gid/mode through from
            // the engine. We avoid `libc::getuid()` to keep this crate
            // libc-free.
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }

    fn object_for_inode(&self, inode: InodeId) -> Option<ObjectId> {
        let entry = self.vfs.entry(inode)?;
        match entry.kind {
            VfsEntryKind::TagObject { oid } | VfsEntryKind::CtxObject { oid, .. } => Some(oid),
            _ => None,
        }
    }
}

// `fuser::Filesystem` is `: 'static`. We therefore only impl the trait when
// the borrows the `MimisbrunnrFs` holds are themselves `'static` (typically
// the engine layer parks the index mirrors in a long-lived `Arc` and
// dereferences them as `&'static` for the duration of a mount session).
impl Filesystem for MimisbrunnrFs<'static> {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        trace!("fuse::lookup parent={} name={name_str:?}", parent.0);
        let parent = self.translate(InodeId(parent.0));
        match self.vfs.lookup(parent, name_str) {
            Some(entry) => {
                let attr = match self.vfs.getattr(entry.inode) {
                    Some(a) => a,
                    None => {
                        reply.error(Errno::ENOENT);
                        return;
                    }
                };
                let oid = self.object_for_inode(entry.inode);
                let file_attr = self.make_file_attr(entry.inode, attr, oid);
                reply.entry(&TTL, &file_attr, Generation(0));
            }
            None => reply.error(Errno::ENOENT),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let logical = self.translate(InodeId(ino.0));
        match self.vfs.getattr(logical) {
            Some(attr) => {
                let oid = self.object_for_inode(logical);
                // Always report the FUSE-facing inode the kernel asked
                // about; the size/kind comes from the translated logical
                // inode.
                reply.attr(&TTL, &self.make_file_attr(InodeId(ino.0), attr, oid));
            }
            None => reply.error(Errno::ENOENT),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let inode = self.translate(InodeId(ino.0));
        let listing = self.vfs.readdir(inode);

        for (i, (name, entry)) in listing.iter().enumerate().skip(offset as usize) {
            let attr = match self.vfs.getattr(entry.inode) {
                Some(a) => a,
                None => continue,
            };
            let kind = match attr.kind {
                VfsAttrKind::Directory => FileType::Directory,
                VfsAttrKind::RegularFile => FileType::RegularFile,
            };
            if reply.add(INodeNo(entry.inode.raw()), (i as u64) + 1, kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let inode = self.translate(InodeId(ino.0));
        let oid = match self.object_for_inode(inode) {
            Some(o) => o,
            None => {
                reply.error(Errno::EISDIR);
                return;
            }
        };
        let bytes = match (self.content)(oid) {
            Some(b) => b,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let start = (offset as usize).min(bytes.len());
        let end = (start + size as usize).min(bytes.len());
        reply.data(&bytes[start..end]);
    }

    // ----- Read-only stubs for write paths. -----
    // TODO(rewrite-phase-N): writable mount.

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _offset: u64,
        _data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        reply.error(Errno::EROFS);
    }

    fn create(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        reply.error(Errno::EROFS);
    }

    fn mkdir(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        reply.error(Errno::EROFS);
    }

    fn unlink(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(Errno::EROFS);
    }

    fn rmdir(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(Errno::EROFS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mimisbrunnr_index::{ForwardIndex, KvIndex, TagIndex};
    use mimisbrunnr_ontology::OntologyState;
    use mimisbrunnr_unix::PathContextManager;

    fn empty_vfs() -> (
        TagIndex,
        KvIndex,
        ForwardIndex,
        OntologyState,
        PathContextManager,
    ) {
        (
            TagIndex::new(),
            KvIndex::new(),
            ForwardIndex::new(),
            OntologyState::default(),
            PathContextManager::new(),
        )
    }

    #[test]
    fn new_root_inode_defaults_to_inode_root() {
        let (ti, kv, fi, ont, pcm) = empty_vfs();
        let vfs = TagVfs::new(&ti, &kv, &fi, &ont, &pcm);
        let provider: ContentProvider<'_> = Box::new(|_| None);
        let fs = MimisbrunnrFs::new(vfs, provider);
        assert_eq!(fs.root_inode, INODE_ROOT);
    }

    #[test]
    fn new_with_root_tags_only_redirects_root_to_tags() {
        let (ti, kv, fi, ont, pcm) = empty_vfs();
        let vfs = TagVfs::new(&ti, &kv, &fi, &ont, &pcm);
        let provider: ContentProvider<'_> = Box::new(|_| None);
        let fs = MimisbrunnrFs::new_with_root(vfs, provider, MountRoot::TagsOnly);
        assert_eq!(fs.root_inode, INODE_TAGS_ROOT);
        assert_eq!(fs.translate(InodeId(1)), INODE_TAGS_ROOT);
        // Non-root inodes pass through unchanged.
        assert_eq!(fs.translate(InodeId(42)), InodeId(42));
    }

    #[test]
    fn new_with_root_ctx_only_redirects_root_to_ctx() {
        let (ti, kv, fi, ont, pcm) = empty_vfs();
        let vfs = TagVfs::new(&ti, &kv, &fi, &ont, &pcm);
        let provider: ContentProvider<'_> = Box::new(|_| None);
        let fs = MimisbrunnrFs::new_with_root(vfs, provider, MountRoot::CtxOnly);
        assert_eq!(fs.root_inode, INODE_CTX_ROOT);
    }

    #[test]
    fn new_with_root_unknown_context_falls_back_to_full() {
        let (ti, kv, fi, ont, pcm) = empty_vfs();
        let vfs = TagVfs::new(&ti, &kv, &fi, &ont, &pcm);
        let provider: ContentProvider<'_> = Box::new(|_| None);
        let fs = MimisbrunnrFs::new_with_root(
            vfs,
            provider,
            MountRoot::SingleContext(TagId::new(99)),
        );
        // Unknown context → fall back to INODE_ROOT.
        assert_eq!(fs.root_inode, INODE_ROOT);
    }
}
