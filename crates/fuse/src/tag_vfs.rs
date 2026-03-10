//! Tag-based virtual filesystem, similar to TMSU/tagsistant.
//!
//! The filesystem exposes a tag-navigable directory hierarchy:
//!
//! ```text
//! /                          root
//! ├── tags/                  all tags as directories
//! │   ├── electronic/        objects tagged 'electronic' + refinement tags
//! │   │   ├── ambient/       electronic AND ambient (further refinement)
//! │   │   │   ├── track.flac
//! │   │   │   └── ...
//! │   │   ├── track1.flac
//! │   │   └── track2.mp3
//! │   ├── ambient/
//! │   │   └── ...
//! │   └── ...
//! └── ctx/                   unix path projection contexts
//!     └── rpi4-sdcard/
//!         └── <projected tree>
//! ```
//!
//! Each tag directory contains:
//! - **Files**: objects matching the current tag intersection
//! - **Subdirectories**: "refinement" tags that appear on at least one matching
//!   object but are not yet applied (faceted exploration)
//!
//! The tag filesystem is **dynamic**: directory contents are computed on-the-fly
//! via bitmap intersection against the tag/kv/forward indexes. Inode numbers are
//! assigned lazily and cached for the lifetime of the mount.

use std::collections::{BTreeSet, HashMap};

use mimisbrunnr_index::{ForwardIndex, KvIndex, TagIndex};
use mimisbrunnr_ontology::ImplicationDag;
use mimisbrunnr_types::{Assertion, ObjectId, TagId};
use roaring::RoaringBitmap;

use crate::vfs::{VfsAttr, VfsFileType, VfsTree};
use log::trace;

// Fixed inode numbers for well-known entries.
const INO_ROOT: u64 = 1;
const INO_TAGS: u64 = 2;
const INO_CTX: u64 = 3;
const INO_FIRST_DYNAMIC: u64 = 4;

/// An entry in the tag filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagVfsEntry {
    /// `/` — the root directory.
    Root,
    /// `/tags/` — lists all tags.
    TagsRoot,
    /// `/ctx/` — lists all path projection contexts.
    CtxRoot,
    /// `/tags/t1/t2/...` — a tag intersection directory.
    TagDir(BTreeSet<TagId>),
    /// A file within a tag directory.
    TagFile {
        tags: BTreeSet<TagId>,
        obj_local: u32,
    },
    /// `/ctx/<name>/` — a context root.
    CtxDir(String),
    /// A node inside a context subtree (delegates to VfsTree).
    CtxNode {
        ctx: String,
        /// Inode in the context's VfsTree.
        vfs_ino: u64,
    },
}

/// A directory entry returned by readdir.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub ino: u64,
    pub name: String,
    pub file_type: VfsFileType,
}

/// Dynamic tag-based virtual filesystem.
///
/// Owns clones of the index structures (snapshot at mount time) and lazily
/// allocates inodes as directories are explored.
pub struct TagVfs {
    tag_index: TagIndex,
    #[allow(dead_code)] // stored for future KV-based attribute queries
    kv_index: KvIndex,
    forward_index: ForwardIndex,
    dag: ImplicationDag,

    /// Context subtrees (built from PathProjections).
    context_trees: HashMap<String, VfsTree>,

    /// Blob data for serving file reads.
    blobs: HashMap<u64, Vec<u8>>,

    // -- Inode allocation --
    entries: HashMap<u64, TagVfsEntry>,
    /// Reverse map: tag dir → inode.
    tag_dir_inos: HashMap<BTreeSet<TagId>, u64>,
    /// Reverse map: (tag set, obj_local) → inode.
    file_inos: HashMap<(BTreeSet<TagId>, u32), u64>,
    /// Reverse map: context name → inode of its CtxDir.
    ctx_dir_inos: HashMap<String, u64>,
    /// Reverse map: (ctx, vfs_ino) → global inode.
    ctx_node_inos: HashMap<(String, u64), u64>,

    next_ino: u64,
}

