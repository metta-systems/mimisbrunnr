use std::{collections::HashMap, time::SystemTime};

use mimisbrunnr_types::{ObjectId, PathProjection, ProjectedEntryType};

/// File type in the VFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsFileType {
    RegularFile,
    Directory,
    Symlink,
}

/// Attributes for an inode (modeled after POSIX stat).
#[derive(Debug, Clone)]
pub struct VfsAttr {
    pub ino: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub kind: VfsFileType,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
}

impl VfsAttr {
    fn dir(ino: u64, mode: u32) -> Self {
        Self {
            ino,
            size: 0,
            blocks: 0,
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
            kind: VfsFileType::Directory,
            mode,
            nlink: 2,
            uid: 0,
            gid: 0,
        }
    }

    fn file(ino: u64, mode: u32, size: u64) -> Self {
        Self {
            ino,
            size,
            blocks: size.div_ceil(512),
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
            kind: VfsFileType::RegularFile,
            mode,
            nlink: 1,
            uid: 0,
            gid: 0,
        }
    }

    fn symlink(ino: u64) -> Self {
        Self {
            ino,
            size: 0,
            blocks: 0,
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
            kind: VfsFileType::Symlink,
            mode: 0o777,
            nlink: 1,
            uid: 0,
            gid: 0,
        }
    }
}

/// A node in the VFS tree.
#[derive(Debug, Clone)]
pub struct VfsNode {
    pub ino: u64,
    pub name: String,
    pub attr: VfsAttr,
    /// The object ID backing this file (None for dirs/symlinks without objects).
    pub object: Option<ObjectId>,
    /// For symlinks: the target path.
    pub symlink_target: Option<String>,
    /// Child inode numbers (for directories).
    pub children: Vec<u64>,
    /// Parent inode number.
    pub parent: u64,
}

/// An inode-indexed VFS tree built from a PathProjection.
///
/// FUSE operates on inodes, not paths. This structure maps the projection's
/// path entries to a tree of inodes that can serve FUSE operations like
/// `lookup`, `getattr`, `readdir`, and `read`.
pub struct VfsTree {
    /// All nodes indexed by inode number.
    nodes: HashMap<u64, VfsNode>,
    /// Path → inode lookup.
    path_to_ino: HashMap<String, u64>,
    /// Next inode to assign.
    next_ino: u64,
}

impl VfsTree {
    /// Build a VFS tree from a path projection.
    pub fn from_projection(proj: &PathProjection) -> Self {
        let mut tree = Self {
            nodes: HashMap::new(),
            path_to_ino: HashMap::new(),
            next_ino: 2, // 1 = root
        };

        // Create root directory (inode 1)
        let root = VfsNode {
            ino: 1,
            name: String::new(),
            attr: VfsAttr::dir(1, 0o755),
            object: None,
            symlink_target: None,
            children: Vec::new(),
            parent: 1, // root's parent is itself
        };
        tree.nodes.insert(1, root);
        tree.path_to_ino.insert(String::new(), 1);

        // Synthesize directories first
        let with_dirs = proj.with_synthesized_dirs();

        // Sort entries so parents are created before children
        let mut entries: Vec<_> = with_dirs.entries.iter().collect();
        entries.sort_by_key(|e| e.path.matches('/').count());

        for entry in entries {
            tree.add_entry(&entry.path, entry.object, &entry.entry_type);
        }

        tree
    }

    fn add_entry(&mut self, path: &str, object: Option<ObjectId>, entry_type: &ProjectedEntryType) {
        if self.path_to_ino.contains_key(path) {
            return; // Already exists (e.g., dir already synthesized)
        }

        let ino = self.next_ino;
        self.next_ino += 1;

        // Determine parent
        let parent_path = match path.rfind('/') {
            Some(i) => &path[..i],
            None => "",
        };
        let parent_ino = *self.path_to_ino.get(parent_path).unwrap_or(&1);

        let name = match path.rfind('/') {
            Some(i) => &path[i + 1..],
            None => path,
        };

        let attr = match entry_type {
            ProjectedEntryType::File { mode, uid, gid } => {
                let mut a = VfsAttr::file(ino, *mode, 0);
                a.uid = *uid;
                a.gid = *gid;
                a
            }
            ProjectedEntryType::Directory { mode } => VfsAttr::dir(ino, *mode),
            ProjectedEntryType::Symlink { .. } => VfsAttr::symlink(ino),
        };

        let symlink_target = match entry_type {
            ProjectedEntryType::Symlink { target } => Some(target.clone()),
            _ => None,
        };

        let node = VfsNode {
            ino,
            name: name.to_string(),
            attr,
            object,
            symlink_target,
            children: Vec::new(),
            parent: parent_ino,
        };

        self.nodes.insert(ino, node);
        self.path_to_ino.insert(path.to_string(), ino);

        // Add as child of parent
        if let Some(parent) = self.nodes.get_mut(&parent_ino) {
            parent.children.push(ino);
        }
    }

