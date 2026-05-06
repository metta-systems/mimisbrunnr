//! [`TagVfs`] — read-only tag/path VFS over the engine's in-memory mirrors.
//!
//! See `docs/DESIGN.md` §12.6 for the mount layout and the faceted-refinement
//! algorithm under `/tags/`. Everything here is **derived** state; nothing is
//! persisted.
//!
//! ## Concurrency
//!
//! `lookup` and `readdir` allocate inodes lazily. To preserve a `&self`
//! contract (so `fuser::Filesystem` can call us behind its `&self` methods)
//! we use interior `Mutex`es around the three pieces of mutable state:
//! the inode table, the `(parent, name) → inode` cache, and the next-inode
//! counter. None of this is hot — it gets touched once per traversed path.

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use log::trace;
use mimisbrunnr_index::{ForwardIndex, KvIndex, TagIndex};
use mimisbrunnr_ontology::OntologyState;
use mimisbrunnr_types::{ObjectId, TagId};
use mimisbrunnr_unix::{PathContextManager, PathProjection};
use roaring::RoaringBitmap;

use crate::attr::VfsAttr;
use crate::entry::{VfsEntry, VfsEntryKind};
use crate::inode::{INODE_CTX_ROOT, INODE_FIRST_DYNAMIC, INODE_ROOT, INODE_TAGS_ROOT, InodeId};

/// Read-only tag/path VFS (DESIGN §12.6).
///
/// Holds shared references into the engine's in-memory mirrors plus a
/// session-local inode table. Borrows are split into immutable (the
/// engine-owned indices) and interior-mutable (the inode bookkeeping); the
/// latter is `Mutex`-guarded so the FUSE adapter can call us behind its
/// `&self` methods.
pub struct TagVfs<'a> {
    /// Tag inverted index — primary source for `HasTag`, faceted refinement.
    pub tag_index: &'a TagIndex,
    /// KV equality index (kept for parity; not used in Phase 5b but listed
    /// in the spec so the engine can reuse the same handle when it adds
    /// attribute-driven views).
    pub kv_index: &'a KvIndex,
    /// Forward index — used to look up an object's per-context unix path
    /// when serving `/ctx/<context>/…`.
    pub forward_index: &'a ForwardIndex,
    /// Ontology — the source of truth for tag id ↔ name mappings.
    pub ontology: &'a OntologyState,
    /// Path-context manager — owns the per-context [`PathProjection`]s.
    pub context_trees: &'a PathContextManager,

    // ----- Session-local inode bookkeeping. -----
    entries: Mutex<HashMap<InodeId, VfsEntry>>,
    lookup_index: Mutex<HashMap<(InodeId, String), InodeId>>,
    next_ino: Mutex<u64>,
}

impl<'a> TagVfs<'a> {
    /// Build a new VFS over the given engine handles. Initialises the three
    /// well-known inodes (root, `/tags`, `/ctx`).
    pub fn new(
        tag_index: &'a TagIndex,
        kv_index: &'a KvIndex,
        forward_index: &'a ForwardIndex,
        ontology: &'a OntologyState,
        context_trees: &'a PathContextManager,
    ) -> Self {
        let mut entries: HashMap<InodeId, VfsEntry> = HashMap::new();
        entries.insert(
            INODE_ROOT,
            VfsEntry {
                inode: INODE_ROOT,
                kind: VfsEntryKind::Root,
                parent: None,
            },
        );
        entries.insert(
            INODE_TAGS_ROOT,
            VfsEntry {
                inode: INODE_TAGS_ROOT,
                kind: VfsEntryKind::TagsRoot,
                parent: Some(INODE_ROOT),
            },
        );
        entries.insert(
            INODE_CTX_ROOT,
            VfsEntry {
                inode: INODE_CTX_ROOT,
                kind: VfsEntryKind::CtxRoot,
                parent: Some(INODE_ROOT),
            },
        );

        let mut lookup_index: HashMap<(InodeId, String), InodeId> = HashMap::new();
        lookup_index.insert((INODE_ROOT, "tags".into()), INODE_TAGS_ROOT);
        lookup_index.insert((INODE_ROOT, "ctx".into()), INODE_CTX_ROOT);

        Self {
            tag_index,
            kv_index,
            forward_index,
            ontology,
            context_trees,
            entries: Mutex::new(entries),
            lookup_index: Mutex::new(lookup_index),
            next_ino: Mutex::new(INODE_FIRST_DYNAMIC),
        }
    }