impl TagVfs {
    /// Create a new tag VFS from index snapshots.
    pub fn new(
        tag_index: TagIndex,
        kv_index: KvIndex,
        forward_index: ForwardIndex,
        dag: ImplicationDag,
    ) -> Self {
        let mut vfs = Self {
            tag_index,
            kv_index,
            forward_index,
            dag,
            context_trees: HashMap::new(),
            blobs: HashMap::new(),
            entries: HashMap::new(),
            tag_dir_inos: HashMap::new(),
            file_inos: HashMap::new(),
            ctx_dir_inos: HashMap::new(),
            ctx_node_inos: HashMap::new(),
            next_ino: INO_FIRST_DYNAMIC,
        };

        // Register fixed entries.
        vfs.entries.insert(INO_ROOT, TagVfsEntry::Root);
        vfs.entries.insert(INO_TAGS, TagVfsEntry::TagsRoot);
        vfs.entries.insert(INO_CTX, TagVfsEntry::CtxRoot);

        vfs
    }

    /// Add a context subtree (unix path projection).
    pub fn add_context(&mut self, name: String, tree: VfsTree) {
        self.context_trees.insert(name, tree);
    }

    /// Store blob data for reading files.
    pub fn set_blob(&mut self, obj_raw: u64, data: Vec<u8>) {
        self.blobs.insert(obj_raw, data);
    }

    /// Get an entry by inode.
    pub fn get(&self, ino: u64) -> Option<&TagVfsEntry> {
        self.entries.get(&ino)
    }

    /// Get file attributes for an inode.
    pub fn getattr(&self, ino: u64) -> Option<VfsAttr> {
        let entry = self.entries.get(&ino)?;
        match entry {
            TagVfsEntry::Root
            | TagVfsEntry::TagsRoot
            | TagVfsEntry::CtxRoot
            | TagVfsEntry::TagDir(_)
            | TagVfsEntry::CtxDir(_) => Some(dir_attr(ino)),

            TagVfsEntry::TagFile { obj_local, .. } => {
                let oid = ObjectId::new(0, *obj_local as u64);
                let size = self
                    .blobs
                    .get(&oid.raw())
                    .map(|b| b.len() as u64)
                    .unwrap_or(0);
                Some(file_attr(ino, size))
            }

            TagVfsEntry::CtxNode { ctx, vfs_ino } => {
                if let Some(tree) = self.context_trees.get(ctx) {
                    if let Some(node) = tree.get(*vfs_ino) {
                        let mut attr = node.attr.clone();
                        attr.ino = ino; // remap to global inode
                        return Some(attr);
                    }
                }
                None
            }
        }
    }

    /// Lookup a child by name within a directory.
    pub fn lookup(&mut self, parent_ino: u64, name: &str) -> Option<u64> {
        trace!("tag_vfs::lookup parent_ino={parent_ino} name={name:?}");
        let parent = self.entries.get(&parent_ino)?.clone();
        match parent {
            TagVfsEntry::Root => match name {
                "tags" => Some(INO_TAGS),
                "ctx" => Some(INO_CTX),
                _ => None,
            },

            TagVfsEntry::TagsRoot => {
                let tag_id = self.dag.lookup(name)?;
                if self.tag_index.bitmap(tag_id)?.is_empty() {
                    return None;
                }
                let tags = BTreeSet::from([tag_id]);
                Some(self.ensure_tag_dir(tags))
            }

            TagVfsEntry::TagDir(ref parent_tags) => {
                // Check if `name` is a refinement tag.
                if let Some(tag_id) = self.dag.lookup(name) {
                    if !parent_tags.contains(&tag_id) {
                        let bitmap = self.intersect_tags(parent_tags);
                        if let Some(tag_bm) = self.tag_index.bitmap(tag_id) {
                            let refined = &bitmap & tag_bm;
                            if !refined.is_empty() {
                                let mut new_tags = parent_tags.clone();
                                new_tags.insert(tag_id);
                                return Some(self.ensure_tag_dir(new_tags));
                            }
                        }
                    }
                    return None;
                }

                // Check if `name` matches a file in this tag dir.
                let bitmap = self.intersect_tags(parent_tags);
                for obj_local in bitmap.iter() {
                    let file_name = self.object_display_name(obj_local, &bitmap);
                    if file_name == name {
                        return Some(
                            self.ensure_file(parent_tags.clone(), obj_local),
                        );
                    }
                }

                None
            }

            TagVfsEntry::CtxRoot => {
                if self.context_trees.contains_key(name) {
                    Some(self.ensure_ctx_dir(name.to_string()))
                } else {
                    None
                }
            }

            TagVfsEntry::CtxDir(ref ctx_name) => {
                if let Some(tree) = self.context_trees.get(ctx_name) {
                    // Lookup in the context's VfsTree root.
                    if let Some(node) = tree.lookup(1, name) {
                        let ctx = ctx_name.clone();
                        return Some(self.ensure_ctx_node(ctx, node.ino));
                    }
                }
                None
            }

            TagVfsEntry::CtxNode {
                ref ctx,
                ref vfs_ino,
            } => {
                let ctx = ctx.clone();
                let vfs_ino = *vfs_ino;
                if let Some(tree) = self.context_trees.get(&ctx) {
                    if let Some(node) = tree.lookup(vfs_ino, name) {
                        return Some(self.ensure_ctx_node(ctx, node.ino));
                    }
                }
                None
            }

            _ => None, // Files don't have children
        }
    }

