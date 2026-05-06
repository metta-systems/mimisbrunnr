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
use mimisbrunnr_types::ObjectId;

use crate::attr::{VfsAttr, VfsAttrKind};
use crate::entry::VfsEntryKind;
use crate::inode::InodeId;
use crate::tag_vfs::TagVfs;

const TTL: Duration = Duration::from_secs(1);
const BLOCK_SIZE: u32 = 4096;

/// Function type that resolves an object's bytes for `read`. Returns
/// `Some(bytes)` if the engine has the content available, `None` otherwise.
pub type ContentProvider<'a> = Box<dyn Fn(ObjectId) -> Option<Vec<u8>> + Send + Sync + 'a>;

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
}

impl<'a> MimisbrunnrFs<'a> {
    /// Construct from a `TagVfs` and a content provider.
    pub fn new(vfs: TagVfs<'a>, content: ContentProvider<'a>) -> Self {
        Self { vfs, content }
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
        let parent = InodeId(parent.0);
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
        let inode = InodeId(ino.0);
        match self.vfs.getattr(inode) {
            Some(attr) => {
                let oid = self.object_for_inode(inode);
                reply.attr(&TTL, &self.make_file_attr(inode, attr, oid));
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
        let inode = InodeId(ino.0);
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
        let inode = InodeId(ino.0);
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