    // ----------------------------------------------------------------------
    // Public API: lookup, getattr, readdir, allocate_inode.
    // ----------------------------------------------------------------------

    /// Resolve `(parent, name)` to a full [`VfsEntry`], allocating a fresh
    /// inode if the child has not been seen before.
    pub fn lookup(&self, parent: InodeId, name: &str) -> Option<VfsEntry> {
        trace!("tag_vfs::lookup parent={parent:?} name={name:?}");

        // Cached?
        if let Some(child) = self
            .lookup_index
            .lock()
            .ok()?
            .get(&(parent, name.to_string()))
            .copied()
        {
            return self.entry(child);
        }

        let parent_kind = self.entry(parent)?.kind;
        let child_kind = self.derive_child_kind(&parent_kind, name)?;
        let inode = self.allocate_inode(child_kind, parent, name);
        self.entry(inode)
    }

    /// stat-like info for `inode`. Returns `None` if the inode is unknown.
    pub fn getattr(&self, inode: InodeId) -> Option<VfsAttr> {
        let kind = self.entry(inode)?.kind;
        Some(self.attr_for_kind(&kind))
    }

    /// List the contents of the directory at `inode`. Returns `(name, child)`
    /// pairs in lexical-by-name order; entries that don't yet have an inode
    /// are allocated on the fly.
    ///
    /// Returns an empty `Vec` for non-directory inodes.
    pub fn readdir(&self, inode: InodeId) -> Vec<(String, VfsEntry)> {
        trace!("tag_vfs::readdir inode={inode:?}");
        let entry = match self.entry(inode) {
            Some(e) => e,
            None => return Vec::new(),
        };

        match entry.kind {
            VfsEntryKind::Root => self.readdir_root(),
            VfsEntryKind::TagsRoot => self.readdir_tags_root(inode),
            VfsEntryKind::CtxRoot => self.readdir_ctx_root(inode),
            VfsEntryKind::TagDir { ref current_tags } => {
                self.readdir_tag_dir(inode, current_tags)
            }
            VfsEntryKind::CtxDir { context, ref sub_path } => {
                self.readdir_ctx_dir(inode, context, sub_path.as_str())
            }
            VfsEntryKind::TagObject { .. } | VfsEntryKind::CtxObject { .. } => Vec::new(),
        }
    }

    /// Allocate (or reuse) the inode for `(parent, name)` of the given
    /// `kind`. If the `(parent, name)` lookup is already cached, the existing
    /// inode is returned and `kind` is **ignored** — kinds for a given
    /// `(parent, name)` are stable for the life of a `TagVfs`.
    pub fn allocate_inode(&self, kind: VfsEntryKind, parent: InodeId, name: &str) -> InodeId {
        // Fast path: already cached.
        {
            let lookup = self.lookup_index.lock().expect("lookup_index poisoned");
            if let Some(&existing) = lookup.get(&(parent, name.to_string())) {
                return existing;
            }
        }

        let mut next = self.next_ino.lock().expect("next_ino poisoned");
        let mut entries = self.entries.lock().expect("entries poisoned");
        let mut lookup = self.lookup_index.lock().expect("lookup_index poisoned");

        // Re-check under the write locks to handle a concurrent allocator.
        if let Some(&existing) = lookup.get(&(parent, name.to_string())) {
            return existing;
        }

        let id = *next;
        *next += 1;
        let inode = InodeId(id);
        entries.insert(
            inode,
            VfsEntry {
                inode,
                kind,
                parent: Some(parent),
            },
        );
        lookup.insert((parent, name.to_string()), inode);
        inode
    }

    /// Lookup the full entry record for `inode`.
    pub fn entry(&self, inode: InodeId) -> Option<VfsEntry> {
        self.entries
            .lock()
            .ok()
            .and_then(|m| m.get(&inode).cloned())
    }