    /// Get a node by inode number.
    pub fn get(&self, ino: u64) -> Option<&VfsNode> {
        self.nodes.get(&ino)
    }

    /// Get a node by path.
    pub fn get_by_path(&self, path: &str) -> Option<&VfsNode> {
        self.path_to_ino
            .get(path)
            .and_then(|ino| self.nodes.get(ino))
    }

    /// Lookup a child by name within a directory inode.
    pub fn lookup(&self, parent_ino: u64, name: &str) -> Option<&VfsNode> {
        let parent = self.nodes.get(&parent_ino)?;
        for &child_ino in &parent.children {
            if let Some(child) = self.nodes.get(&child_ino)
                && child.name == name
            {
                return Some(child);
            }
        }

        None
    }

    /// List children of a directory inode (for readdir).
    pub fn readdir(&self, dir_ino: u64) -> Option<Vec<(u64, &str, VfsFileType)>> {
        let dir = self.nodes.get(&dir_ino)?;
        if dir.attr.kind != VfsFileType::Directory {
            return None;
        }

        let mut entries = vec![
            (dir.ino, ".", VfsFileType::Directory),
            (dir.parent, "..", VfsFileType::Directory),
        ];

        for &child_ino in &dir.children {
            if let Some(child) = self.nodes.get(&child_ino) {
                entries.push((child.ino, &child.name, child.attr.kind));
            }
        }

        Some(entries)
    }

    /// Get the root inode.
    pub fn root(&self) -> &VfsNode {
        self.nodes.get(&1).unwrap()
    }

    /// Total number of inodes.
    pub fn inode_count(&self) -> usize {
        self.nodes.len()
    }