    /// List directory contents.
    pub fn readdir(&mut self, dir_ino: u64) -> Option<Vec<DirEntry>> {
        trace!("tag_vfs::readdir dir_ino={dir_ino}");
        let entry = self.entries.get(&dir_ino)?.clone();
        let mut result = Vec::new();

        // Always add . and ..
        let parent_ino = self.parent_ino(dir_ino);
        result.push(DirEntry {
            ino: dir_ino,
            name: ".".into(),
            file_type: VfsFileType::Directory,
        });
        result.push(DirEntry {
            ino: parent_ino,
            name: "..".into(),
            file_type: VfsFileType::Directory,
        });

        match entry {
            TagVfsEntry::Root => {
                result.push(DirEntry {
                    ino: INO_TAGS,
                    name: "tags".into(),
                    file_type: VfsFileType::Directory,
                });
                if !self.context_trees.is_empty() {
                    result.push(DirEntry {
                        ino: INO_CTX,
                        name: "ctx".into(),
                        file_type: VfsFileType::Directory,
                    });
                }
            }

            TagVfsEntry::TagsRoot => {
                // Collect (tag_id, name) first to avoid borrow conflicts.
                let tags_with_names: Vec<(TagId, String)> = self
                    .dag
                    .all_tags()
                    .into_iter()
                    .filter(|tag_id| {
                        self.tag_index
                            .bitmap(*tag_id)
                            .is_some_and(|bm| !bm.is_empty())
                    })
                    .filter_map(|tag_id| {
                        self.dag.get(tag_id).map(|def| (tag_id, def.name.clone()))
                    })
                    .collect();

                for (tag_id, name) in tags_with_names {
                    let tags = BTreeSet::from([tag_id]);
                    let ino = self.ensure_tag_dir(tags);
                    result.push(DirEntry {
                        ino,
                        name,
                        file_type: VfsFileType::Directory,
                    });
                }
            }

            TagVfsEntry::TagDir(ref parent_tags) => {
                let bitmap = self.intersect_tags(parent_tags);
                let parent_tags_owned = parent_tags.clone();

                // 1. Collect refinement tags (faceted exploration).
                let refinements: Vec<(TagId, BTreeSet<TagId>, String)> = self
                    .dag
                    .all_tags()
                    .into_iter()
                    .filter(|tag_id| !parent_tags_owned.contains(tag_id))
                    .filter(|tag_id| {
                        self.tag_index
                            .bitmap(*tag_id)
                            .is_some_and(|tag_bm| !(&bitmap & tag_bm).is_empty())
                    })
                    .filter_map(|tag_id| {
                        let mut new_tags = parent_tags_owned.clone();
                        new_tags.insert(tag_id);
                        self.dag
                            .get(tag_id)
                            .map(|def| (tag_id, new_tags, def.name.clone()))
                    })
                    .collect();

                for (_tag_id, new_tags, name) in refinements {
                    let ino = self.ensure_tag_dir(new_tags);
                    result.push(DirEntry {
                        ino,
                        name,
                        file_type: VfsFileType::Directory,
                    });
                }

                // 2. Collect files with display names.
                let files: Vec<(u32, String)> = bitmap
                    .iter()
                    .map(|obj_local| {
                        let name = self.object_display_name(obj_local, &bitmap);
                        (obj_local, name)
                    })
                    .collect();

                for (obj_local, display_name) in files {
                    let ino = self.ensure_file(parent_tags_owned.clone(), obj_local);
                    result.push(DirEntry {
                        ino,
                        name: display_name,
                        file_type: VfsFileType::RegularFile,
                    });
                }
            }

            TagVfsEntry::CtxRoot => {
                let ctx_names: Vec<String> =
                    self.context_trees.keys().cloned().collect();
                for name in ctx_names {
                    let ino = self.ensure_ctx_dir(name.clone());
                    result.push(DirEntry {
                        ino,
                        name,
                        file_type: VfsFileType::Directory,
                    });
                }
            }

            TagVfsEntry::CtxDir(ref ctx_name) => {
                let ctx = ctx_name.clone();
                let children: Vec<(u64, String, VfsFileType)> = self
                    .context_trees
                    .get(&ctx)
                    .and_then(|tree| tree.readdir(1))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(_, name, _)| *name != "." && *name != "..")
                    .map(|(vfs_ino, name, ft)| (vfs_ino, name.to_string(), ft))
                    .collect();

                for (vfs_ino, name, file_type) in children {
                    let ino = self.ensure_ctx_node(ctx.clone(), vfs_ino);
                    result.push(DirEntry {
                        ino,
                        name,
                        file_type,
                    });
                }
            }

            TagVfsEntry::CtxNode {
                ref ctx,
                ref vfs_ino,
            } => {
                let ctx = ctx.clone();
                let vfs_ino = *vfs_ino;
                let children: Vec<(u64, String, VfsFileType)> = self
                    .context_trees
                    .get(&ctx)
                    .and_then(|tree| tree.readdir(vfs_ino))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(_, name, _)| *name != "." && *name != "..")
                    .map(|(vfs_ino, name, ft)| (vfs_ino, name.to_string(), ft))
                    .collect();

                for (child_vfs_ino, name, file_type) in children {
                    let ino = self.ensure_ctx_node(ctx.clone(), child_vfs_ino);
                    result.push(DirEntry {
                        ino,
                        name,
                        file_type,
                    });
                }
            }