    /// Borrow the projection for a `/ctx/` context inode.
    fn projection_for(&self, context: TagId) -> Option<&PathProjection> {
        self.context_trees.get(context)
    }

    // ----------------------------------------------------------------------
    // lookup helpers
    // ----------------------------------------------------------------------

    fn derive_child_kind(
        &self,
        parent_kind: &VfsEntryKind,
        name: &str,
    ) -> Option<VfsEntryKind> {
        match parent_kind {
            VfsEntryKind::Root => match name {
                "tags" => Some(VfsEntryKind::TagsRoot),
                "ctx" => Some(VfsEntryKind::CtxRoot),
                _ => None,
            },
            VfsEntryKind::TagsRoot => self.derive_child_under_tags_root(name),
            VfsEntryKind::TagDir { current_tags } => {
                self.derive_child_under_tag_dir(current_tags, name)
            }
            VfsEntryKind::CtxRoot => self.derive_child_under_ctx_root(name),
            VfsEntryKind::CtxDir { context, sub_path } => {
                self.derive_child_under_ctx_dir(*context, sub_path.as_str(), name)
            }
            VfsEntryKind::TagObject { .. } | VfsEntryKind::CtxObject { .. } => None,
        }
    }

    fn derive_child_under_tags_root(&self, name: &str) -> Option<VfsEntryKind> {
        let tag_id = *self.ontology.names.get(name)?;
        let bm = self.tag_index.query_simple(tag_id)?;
        if bm.is_empty() {
            return None;
        }
        let mut current_tags = BTreeSet::new();
        current_tags.insert(tag_id);
        Some(VfsEntryKind::TagDir { current_tags })
    }

    fn derive_child_under_tag_dir(
        &self,
        current_tags: &BTreeSet<TagId>,
        name: &str,
    ) -> Option<VfsEntryKind> {
        // Try sub-tag first.
        if let Some(&tag_id) = self.ontology.names.get(name) {
            if !current_tags.contains(&tag_id) {
                let current_bm = self.intersect(current_tags);
                let candidate_bm = self.tag_index.query_simple(tag_id)?;
                let refined = &current_bm & candidate_bm;
                if !refined.is_empty() {
                    let mut next = current_tags.clone();
                    next.insert(tag_id);
                    return Some(VfsEntryKind::TagDir { current_tags: next });
                }
            }
            // Fall through — name is a known tag but doesn't refine; not a
            // valid child here.
            return None;
        }

        // Otherwise try a decimal ObjectId.
        let raw: u64 = name.parse().ok()?;
        let oid = ObjectId::from_u64(raw);
        let current_bm = self.intersect(current_tags);
        if !current_bm.contains(low32_of(oid)) {
            return None;
        }
        Some(VfsEntryKind::TagObject { oid })
    }

    fn derive_child_under_ctx_root(&self, name: &str) -> Option<VfsEntryKind> {
        let tag_id = *self.ontology.names.get(name)?;
        if !self.context_trees.projections.contains_key(&tag_id) {
            return None;
        }
        Some(VfsEntryKind::CtxDir {
            context: tag_id,
            sub_path: String::new(),
        })
    }

    fn derive_child_under_ctx_dir(
        &self,
        context: TagId,
        sub_path: &str,
        name: &str,
    ) -> Option<VfsEntryKind> {
        let projection = self.projection_for(context)?;
        let candidate_path = if sub_path.is_empty() {
            name.to_string()
        } else {
            format!("{sub_path}/{name}")
        };

        // Is `candidate_path` an exact match (file)?
        if let Some(oid) = projection.reverse_lookup(&candidate_path) {
            return Some(VfsEntryKind::CtxObject { context, oid });
        }

        // Is `candidate_path` a directory prefix of some path?
        let prefix = format!("{candidate_path}/");
        if projection.iter().any(|(_, p)| p.starts_with(&prefix)) {
            return Some(VfsEntryKind::CtxDir {
                context,
                sub_path: candidate_path,
            });
        }

        None
    }

    // ----------------------------------------------------------------------
    // readdir helpers
    // ----------------------------------------------------------------------