    /// Update file size for a given object (called when blob size is known).
    pub fn set_file_size(&mut self, ino: u64, size: u64) {
        if let Some(node) = self.nodes.get_mut(&ino) {
            node.attr.size = size;
            node.attr.blocks = size.div_ceil(512);
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, mimisbrunnr_types::ProjectedEntry};

    fn oid(n: u64) -> ObjectId {
        ObjectId::new(0, n)
    }

    fn sample_projection() -> PathProjection {
        let mut proj = PathProjection::new("test");
        proj.add(ProjectedEntry::file(oid(1), "README.md"));
        proj.add(ProjectedEntry::file(oid(2), "Cargo.toml"));
        proj.add(ProjectedEntry::file(oid(3), "src/main.rs"));
        proj.add(ProjectedEntry::file(oid(4), "src/lib.rs"));
        proj.add(ProjectedEntry::file(oid(5), "src/util/helpers.rs"));
        proj.add(ProjectedEntry::symlink("src/util/link.rs", "../lib.rs"));
        proj
    }

    #[test]
    fn build_from_projection() {
        let proj = sample_projection();
        let tree = VfsTree::from_projection(&proj);

        // Root + 2 root files + src dir + 2 src files + util dir + 1 util file + 1 symlink
        assert!(tree.inode_count() >= 9);

        // Root exists
        let root = tree.root();
        assert_eq!(root.ino, 1);
        assert_eq!(root.attr.kind, VfsFileType::Directory);
    }

    #[test]
    fn lookup_root_children() {
        let proj = sample_projection();
        let tree = VfsTree::from_projection(&proj);

        let readme = tree.lookup(1, "README.md").unwrap();
        assert_eq!(readme.attr.kind, VfsFileType::RegularFile);
        assert_eq!(readme.object, Some(oid(1)));

        let cargo = tree.lookup(1, "Cargo.toml").unwrap();
        assert_eq!(cargo.object, Some(oid(2)));

        let src = tree.lookup(1, "src").unwrap();
        assert_eq!(src.attr.kind, VfsFileType::Directory);
    }

    #[test]
    fn lookup_nested() {
        let proj = sample_projection();
        let tree = VfsTree::from_projection(&proj);

        let src = tree.lookup(1, "src").unwrap();
        let main = tree.lookup(src.ino, "main.rs").unwrap();
        assert_eq!(main.object, Some(oid(3)));

        let util = tree.lookup(src.ino, "util").unwrap();
        let helpers = tree.lookup(util.ino, "helpers.rs").unwrap();
        assert_eq!(helpers.object, Some(oid(5)));
    }

    #[test]
    fn lookup_nonexistent() {
        let proj = sample_projection();
        let tree = VfsTree::from_projection(&proj);

        assert!(tree.lookup(1, "nonexistent").is_none());
    }

    #[test]
    fn get_by_path() {
        let proj = sample_projection();
        let tree = VfsTree::from_projection(&proj);

        let node = tree.get_by_path("src/main.rs").unwrap();
        assert_eq!(node.object, Some(oid(3)));

        let root = tree.get_by_path("").unwrap();
        assert_eq!(root.ino, 1);
    }

    #[test]
    fn readdir_root() {
        let proj = sample_projection();
        let tree = VfsTree::from_projection(&proj);

        let entries = tree.readdir(1).unwrap();
        // ".", "..", README.md, Cargo.toml, src
        assert!(entries.len() >= 5);

        let names: Vec<&str> = entries.iter().map(|(_, name, _)| *name).collect();
        assert!(names.contains(&"."));
        assert!(names.contains(&".."));
        assert!(names.contains(&"README.md"));
        assert!(names.contains(&"src"));
    }

    #[test]
    fn readdir_subdir() {
        let proj = sample_projection();
        let tree = VfsTree::from_projection(&proj);

        let src = tree.lookup(1, "src").unwrap();
        let entries = tree.readdir(src.ino).unwrap();

        let names: Vec<&str> = entries.iter().map(|(_, name, _)| *name).collect();
        assert!(names.contains(&"main.rs"));
        assert!(names.contains(&"lib.rs"));
        assert!(names.contains(&"util"));
    }

    #[test]
    fn readdir_non_directory_returns_none() {
        let proj = sample_projection();
        let tree = VfsTree::from_projection(&proj);

        let readme = tree.lookup(1, "README.md").unwrap();
        assert!(tree.readdir(readme.ino).is_none());
    }

    #[test]
    fn symlink_node() {
        let proj = sample_projection();
        let tree = VfsTree::from_projection(&proj);

        let src = tree.lookup(1, "src").unwrap();
        let util = tree.lookup(src.ino, "util").unwrap();
        let link = tree.lookup(util.ino, "link.rs").unwrap();

        assert_eq!(link.attr.kind, VfsFileType::Symlink);
        assert_eq!(link.symlink_target.as_deref(), Some("../lib.rs"));
        assert_eq!(link.object, None);
    }

    #[test]
    fn parent_inodes() {
        let proj = sample_projection();
        let tree = VfsTree::from_projection(&proj);

        let readme = tree.lookup(1, "README.md").unwrap();
        assert_eq!(readme.parent, 1); // parent is root

        let src = tree.lookup(1, "src").unwrap();
        let main = tree.lookup(src.ino, "main.rs").unwrap();
        assert_eq!(main.parent, src.ino); // parent is src
    }

    #[test]
    fn set_file_size() {
        let proj = sample_projection();
        let mut tree = VfsTree::from_projection(&proj);

        let readme = tree.lookup(1, "README.md").unwrap();
        let ino = readme.ino;
        assert_eq!(readme.attr.size, 0); // initially unknown

        tree.set_file_size(ino, 12345);
        let readme = tree.get(ino).unwrap();
        assert_eq!(readme.attr.size, 12345);
        assert_eq!(readme.attr.blocks, 12345u64.div_ceil(512));
    }

    #[test]
    fn file_permissions() {
        let mut proj = PathProjection::new("test");
        proj.add(ProjectedEntry::file_with_mode(oid(1), "script.sh", 0o755));
        proj.add(ProjectedEntry::file(oid(2), "data.txt")); // default 0o644

        let tree = VfsTree::from_projection(&proj);

        let script = tree.get_by_path("script.sh").unwrap();
        assert_eq!(script.attr.mode, 0o755);

        let data = tree.get_by_path("data.txt").unwrap();
        assert_eq!(data.attr.mode, 0o644);
    }

    #[test]
    fn empty_projection() {
        let proj = PathProjection::new("empty");
        let tree = VfsTree::from_projection(&proj);

        assert_eq!(tree.inode_count(), 1); // just root
        let entries = tree.readdir(1).unwrap();
        assert_eq!(entries.len(), 2); // just "." and ".."
    }

    #[test]
    fn deeply_nested() {
        let mut proj = PathProjection::new("deep");
        proj.add(ProjectedEntry::file(oid(1), "a/b/c/d/e/f.txt"));

        let tree = VfsTree::from_projection(&proj);

        // Should have synthesized dirs: a, a/b, a/b/c, a/b/c/d, a/b/c/d/e
        assert!(tree.get_by_path("a").is_some());
        assert!(tree.get_by_path("a/b").is_some());
        assert!(tree.get_by_path("a/b/c/d/e").is_some());
        assert!(tree.get_by_path("a/b/c/d/e/f.txt").is_some());
    }

    #[test]
    fn inode_stability() {
        let proj = sample_projection();
        let tree = VfsTree::from_projection(&proj);

        // Getting by path then by inode should give the same node
        let by_path = tree.get_by_path("src/main.rs").unwrap();
        let by_ino = tree.get(by_path.ino).unwrap();
        assert_eq!(by_path.ino, by_ino.ino);
        assert_eq!(by_path.object, by_ino.object);
    }
}
