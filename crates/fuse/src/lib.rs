mod error;
#[cfg(feature = "fuse")]
mod fuse_impl;
mod tag_vfs;
mod vfs;

#[cfg(feature = "fuse")]
pub use fuse_impl::MimisbrunnrFs;
pub use {
    error::FuseError,
    tag_vfs::TagVfs,
    vfs::{VfsAttr, VfsFileType, VfsNode, VfsTree},
};