    fn readdir_root(&self) -> Vec<(String, VfsEntry)> {
        let mut out = Vec::with_capacity(2);
        if let Some(e) = self.entry(INODE_TAGS_ROOT) {
            out.push(("tags".into(), e));
        }
        if let Some(e) = self.entry(INODE_CTX_ROOT) {
            out.push(("ctx".into(), e));
        }
        out
    }

    fn readdir_tags_root(&self, parent: InodeId) -> Vec<(String, VfsEntry)> {
        // Show every tag with a non-empty bitmap, sorted lexically by name.
        let mut names: Vec<(String, TagId)> = self
            .ontology
            .names
            .iter()
            .filter_map(|(name, &tag)| {
                let bm = self.tag_index.query_simple(tag)?;
                if bm.is_empty() {
                    None
                } else {
                    Some((name.clone(), tag))
                }
            })
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out = Vec::with_capacity(names.len());
        for (name, tag) in names {
            let mut current_tags = BTreeSet::new();
            current_tags.insert(tag);
            let inode = self.allocate_inode(
                VfsEntryKind::TagDir {
                    current_tags: current_tags.clone(),
                },
                parent,
                &name,
            );
            if let Some(entry) = self.entry(inode) {
                out.push((name, entry));
            }
        }
        out
    }

    fn readdir_tag_dir(
        &self,
        parent: InodeId,
        current_tags: &BTreeSet<TagId>,
    ) -> Vec<(String, VfsEntry)> {
        let current_bm = self.intersect(current_tags);

        // 1. Sub-tags whose bitmap intersects the current match (faceted
        //    refinement).
        let mut refinements: Vec<(String, TagId)> = self
            .ontology
            .names
            .iter()
            .filter_map(|(name, &tag)| {
                if current_tags.contains(&tag) {
                    return None;
                }
                let bm = self.tag_index.query_simple(tag)?;
                let refined = &current_bm & bm;
                if refined.is_empty() {
                    None
                } else {
                    Some((name.clone(), tag))
                }
            })
            .collect();
        refinements.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out: Vec<(String, VfsEntry)> = Vec::new();

        for (name, tag) in refinements {
            let mut next_tags = current_tags.clone();
            next_tags.insert(tag);
            let inode = self.allocate_inode(
                VfsEntryKind::TagDir {
                    current_tags: next_tags,
                },
                parent,
                &name,
            );
            if let Some(entry) = self.entry(inode) {
                out.push((name, entry));
            }
        }

        // 2. Matched objects, named by ObjectId in decimal.
        let mut matched: Vec<u32> = current_bm.iter().collect();
        matched.sort_unstable();
        for entry32 in matched {
            // The bitmap key is the low 32 bits of the local seq; reconstruct
            // a node-0 ObjectId for naming. See `mimisbrunnr_query` for the
            // same convention.
            let oid = ObjectId::from_parts(0, entry32 as u64);
            let name = format!("{}", oid.to_u64());
            let inode = self.allocate_inode(
                VfsEntryKind::TagObject { oid },
                parent,
                &name,
            );
            if let Some(entry) = self.entry(inode) {
                out.push((name, entry));
            }
        }

        out
    }

    fn readdir_ctx_root(&self, parent: InodeId) -> Vec<(String, VfsEntry)> {
        let mut names: Vec<(String, TagId)> = self
            .context_trees
            .projections
            .keys()
            .filter_map(|&tag| {
                self.ontology
                    .tags
                    .get(&tag)
                    .map(|def| (def.name.clone(), tag))
            })
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out = Vec::with_capacity(names.len());
        for (name, tag) in names {
            let inode = self.allocate_inode(
                VfsEntryKind::CtxDir {
                    context: tag,
                    sub_path: String::new(),
                },
                parent,
                &name,
            );
            if let Some(entry) = self.entry(inode) {
                out.push((name, entry));
            }
        }
        out
    }