            _ => return None, // Not a directory
        }

        Some(result)
    }

    /// Read file content.
    pub fn read(&self, ino: u64, offset: u64, size: u32) -> Option<&[u8]> {
        trace!("tag_vfs::read ino={ino} offset={offset} size={size}");
        let entry = self.entries.get(&ino)?;
        match entry {
            TagVfsEntry::TagFile { obj_local, .. } => {
                let oid = ObjectId::new(0, *obj_local as u64);
                let data = self.blobs.get(&oid.raw())?;
                let start = (offset as usize).min(data.len());
                let end = (start + size as usize).min(data.len());
                Some(&data[start..end])
            }
            TagVfsEntry::CtxNode { ctx, vfs_ino } => {
                let tree = self.context_trees.get(ctx)?;
                let node = tree.get(*vfs_ino)?;
                let oid = node.object?;
                let data = self.blobs.get(&oid.raw())?;
                let start = (offset as usize).min(data.len());
                let end = (start + size as usize).min(data.len());
                Some(&data[start..end])
            }
            _ => None,
        }
    }

    /// Get the symlink target for a context node.
    pub fn readlink(&self, ino: u64) -> Option<&str> {
        trace!("tag_vfs::readlink ino={ino}");
        let entry = self.entries.get(&ino)?;
        if let TagVfsEntry::CtxNode { ctx, vfs_ino } = entry {
            let tree = self.context_trees.get(ctx)?;
            let node = tree.get(*vfs_ino)?;
            node.symlink_target.as_deref()
        } else {
            None
        }
    }

    // ------------------------------------------------------------------
    // Inode allocation helpers
    // ------------------------------------------------------------------

    fn alloc_ino(&mut self) -> u64 {
        trace!("tag_vfs::alloc_ino -> {}", self.next_ino);
        let ino = self.next_ino;
        self.next_ino += 1;
        ino
    }

    fn ensure_tag_dir(&mut self, tags: BTreeSet<TagId>) -> u64 {
        if let Some(&ino) = self.tag_dir_inos.get(&tags) {
            return ino;
        }
        let ino = self.alloc_ino();
        self.tag_dir_inos.insert(tags.clone(), ino);
        self.entries.insert(ino, TagVfsEntry::TagDir(tags));
        ino
    }

    fn ensure_file(&mut self, tags: BTreeSet<TagId>, obj_local: u32) -> u64 {
        let key = (tags.clone(), obj_local);
        if let Some(&ino) = self.file_inos.get(&key) {
            return ino;
        }
        let ino = self.alloc_ino();
        self.file_inos.insert(key, ino);
        self.entries
            .insert(ino, TagVfsEntry::TagFile { tags, obj_local });
        ino
    }

    fn ensure_ctx_dir(&mut self, name: String) -> u64 {
        if let Some(&ino) = self.ctx_dir_inos.get(&name) {
            return ino;
        }
        let ino = self.alloc_ino();
        self.ctx_dir_inos.insert(name.clone(), ino);
        self.entries.insert(ino, TagVfsEntry::CtxDir(name));
        ino
    }

    fn ensure_ctx_node(&mut self, ctx: String, vfs_ino: u64) -> u64 {
        let key = (ctx.clone(), vfs_ino);
        if let Some(&ino) = self.ctx_node_inos.get(&key) {
            return ino;
        }
        let ino = self.alloc_ino();
        self.ctx_node_inos.insert(key, ino);
        self.entries
            .insert(ino, TagVfsEntry::CtxNode { ctx, vfs_ino });
        ino
    }

    // ------------------------------------------------------------------
    // Query helpers
    // ------------------------------------------------------------------

    /// Intersect bitmaps for a set of tags.
    fn intersect_tags(&self, tags: &BTreeSet<TagId>) -> RoaringBitmap {
        trace!("tag_vfs::intersect_tags tags={tags:?}");
        let mut iter = tags.iter();
        let first = match iter.next() {
            Some(t) => t,
            None => return RoaringBitmap::new(),
        };
        let mut result = self
            .tag_index
            .bitmap(*first)
            .cloned()
            .unwrap_or_default();
        for tag_id in iter {
            if let Some(bm) = self.tag_index.bitmap(*tag_id) {
                result &= bm;
            } else {
                return RoaringBitmap::new();
            }
            if result.is_empty() {
                break;
            }
        }
        result
    }

    /// Determine a display name for an object. Uses the `name` attribute
    /// if available, otherwise falls back to `obj_<id>`.
    ///
    /// If multiple objects in the same bitmap share a name, disambiguates
    /// with `_<id>` suffix.
    fn object_display_name(&self, obj_local: u32, bitmap: &RoaringBitmap) -> String {
        let oid = ObjectId::new(0, obj_local as u64);
        let base_name = self.get_name_attr(oid);

        let name = match base_name {
            Some(n) => n,
            None => return format!("obj_{}", obj_local),
        };

        // Check for duplicate names within the same directory.
        let mut count = 0;
        for other in bitmap.iter() {
            if other == obj_local {
                continue;
            }
            let other_oid = ObjectId::new(0, other as u64);
            if let Some(other_name) = self.get_name_attr(other_oid) {
                if other_name == name {
                    count += 1;
                }
            }
        }

        if count > 0 {
            // Disambiguate: insert id before extension.
            if let Some(dot) = name.rfind('.') {
                format!("{}_{}{}", &name[..dot], obj_local, &name[dot..])
            } else {
                format!("{}_{}", name, obj_local)
            }
        } else {
            name
        }
    }

    /// Get the `name` attribute from the forward index.
    fn get_name_attr(&self, oid: ObjectId) -> Option<String> {
        use mimisbrunnr_types::Value;
        let name_tag = self.dag.lookup("name")?;
        for entry in self.forward_index.get(oid) {
            if let Assertion::Attr { key, value } = &entry.assertion {
                if *key == name_tag {
                    return match value {
                        Value::Text(s) => Some(s.clone()),
                        other => Some(other.to_string()),
                    };
                }
            }
        }
        None
    }

    /// Determine the parent inode for a given inode.
    fn parent_ino(&self, ino: u64) -> u64 {
        match ino {
            INO_ROOT => INO_ROOT,
            INO_TAGS => INO_ROOT,
            INO_CTX => INO_ROOT,
            _ => {
                match self.entries.get(&ino) {
                    Some(TagVfsEntry::TagDir(tags)) if tags.len() == 1 => INO_TAGS,
                    Some(TagVfsEntry::TagDir(tags)) => {
                        // Parent is the same tag set minus the last-inserted tag.
                        // Since BTreeSet is ordered, we remove the last element.
                        let mut parent_tags = tags.clone();
                        parent_tags.pop_last();
                        self.tag_dir_inos
                            .get(&parent_tags)
                            .copied()
                            .unwrap_or(INO_TAGS)
                    }
                    Some(TagVfsEntry::TagFile { tags, .. }) => self
                        .tag_dir_inos
                        .get(tags)
                        .copied()
                        .unwrap_or(INO_TAGS),
                    Some(TagVfsEntry::CtxDir(_)) => INO_CTX,
                    Some(TagVfsEntry::CtxNode { ctx, vfs_ino }) => {
                        if *vfs_ino == 1 {
                            // Root of context tree → parent is CtxDir
                            self.ctx_dir_inos.get(ctx).copied().unwrap_or(INO_CTX)
                        } else {
                            // Find parent in context tree
                            if let Some(tree) = self.context_trees.get(ctx) {
                                if let Some(node) = tree.get(*vfs_ino) {
                                    return self
                                        .ctx_node_inos
                                        .get(&(ctx.clone(), node.parent))
                                        .copied()
                                        .unwrap_or(INO_CTX);
                                }
                            }
                            INO_CTX
                        }
                    }
                    _ => INO_ROOT,
                }
            }
        }
    }

    /// Total number of allocated inodes.
    pub fn inode_count(&self) -> usize {
        self.entries.len()
    }
}

