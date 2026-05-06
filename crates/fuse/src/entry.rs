//! Logical entries (and their kinds) the [`crate::TagVfs`] tracks per inode.
//!
//! This is the canonical answer to "what does inode N represent?". The FUSE
//! adapter never sees these directly — it goes through [`crate::TagVfs`]
//! methods which translate kinds into `lookup` / `getattr` / `readdir`
//! responses.

use std::collections::BTreeSet;

use mimisbrunnr_types::{ObjectId, TagId};

use crate::inode::InodeId;

/// What an inode represents inside the VFS.
///
/// `TagDir` uses a `BTreeSet<TagId>` so the path order is irrelevant — both
/// `/tags/electronic/portable` and `/tags/portable/electronic` resolve to the
/// same canonical tag set, and therefore the same inode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VfsEntryKind {
    /// `/` — the mount root.
    Root,
    /// `/tags` — root of the tag-navigation tree.
    TagsRoot,
    /// `/ctx` — root of the path-context projections.
    CtxRoot,
    /// `/tags/<a>/<b>/<c>` — the matched-set hierarchy.
    ///
    /// `current_tags` is the canonical set of tags the path so far has
    /// asserted. The matched objects are `tag_index.intersect(current_tags)`.
    TagDir { current_tags: BTreeSet<TagId> },
    /// `/tags/<…>/<oid>` — a specific matched object.
    TagObject { oid: ObjectId },
    /// `/ctx/<context>/<sub_path>` — a directory inside a path projection.
    ///
    /// `sub_path` is the projection-relative directory the inode points at,
    /// using `/` separators. The empty string means the projection's root.
    CtxDir {
        context: TagId,
        sub_path: String,
    },
    /// `/ctx/<context>/…/<file>` — a leaf object in a path projection.
    CtxObject { context: TagId, oid: ObjectId },
}

/// One entry in the VFS inode table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsEntry {
    /// The session-local inode id of this entry.
    pub inode: InodeId,
    /// What this inode represents.
    pub kind: VfsEntryKind,
    /// Parent inode (`None` for the root, which is its own parent in POSIX
    /// terms but we keep that detail out of the data model).
    pub parent: Option<InodeId>,
}