    fn readdir_ctx_dir(
        &self,
        parent: InodeId,
        context: TagId,
        sub_path: &str,
    ) -> Vec<(String, VfsEntry)> {
        let projection = match self.projection_for(context) {
            Some(p) => p,
            None => return Vec::new(),
        };

        // Walk every (oid, path) pair. For each, strip `sub_path/`; the next
        // component is either an immediate file or a sub-directory name.
        let prefix = if sub_path.is_empty() {
            String::new()
        } else {
            format!("{sub_path}/")
        };

        // Two collections: dir names and (file_name, oid).
        let mut dir_names: BTreeSet<String> = BTreeSet::new();
        let mut files: BTreeSet<(String, ObjectId)> = BTreeSet::new();

        for (oid, path) in projection.iter() {
            if !prefix.is_empty() && !path.starts_with(&prefix) {
                continue;
            }
            let tail = &path[prefix.len()..];
            if tail.is_empty() {
                continue;
            }
            match tail.find('/') {
                Some(slash) => {
                    let dir = tail[..slash].to_string();
                    if !dir.is_empty() {
                        dir_names.insert(dir);
                    }
                }
                None => {
                    files.insert((tail.to_string(), oid));
                }
            }
        }

        let mut out: Vec<(String, VfsEntry)> = Vec::with_capacity(dir_names.len() + files.len());

        for name in dir_names {
            let next_sub = if sub_path.is_empty() {
                name.clone()
            } else {
                format!("{sub_path}/{name}")
            };
            let inode = self.allocate_inode(
                VfsEntryKind::CtxDir {
                    context,
                    sub_path: next_sub,
                },
                parent,
                &name,
            );
            if let Some(entry) = self.entry(inode) {
                out.push((name, entry));
            }
        }

        for (name, oid) in files {
            let inode =
                self.allocate_inode(VfsEntryKind::CtxObject { context, oid }, parent, &name);
            if let Some(entry) = self.entry(inode) {
                out.push((name, entry));
            }
        }

        out
    }

    // ----------------------------------------------------------------------
    // attr helper
    // ----------------------------------------------------------------------

    fn attr_for_kind(&self, kind: &VfsEntryKind) -> VfsAttr {
        match kind {
            VfsEntryKind::Root
            | VfsEntryKind::TagsRoot
            | VfsEntryKind::CtxRoot
            | VfsEntryKind::TagDir { .. }
            | VfsEntryKind::CtxDir { .. } => VfsAttr::directory(),
            VfsEntryKind::TagObject { .. } | VfsEntryKind::CtxObject { .. } => {
                // We don't know object size at this layer; the engine plumbs
                // a content provider into MimisbrunnrFs which provides bytes
                // on `read`. For `getattr` we report 0; the FUSE adapter
                // populates the size from the content provider.
                VfsAttr::regular_file(0)
            }
        }
    }

    // ----------------------------------------------------------------------
    // bitmap helper
    // ----------------------------------------------------------------------

    fn intersect(&self, tags: &BTreeSet<TagId>) -> RoaringBitmap {
        let v: Vec<TagId> = tags.iter().copied().collect();
        self.tag_index.intersect(&v)
    }
}

/// Pull the low 32 bits of an [`ObjectId`]'s local sequence — this is the
/// representation used by [`mimisbrunnr_query`] / [`mimisbrunnr_index`] when
/// they pack object ids into roaring bitmaps. See `mimisbrunnr_query`'s
/// module docs for the rationale.
///
/// TODO(rewrite-phase-N): the 32-bit truncation is a shared limitation; this
/// will lift when the bitmap representation widens.
fn low32_of(oid: ObjectId) -> u32 {
    (oid.local_seq() & 0xffff_ffff) as u32
}