fn dir_attr(ino: u64) -> VfsAttr {
    VfsAttr {
        ino,
        size: 0,
        blocks: 0,
        atime: std::time::SystemTime::UNIX_EPOCH,
        mtime: std::time::SystemTime::UNIX_EPOCH,
        ctime: std::time::SystemTime::UNIX_EPOCH,
        kind: VfsFileType::Directory,
        mode: 0o755,
        nlink: 2,
        uid: 0,
        gid: 0,
    }
}

fn file_attr(ino: u64, size: u64) -> VfsAttr {
    VfsAttr {
        ino,
        size,
        blocks: size.div_ceil(512),
        atime: std::time::SystemTime::UNIX_EPOCH,
        mtime: std::time::SystemTime::UNIX_EPOCH,
        ctime: std::time::SystemTime::UNIX_EPOCH,
        kind: VfsFileType::RegularFile,
        mode: 0o644,
        nlink: 1,
        uid: 0,
        gid: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mimisbrunnr_index::TagIndex;
    use mimisbrunnr_ontology::{TagDefinition, TagSemantics, ValueType};
    use mimisbrunnr_types::TagOrigin;

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    fn label(id: u32, name: &str) -> TagDefinition {
        TagDefinition::new(tag(id), name, TagSemantics::Label)
    }

    fn attr_def(id: u32, name: &str) -> TagDefinition {
        TagDefinition::new(
            tag(id),
            name,
            TagSemantics::Attribute {
                value_type: ValueType::Text,
            },
        )
    }

    struct TestFixture {
        tag_index: TagIndex,
        kv_index: KvIndex,
        forward_index: ForwardIndex,
        dag: ImplicationDag,
    }

    impl TestFixture {
        fn new() -> Self {
            Self {
                tag_index: TagIndex::new(),
                kv_index: KvIndex::new(),
                forward_index: ForwardIndex::new(),
                dag: ImplicationDag::new(),
            }
        }

        fn register_tag(&mut self, id: u32, name: &str) -> TagId {
            self.dag.register_tag(label(id, name)).unwrap()
        }

        fn register_attr(&mut self, id: u32, name: &str) -> TagId {
            self.dag.register_tag(attr_def(id, name)).unwrap()
        }

        fn add_object(&mut self, obj_local: u32, tags: &[TagId], name: Option<&str>) {
            let oid = ObjectId::new(0, obj_local as u64);
            for &tag_id in tags {
                self.tag_index.tag_object(tag_id, obj_local);
                self.forward_index
                    .add(oid, Assertion::Tag(tag_id), TagOrigin::Direct);
            }
            if let Some(n) = name {
                if let Some(name_tag) = self.dag.lookup("name") {
                    let val = mimisbrunnr_types::Value::Text(n.to_string());
                    self.kv_index.insert(name_tag, &val, obj_local);
                    self.forward_index
                        .add(oid, Assertion::Attr { key: name_tag, value: val }, TagOrigin::Direct);
                }
            }
        }

        fn build_vfs(self) -> TagVfs {
            TagVfs::new(self.tag_index, self.kv_index, self.forward_index, self.dag)
        }
    }

    fn music_fixture() -> TestFixture {
        let mut f = TestFixture::new();
        let electronic = f.register_tag(1, "electronic");
        let ambient = f.register_tag(2, "ambient");
        let _portable = f.register_tag(3, "portable");
        let name_attr = f.register_attr(10, "name");
        let _ = name_attr;

        // obj 1: electronic, ambient
        f.add_object(1, &[electronic, ambient], Some("track1.flac"));
        // obj 2: electronic
        f.add_object(2, &[electronic], Some("track2.mp3"));
        // obj 3: ambient
        f.add_object(3, &[ambient], Some("drone.wav"));

        f
    }

    #[test]
    fn root_has_tags_dir() {
        let mut vfs = music_fixture().build_vfs();
        let entries = vfs.readdir(INO_ROOT).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"tags"));
    }

    #[test]
    fn tags_root_lists_populated_tags() {
        let mut vfs = music_fixture().build_vfs();
        let entries = vfs.readdir(INO_TAGS).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        // Should list electronic and ambient (portable has no objects)
        assert!(names.contains(&"electronic"));
        assert!(names.contains(&"ambient"));
        assert!(!names.contains(&"portable"));
    }

    #[test]
    fn lookup_tag_dir() {
        let mut vfs = music_fixture().build_vfs();
        let ino = vfs.lookup(INO_TAGS, "electronic").unwrap();
        let entry = vfs.get(ino).unwrap();
        assert!(matches!(entry, TagVfsEntry::TagDir(tags) if tags.len() == 1));
    }

    #[test]
    fn tag_dir_lists_files_and_refinements() {
        let mut vfs = music_fixture().build_vfs();
        let tag_ino = vfs.lookup(INO_TAGS, "electronic").unwrap();
        let entries = vfs.readdir(tag_ino).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        // Files: track1.flac (obj 1), track2.mp3 (obj 2)
        assert!(names.contains(&"track1.flac"));
        assert!(names.contains(&"track2.mp3"));

        // Refinement: ambient (obj 1 has both electronic and ambient)
        assert!(names.contains(&"ambient"));

        // Not: portable (no electronic+portable intersection)
        assert!(!names.contains(&"portable"));
    }

    #[test]
    fn nested_tag_intersection() {
        let mut vfs = music_fixture().build_vfs();
        let elec_ino = vfs.lookup(INO_TAGS, "electronic").unwrap();
        let amb_ino = vfs.lookup(elec_ino, "ambient").unwrap();

        let entries = vfs.readdir(amb_ino).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        // Only obj 1 has both electronic AND ambient
        assert!(names.contains(&"track1.flac"));
        assert!(!names.contains(&"track2.mp3"));
        assert!(!names.contains(&"drone.wav"));
    }

    #[test]
    fn file_lookup_by_name() {
        let mut vfs = music_fixture().build_vfs();
        let tag_ino = vfs.lookup(INO_TAGS, "electronic").unwrap();
        let file_ino = vfs.lookup(tag_ino, "track1.flac").unwrap();
        let entry = vfs.get(file_ino).unwrap();
        assert!(matches!(entry, TagVfsEntry::TagFile { obj_local: 1, .. }));
    }

    #[test]
    fn file_attr_returns_size() {
        let f = music_fixture();
        let mut vfs = f.build_vfs();

        // Set blob data for obj 1.
        let oid = ObjectId::new(0, 1);
        vfs.set_blob(oid.raw(), vec![0u8; 1024]);

        let tag_ino = vfs.lookup(INO_TAGS, "electronic").unwrap();
        let file_ino = vfs.lookup(tag_ino, "track1.flac").unwrap();

        let attr = vfs.getattr(file_ino).unwrap();
        assert_eq!(attr.size, 1024);
        assert_eq!(attr.kind, VfsFileType::RegularFile);
    }

    #[test]
    fn read_file_content() {
        let mut vfs = music_fixture().build_vfs();
        let oid = ObjectId::new(0, 1);
        vfs.set_blob(oid.raw(), b"hello world".to_vec());

        let tag_ino = vfs.lookup(INO_TAGS, "electronic").unwrap();
        let file_ino = vfs.lookup(tag_ino, "track1.flac").unwrap();

        let data = vfs.read(file_ino, 0, 1024).unwrap();
        assert_eq!(data, b"hello world");

        // Partial read with offset.
        let data = vfs.read(file_ino, 6, 5).unwrap();
        assert_eq!(data, b"world");
    }

    #[test]
    fn objects_without_name_use_obj_id() {
        let mut f = TestFixture::new();
        let music = f.register_tag(1, "music");
        // No "name" attribute registered, so obj has no name.
        f.tag_index.tag_object(music, 42);
        let oid = ObjectId::new(0, 42);
        f.forward_index
            .add(oid, Assertion::Tag(music), TagOrigin::Direct);

        let mut vfs = f.build_vfs();
        let tag_ino = vfs.lookup(INO_TAGS, "music").unwrap();
        let entries = vfs.readdir(tag_ino).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"obj_42"));
    }

    #[test]
    fn duplicate_names_disambiguated() {
        let mut f = TestFixture::new();
        let music = f.register_tag(1, "music");
        let _name_attr = f.register_attr(10, "name");

        f.add_object(1, &[music], Some("track.mp3"));
        f.add_object(2, &[music], Some("track.mp3"));

        let mut vfs = f.build_vfs();
        let tag_ino = vfs.lookup(INO_TAGS, "music").unwrap();
        let entries = vfs.readdir(tag_ino).unwrap();
        let file_names: Vec<&str> = entries
            .iter()
            .filter(|e| e.file_type == VfsFileType::RegularFile)
            .map(|e| e.name.as_str())
            .collect();

        // Both should be present with disambiguation.
        assert_eq!(file_names.len(), 2);
        assert!(file_names.contains(&"track_1.mp3"));
        assert!(file_names.contains(&"track_2.mp3"));
    }

    #[test]
    fn commutativity_of_tag_paths() {
        let mut vfs = music_fixture().build_vfs();

        // /tags/electronic/ambient and /tags/ambient/electronic
        // should show the same files.
        let elec_ino = vfs.lookup(INO_TAGS, "electronic").unwrap();
        let elec_amb_ino = vfs.lookup(elec_ino, "ambient").unwrap();

        let amb_ino = vfs.lookup(INO_TAGS, "ambient").unwrap();
        let amb_elec_ino = vfs.lookup(amb_ino, "electronic").unwrap();

        // Same inode (same BTreeSet of tags).
        assert_eq!(elec_amb_ino, amb_elec_ino);
    }

    #[test]
    fn ctx_dir_integration() {
        use mimisbrunnr_types::{PathProjection, ProjectedEntry};

        let f = TestFixture::new();
        let mut vfs = f.build_vfs();

        // Add a context subtree.
        let mut proj = PathProjection::new("test-ctx");
        proj.add(ProjectedEntry::file(ObjectId::new(0, 99), "hello.txt"));
        let tree = VfsTree::from_projection(&proj);
        vfs.add_context("test-ctx".into(), tree);

        // Root should now show ctx/.
        let root = vfs.readdir(INO_ROOT).unwrap();
        let names: Vec<&str> = root.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"ctx"));

        // ctx/ should list test-ctx.
        let ctx_entries = vfs.readdir(INO_CTX).unwrap();
        let ctx_names: Vec<&str> = ctx_entries.iter().map(|e| e.name.as_str()).collect();
        assert!(ctx_names.contains(&"test-ctx"));

        // test-ctx should list hello.txt.
        let ctx_ino = vfs.lookup(INO_CTX, "test-ctx").unwrap();
        let ctx_dir = vfs.readdir(ctx_ino).unwrap();
        let file_names: Vec<&str> = ctx_dir.iter().map(|e| e.name.as_str()).collect();
        assert!(file_names.contains(&"hello.txt"));
    }

    #[test]
    fn getattr_on_all_node_types() {
        let mut vfs = music_fixture().build_vfs();

        // Root
        let attr = vfs.getattr(INO_ROOT).unwrap();
        assert_eq!(attr.kind, VfsFileType::Directory);

        // TagsRoot
        let attr = vfs.getattr(INO_TAGS).unwrap();
        assert_eq!(attr.kind, VfsFileType::Directory);

        // Tag dir
        let ino = vfs.lookup(INO_TAGS, "electronic").unwrap();
        let attr = vfs.getattr(ino).unwrap();
        assert_eq!(attr.kind, VfsFileType::Directory);

        // File
        let file_ino = vfs.lookup(ino, "track1.flac").unwrap();
        let attr = vfs.getattr(file_ino).unwrap();
        assert_eq!(attr.kind, VfsFileType::RegularFile);
    }

    #[test]
    fn nonexistent_tag_lookup_returns_none() {
        let mut vfs = music_fixture().build_vfs();
        assert!(vfs.lookup(INO_TAGS, "nonexistent").is_none());
    }

    #[test]
    fn empty_intersection_not_shown() {
        let mut vfs = music_fixture().build_vfs();
        // portable tag has no objects, so it should not appear anywhere.
        let elec_ino = vfs.lookup(INO_TAGS, "electronic").unwrap();
        assert!(vfs.lookup(elec_ino, "portable").is_none());
    }
}
