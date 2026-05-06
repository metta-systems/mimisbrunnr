//! `mimisbrunnr-fuse` — FUSE bridge for Mímisbrunnr (DESIGN §12.6, §12.7).
//!
//! This crate is a *thin reader* over the in-memory mirrors that the engine
//! holds (tag / kv / forward indices, ontology state, path-context manager).
//! It does **not** depend on the engine — the engine (Phase 6) is expected to
//! own a [`TagVfs`] and pass references into [`MimisbrunnrFs`] when mounting.
//!
//! # Mount layout
//!
//! ```text
//! mimisbrunnr/
//! ├── tags/                       # TMSU/tagsistant-style tag navigation
//! │   ├── electronic/
//! │   │   ├── portable/
//! │   │   │   ├── 4242            # virtual file → ObjectId 4242
//! │   │   │   └── 4789
//! │   │   └── stationary/
//! │   └── archive/
//! └── ctx/                        # unix-path projections under context tags
//!     ├── rpi4-sdcard/
//!     │   └── boot/
//!     │       └── vesper          # → ObjectId via PathProjection
//!     └── …
//! ```
//!
//! Under `/tags/`, each path component is a tag name; the listing of a
//! directory is the **faceted refinement** (DESIGN §5.6) of the current tag
//! set — only tags that have non-empty intersection with the current matched
//! bitmap are shown. Files in a tags-directory are the matched objects, named
//! by `ObjectId` in decimal.
//!
//! Under `/ctx/`, each top-level entry is a path-context tag (a `Grouping`
//! tag holding a [`mimisbrunnr_unix::PathProjection`]); below it, the
//! projection's `paths` map drives the directory hierarchy.
//!
//! # Inode identity
//!
//! Per IMPLEMENTATION.md §13.2, inodes are **session-local** and allocated
//! lazily as paths are traversed. They are *never persisted* — the TagVfs is
//! a derived projection.
//!
//! # Read-only in this phase
//!
//! Phase 5b ships a read-only mount. Write operations return `EROFS` and are
//! marked with `TODO(rewrite-phase-N): writable mount`.

#![forbid(unsafe_code)]

mod attr;
mod entry;
mod error;
mod inode;
mod tag_vfs;

#[cfg(feature = "fuse")]
mod mimisbrunnr_fs;

pub use attr::{VfsAttr, VfsAttrKind};
pub use entry::{VfsEntry, VfsEntryKind};
pub use error::FuseError;
pub use inode::{INODE_CTX_ROOT, INODE_ROOT, INODE_TAGS_ROOT, InodeId};
pub use tag_vfs::TagVfs;

#[cfg(feature = "fuse")]
pub use mimisbrunnr_fs::{ContentProvider, MimisbrunnrFs, MountRoot};
