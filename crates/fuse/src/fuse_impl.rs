use std::{ffi::OsStr, sync::RwLock, time::Duration};

use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, Generation, INodeNo, LockOwner, OpenFlags,
    ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, Request,
};

use {
    crate::{
        tag_vfs::TagVfs,
        vfs::{VfsFileType, VfsTree},
    },
    log::trace,
};

const TTL: Duration = Duration::from_secs(1);

fn vfs_kind_to_fuse(kind: VfsFileType) -> FileType {
    match kind {
        VfsFileType::RegularFile => FileType::RegularFile,
        VfsFileType::Directory => FileType::Directory,
        VfsFileType::Symlink => FileType::Symlink,
    }
}

fn vfs_attr_to_fuse(attr: &crate::vfs::VfsAttr) -> FileAttr {
    FileAttr {
        ino: INodeNo(attr.ino),
        size: attr.size,
        blocks: attr.blocks,
        atime: attr.atime,
        mtime: attr.mtime,
        ctime: attr.ctime,
        crtime: attr.ctime,
        kind: vfs_kind_to_fuse(attr.kind),
        perm: attr.mode as u16,
        nlink: attr.nlink,
        uid: attr.uid,
        gid: attr.gid,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

/// FUSE filesystem backed by a tag-navigable VFS.
///
/// Exposes a tag filesystem (like TMSU/tagsistant) at `/tags/` and
/// unix path projections at `/ctx/`. Files appear inside tag directories
/// and can be refined by navigating into nested tag subdirectories.
///
/// Uses `RwLock` for interior mutability because `fuser::Filesystem`
/// trait methods take `&self`, but `TagVfs` needs `&mut self` for
/// lazy inode allocation.
pub struct MimisbrunnrFs {
    vfs: RwLock<TagVfs>,
}

impl MimisbrunnrFs {
    /// Create from a fully configured TagVfs.
    pub fn from_tag_vfs(vfs: TagVfs) -> Self {
        Self {
            vfs: RwLock::new(vfs),
        }
    }

    /// Create from a legacy VfsTree (for backwards compatibility).
    /// The tree is installed as a context named "default".
    pub fn new(tree: VfsTree) -> Self {
        use {
            mimisbrunnr_index::{ForwardIndex, KvIndex, TagIndex},
            mimisbrunnr_ontology::ImplicationDag,
        };

        let mut vfs = TagVfs::new(
            TagIndex::new(),
            KvIndex::new(),
            ForwardIndex::new(),
            ImplicationDag::new(),
        );
        vfs.add_context("default".into(), tree);
        Self {
            vfs: RwLock::new(vfs),
        }
    }

    pub fn set_blob(&self, obj_raw: u64, data: Vec<u8>) {
        self.vfs.write().unwrap().set_blob(obj_raw, data);
    }
}

impl Filesystem for MimisbrunnrFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name = name.to_str().unwrap_or("");
        trace!("fuse::lookup parent={} name={:?}", parent.0, name);
        let mut vfs = self.vfs.write().unwrap();
        match vfs.lookup(parent.0, name) {
            Some(ino) => match vfs.getattr(ino) {
                Some(attr) => reply.entry(&TTL, &vfs_attr_to_fuse(&attr), Generation(0)),
                None => reply.error(Errno::ENOENT),
            },
            None => reply.error(Errno::ENOENT),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        trace!("fuse::getattr ino={}", ino.0);
        match self.vfs.read().unwrap().getattr(ino.0) {
            Some(attr) => reply.attr(&TTL, &vfs_attr_to_fuse(&attr)),
            None => reply.error(Errno::ENOENT),
        }
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
        trace!("fuse::read ino={} offset={offset} size={size}", ino.0);
        match self.vfs.read().unwrap().read(ino.0, offset, size) {
            Some(data) => reply.data(data),
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
        trace!("fuse::readdir ino={} offset={offset}", ino.0);
        match self.vfs.write().unwrap().readdir(ino.0) {
            Some(entries) => {
                for (i, entry) in entries.iter().enumerate().skip(offset as usize) {
                    let full = reply.add(
                        INodeNo(entry.ino),
                        (i + 1) as u64,
                        vfs_kind_to_fuse(entry.file_type),
                        &entry.name,
                    );
                    if full {
                        break;
                    }
                }
                reply.ok();
            }
            None => reply.error(Errno::ENOTDIR),
        }
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        trace!("fuse::readlink ino={}", ino.0);
        match self.vfs.read().unwrap().readlink(ino.0) {
            Some(target) => reply.data(target.as_bytes()),
            None => reply.error(Errno::ENOENT),
        }
    }
}