// ----------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mimisbrunnr_index::{ForwardIndex, KvIndex, TagIndex};
    use mimisbrunnr_ontology::{IdAllocator, OntologyModule, OntologyState};
    use mimisbrunnr_types::{ObjectId, TagDefinition, TagId, TagSemantics};
    use mimisbrunnr_unix::PathContextManager;

    use super::*;

    fn label(name: &str) -> TagDefinition {
        TagDefinition {
            id: TagId::new(0),
            name: name.into(),
            semantics: TagSemantics::Label,
            implies: vec![],
            storage: None,
        }
    }

    fn grouping(name: &str) -> TagDefinition {
        TagDefinition {
            id: TagId::new(0),
            name: name.into(),
            semantics: TagSemantics::Grouping,
            implies: vec![],
            storage: None,
        }
    }

    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0, local)
    }

    /// A small fixture mirroring the spec example: tags `electronic`,
    /// `portable`, `stationary`, `discontinued`, `archive`. Objects:
    /// - 4242: electronic, portable
    /// - 4789: electronic, portable
    /// - 1000: electronic, stationary
    /// - 9999: archive
    /// - 7000: discontinued, electronic, stationary
    fn build_state() -> (
        OntologyState,
        TagIndex,
        KvIndex,
        ForwardIndex,
        PathContextManager,
    ) {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let module = OntologyModule {
            id: "test".into(),
            version: "0.1.0".into(),
            name: "test".into(),
            tags: vec![
                label("electronic"),
                label("portable"),
                label("stationary"),
                label("discontinued"),
                label("archive"),
            ],
            implications: vec![],
        };
        state.install(module, &mut alloc).unwrap();

        let electronic = state.names["electronic"];
        let portable = state.names["portable"];
        let stationary = state.names["stationary"];
        let discontinued = state.names["discontinued"];
        let archive = state.names["archive"];

        let mut tag_index = TagIndex::new();
        for o in [4242u64, 4789] {
            tag_index.add_member(electronic, oid(o));
            tag_index.add_member(portable, oid(o));
        }
        tag_index.add_member(electronic, oid(1000));
        tag_index.add_member(stationary, oid(1000));
        tag_index.add_member(archive, oid(9999));
        tag_index.add_member(discontinued, oid(7000));
        tag_index.add_member(electronic, oid(7000));
        tag_index.add_member(stationary, oid(7000));

        (
            state,
            tag_index,
            KvIndex::default(),
            ForwardIndex::default(),
            PathContextManager::new(),
        )
    }

    #[test]
    fn well_known_inodes_exist() {
        let (state, tag_index, kv_index, forward_index, ctx) = build_state();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx);
        assert!(vfs.entry(INODE_ROOT).is_some());
        assert!(vfs.entry(INODE_TAGS_ROOT).is_some());
        assert!(vfs.entry(INODE_CTX_ROOT).is_some());
    }

    #[test]
    fn lookup_root_tags_returns_tags_root() {
        let (state, tag_index, kv_index, forward_index, ctx) = build_state();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx);
        let entry = vfs.lookup(INODE_ROOT, "tags").unwrap();
        assert_eq!(entry.inode, INODE_TAGS_ROOT);
        assert!(matches!(entry.kind, VfsEntryKind::TagsRoot));
    }

    #[test]
    fn lookup_root_ctx_returns_ctx_root() {
        let (state, tag_index, kv_index, forward_index, ctx) = build_state();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx);
        let entry = vfs.lookup(INODE_ROOT, "ctx").unwrap();
        assert_eq!(entry.inode, INODE_CTX_ROOT);
    }

    #[test]
    fn tags_root_lookup_allocates_stable_inode() {
        let (state, tag_index, kv_index, forward_index, ctx) = build_state();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx);
        let first = vfs.lookup(INODE_TAGS_ROOT, "electronic").unwrap();
        let again = vfs.lookup(INODE_TAGS_ROOT, "electronic").unwrap();
        assert_eq!(first.inode, again.inode);
        assert!(matches!(first.kind, VfsEntryKind::TagDir { .. }));
    }

    #[test]
    fn tags_root_readdir_lists_populated_tags_lex_sorted() {
        let (state, tag_index, kv_index, forward_index, ctx) = build_state();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx);
        let listing = vfs.readdir(INODE_TAGS_ROOT);
        let names: Vec<&str> = listing.iter().map(|(n, _)| n.as_str()).collect();
        // All five tags have non-empty bitmaps, sorted alphabetically.
        assert_eq!(
            names,
            vec!["archive", "discontinued", "electronic", "portable", "stationary"]
        );
    }

    #[test]
    fn faceted_refinement_under_tag_dir() {
        let (state, tag_index, kv_index, forward_index, ctx) = build_state();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx);
        let electronic = vfs.lookup(INODE_TAGS_ROOT, "electronic").unwrap();
        let listing = vfs.readdir(electronic.inode);
        let names: Vec<&str> = listing.iter().map(|(n, _)| n.as_str()).collect();
        // Sub-tags first (sorted), then matched objects (sorted by oid).
        assert!(names.contains(&"discontinued"));
        assert!(names.contains(&"portable"));
        assert!(names.contains(&"stationary"));
        assert!(!names.contains(&"archive")); // archive ∩ electronic = ∅
        // Files are decimal oids (low 32 bits → just the local seq).
        assert!(names.contains(&"1000"));
        assert!(names.contains(&"4242"));
        assert!(names.contains(&"4789"));
        assert!(names.contains(&"7000"));
    }

    #[test]
    fn faceted_refinement_drops_empty_facet() {
        // Under /tags/electronic/portable, `discontinued` should not appear
        // because portable ∩ electronic ∩ discontinued = ∅.
        let (state, tag_index, kv_index, forward_index, ctx) = build_state();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx);
        let electronic = vfs.lookup(INODE_TAGS_ROOT, "electronic").unwrap();
        let portable = vfs.lookup(electronic.inode, "portable").unwrap();
        let listing = vfs.readdir(portable.inode);
        let names: Vec<&str> = listing.iter().map(|(n, _)| n.as_str()).collect();
        assert!(!names.contains(&"discontinued"));
        // The two portable electronic objects should be the only files.
        assert!(names.contains(&"4242"));
        assert!(names.contains(&"4789"));
        assert!(!names.contains(&"1000"));
    }

    #[test]
    fn lookup_object_under_tag_dir() {
        let (state, tag_index, kv_index, forward_index, ctx) = build_state();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx);
        let electronic = vfs.lookup(INODE_TAGS_ROOT, "electronic").unwrap();
        let obj = vfs.lookup(electronic.inode, "4242").unwrap();
        assert!(matches!(obj.kind, VfsEntryKind::TagObject { .. }));
    }

    #[test]
    fn getattr_on_tag_object_is_regular_file() {
        let (state, tag_index, kv_index, forward_index, ctx) = build_state();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx);
        let electronic = vfs.lookup(INODE_TAGS_ROOT, "electronic").unwrap();
        let obj = vfs.lookup(electronic.inode, "4242").unwrap();
        let attr = vfs.getattr(obj.inode).unwrap();
        assert_eq!(attr.kind, crate::attr::VfsAttrKind::RegularFile);
        assert_eq!(attr.nlink, 1);
        // Size is 0 at this layer; MimisbrunnrFs fills it from the content
        // provider.
        assert_eq!(attr.size, 0);
    }

    #[test]
    fn getattr_on_directories_reports_directory() {
        let (state, tag_index, kv_index, forward_index, ctx) = build_state();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx);
        for inode in [INODE_ROOT, INODE_TAGS_ROOT, INODE_CTX_ROOT] {
            let attr = vfs.getattr(inode).unwrap();
            assert_eq!(attr.kind, crate::attr::VfsAttrKind::Directory);
        }
    }

    #[test]
    fn missing_tag_lookup_returns_none() {
        let (state, tag_index, kv_index, forward_index, ctx) = build_state();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx);
        assert!(vfs.lookup(INODE_TAGS_ROOT, "nonexistent").is_none());
    }

    #[test]
    fn tag_path_order_is_canonical() {
        // /tags/electronic/portable and /tags/portable/electronic resolve to
        // the same TagDir kind (BTreeSet semantics). Inodes will differ
        // because the cache keys on (parent, name), but the *kinds* match.
        let (state, tag_index, kv_index, forward_index, ctx) = build_state();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx);

        let e = vfs.lookup(INODE_TAGS_ROOT, "electronic").unwrap();
        let ep = vfs.lookup(e.inode, "portable").unwrap();
        let p = vfs.lookup(INODE_TAGS_ROOT, "portable").unwrap();
        let pe = vfs.lookup(p.inode, "electronic").unwrap();

        match (&ep.kind, &pe.kind) {
            (
                VfsEntryKind::TagDir { current_tags: a },
                VfsEntryKind::TagDir { current_tags: b },
            ) => {
                assert_eq!(a, b);
            }
            other => panic!("unexpected kinds: {other:?}"),
        }
    }

    // ----- /ctx/ tests -----

    fn build_state_with_ctx() -> (
        OntologyState,
        TagIndex,
        KvIndex,
        ForwardIndex,
        PathContextManager,
        TagId,
    ) {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        state
            .install(
                OntologyModule {
                    id: "ctx-test".into(),
                    version: "0.1.0".into(),
                    name: "ctx-test".into(),
                    tags: vec![grouping("rpi4-sdcard"), label("electronic")],
                    implications: vec![],
                },
                &mut alloc,
            )
            .unwrap();

        let context = state.names["rpi4-sdcard"];
        let mut ctx = PathContextManager::new();
        ctx.create_context(&state, context, PathBuf::from("/host/rpi4")).unwrap();
        let proj = ctx.get_mut(context).unwrap();
        proj.add(oid(100), "boot/vesper".into()).unwrap();
        proj.add(oid(101), "boot/config.txt".into()).unwrap();
        proj.add(oid(102), "etc/fstab".into()).unwrap();

        (
            state,
            TagIndex::default(),
            KvIndex::default(),
            ForwardIndex::default(),
            ctx,
            context,
        )
    }

    #[test]
    fn ctx_root_lookup_returns_ctx_dir() {
        let (state, tag_index, kv_index, forward_index, ctx_mgr, _ctx_tag) =
            build_state_with_ctx();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx_mgr);
        let dir = vfs.lookup(INODE_CTX_ROOT, "rpi4-sdcard").unwrap();
        match &dir.kind {
            VfsEntryKind::CtxDir { sub_path, .. } => assert!(sub_path.is_empty()),
            other => panic!("expected CtxDir, got {other:?}"),
        }
    }

    #[test]
    fn ctx_root_readdir_lists_contexts() {
        let (state, tag_index, kv_index, forward_index, ctx_mgr, _ctx_tag) =
            build_state_with_ctx();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx_mgr);
        let listing = vfs.readdir(INODE_CTX_ROOT);
        let names: Vec<&str> = listing.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["rpi4-sdcard"]);
    }

    #[test]
    fn ctx_dir_readdir_lists_first_level_entries() {
        let (state, tag_index, kv_index, forward_index, ctx_mgr, _ctx_tag) =
            build_state_with_ctx();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx_mgr);
        let dir = vfs.lookup(INODE_CTX_ROOT, "rpi4-sdcard").unwrap();
        let listing = vfs.readdir(dir.inode);
        let names: Vec<&str> = listing.iter().map(|(n, _)| n.as_str()).collect();
        // boot and etc directories synthesised; no top-level files.
        assert!(names.contains(&"boot"));
        assert!(names.contains(&"etc"));
    }

    #[test]
    fn ctx_dir_lookup_resolves_file() {
        let (state, tag_index, kv_index, forward_index, ctx_mgr, _ctx_tag) =
            build_state_with_ctx();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx_mgr);
        let ctx_dir = vfs.lookup(INODE_CTX_ROOT, "rpi4-sdcard").unwrap();
        let boot = vfs.lookup(ctx_dir.inode, "boot").unwrap();
        let vesper = vfs.lookup(boot.inode, "vesper").unwrap();
        match vesper.kind {
            VfsEntryKind::CtxObject { oid: o, .. } => assert_eq!(o, oid(100)),
            other => panic!("expected CtxObject, got {other:?}"),
        }
    }

    #[test]
    fn ctx_dir_readdir_subdir_lists_files() {
        let (state, tag_index, kv_index, forward_index, ctx_mgr, _ctx_tag) =
            build_state_with_ctx();
        let vfs = TagVfs::new(&tag_index, &kv_index, &forward_index, &state, &ctx_mgr);
        let ctx_dir = vfs.lookup(INODE_CTX_ROOT, "rpi4-sdcard").unwrap();
        let boot = vfs.lookup(ctx_dir.inode, "boot").unwrap();
        let listing = vfs.readdir(boot.inode);
        let names: Vec<&str> = listing.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"vesper"));
        assert!(names.contains(&"config.txt"));
    }
}
