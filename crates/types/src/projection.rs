use crate::ObjectId;

/// Type of entry in a path projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectedEntryType {
    File { mode: u32, uid: u32, gid: u32 },
    Symlink { target: String },
    Directory { mode: u32 },
}

impl ProjectedEntryType {
    pub fn file() -> Self {
        Self::File {
            mode: 0o644,
            uid: 0,
            gid: 0,
        }
    }

    pub fn file_with_mode(mode: u32) -> Self {
        Self::File {
            mode,
            uid: 0,
            gid: 0,
        }
    }

    pub fn executable() -> Self {
        Self::File {
            mode: 0o755,
            uid: 0,
            gid: 0,
        }
    }

    pub fn symlink(target: impl Into<String>) -> Self {
        Self::Symlink {
            target: target.into(),
        }
    }

    pub fn directory() -> Self {
        Self::Directory { mode: 0o755 }
    }

    pub fn is_file(&self) -> bool {
        matches!(self, Self::File { .. })
    }

    pub fn is_directory(&self) -> bool {
        matches!(self, Self::Directory { .. })
    }

    pub fn is_symlink(&self) -> bool {
        matches!(self, Self::Symlink { .. })
    }
}

/// A single entry in a path projection: maps an object to a Unix path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedEntry {
    pub object: Option<ObjectId>,
    pub path: String,
    pub entry_type: ProjectedEntryType,
}

impl ProjectedEntry {
    pub fn file(object: ObjectId, path: impl Into<String>) -> Self {
        Self {
            object: Some(object),
            path: path.into(),
            entry_type: ProjectedEntryType::file(),
        }
    }

    pub fn file_with_mode(object: ObjectId, path: impl Into<String>, mode: u32) -> Self {
        Self {
            object: Some(object),
            path: path.into(),
            entry_type: ProjectedEntryType::file_with_mode(mode),
        }
    }

    pub fn symlink(path: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            object: None,
            path: path.into(),
            entry_type: ProjectedEntryType::symlink(target),
        }
    }

    pub fn directory(path: impl Into<String>) -> Self {
        Self {
            object: None,
            path: path.into(),
            entry_type: ProjectedEntryType::directory(),
        }
    }

    pub fn parent(&self) -> Option<&str> {
        let path = self.path.trim_end_matches('/');
        path.rfind('/').map(|i| &path[..i])
    }

    pub fn name(&self) -> &str {
        let path = self.path.trim_end_matches('/');
        match path.rfind('/') {
            Some(i) => &path[i + 1..],
            None => path,
        }
    }
}

/// A named Unix filesystem projection — a manifest of path-to-object mappings.
#[derive(Debug, Clone)]
pub struct PathProjection {
    pub context: String,
    pub entries: Vec<ProjectedEntry>,
}

impl PathProjection {
    pub fn new(context: impl Into<String>) -> Self {
        Self {
            context: context.into(),
            entries: Vec::new(),
        }
    }

    pub fn add(&mut self, entry: ProjectedEntry) {
        self.entries.push(entry);
    }

    pub fn get(&self, path: &str) -> Option<&ProjectedEntry> {
        self.entries.iter().find(|e| e.path == path)
    }

    pub fn remove(&mut self, path: &str) -> bool {
        let len = self.entries.len();
        self.entries.retain(|e| e.path != path);
        self.entries.len() < len
    }

    pub fn list_dir(&self, dir_path: &str) -> Vec<&ProjectedEntry> {
        let prefix = if dir_path.is_empty() || dir_path == "/" {
            String::new()
        } else {
            let mut p = dir_path.to_string();
            if !p.ends_with('/') {
                p.push('/');
            }
            p
        };

        self.entries
            .iter()
            .filter(|e| {
                if prefix.is_empty() {
                    !e.path.contains('/')
                } else if let Some(rest) = e.path.strip_prefix(&prefix) {
                    !rest.contains('/')
                } else {
                    false
                }
            })
            .collect()
    }

    pub fn with_synthesized_dirs(&self) -> Self {
        let mut result = self.clone();
        let mut dirs = std::collections::BTreeSet::new();
        for entry in &self.entries {
            let mut path = entry.path.as_str();
            while let Some(i) = path.rfind('/') {
                path = &path[..i];
                if !path.is_empty() {
                    dirs.insert(path.to_string());
                }
            }
        }
        for dir in dirs {
            if result.get(&dir).is_none() {
                result.add(ProjectedEntry::directory(dir));
            }
        }
        result
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn files(&self) -> impl Iterator<Item = &ProjectedEntry> {
        self.entries.iter().filter(|e| e.entry_type.is_file())
    }

    pub fn tree(&self) -> Vec<&str> {
        let mut paths: Vec<&str> = self.entries.iter().map(|e| e.path.as_str()).collect();
        paths.sort();
        paths
    }
}

#[cfg(test)]
mod tests {
    use {super::*, arbitrary_int::u48};

    fn oid(n: u64) -> ObjectId {
        ObjectId::new(0, u48::from_u64(n))
    }

    #[test]
    fn create_projection() {
        let proj = PathProjection::new("test-context");
        assert_eq!(proj.context, "test-context");
        assert!(proj.is_empty());
    }

    #[test]
    fn add_and_get_entries() {
        let mut proj = PathProjection::new("ctx");
        proj.add(ProjectedEntry::file(oid(1), "src/main.rs"));
        proj.add(ProjectedEntry::file(oid(2), "src/lib.rs"));
        proj.add(ProjectedEntry::file_with_mode(
            oid(3),
            "target/release/app",
            0o755,
        ));

        assert_eq!(proj.len(), 3);
        assert_eq!(proj.get("src/main.rs").unwrap().object, Some(oid(1)));
        assert_eq!(proj.get("nonexistent"), None);
    }

    #[test]
    fn remove_entry() {
        let mut proj = PathProjection::new("ctx");
        proj.add(ProjectedEntry::file(oid(1), "a.txt"));
        proj.add(ProjectedEntry::file(oid(2), "b.txt"));

        assert!(proj.remove("a.txt"));
        assert_eq!(proj.len(), 1);
        assert!(!proj.remove("a.txt")); // already removed
    }

    #[test]
    fn list_dir_root() {
        let mut proj = PathProjection::new("ctx");
        proj.add(ProjectedEntry::file(oid(1), "README.md"));
        proj.add(ProjectedEntry::file(oid(2), "Cargo.toml"));
        proj.add(ProjectedEntry::file(oid(3), "src/main.rs"));

        let root = proj.list_dir("");
        assert_eq!(root.len(), 2); // README.md, Cargo.toml (not src/main.rs)
    }

    #[test]
    fn list_dir_subdir() {
        let mut proj = PathProjection::new("ctx");
        proj.add(ProjectedEntry::file(oid(1), "src/main.rs"));
        proj.add(ProjectedEntry::file(oid(2), "src/lib.rs"));
        proj.add(ProjectedEntry::file(oid(3), "src/util/helpers.rs"));
        proj.add(ProjectedEntry::file(oid(4), "tests/test.rs"));

        let src = proj.list_dir("src");
        assert_eq!(src.len(), 2); // main.rs, lib.rs (not util/helpers.rs)
    }

    #[test]
    fn synthesize_directories() {
        let mut proj = PathProjection::new("ctx");
        proj.add(ProjectedEntry::file(oid(1), "src/main.rs"));
        proj.add(ProjectedEntry::file(oid(2), "src/util/helpers.rs"));
        proj.add(ProjectedEntry::file(oid(3), "tests/test.rs"));

        let with_dirs = proj.with_synthesized_dirs();
        // Should have: src, src/util, tests as synthesized dirs
        assert!(with_dirs.get("src").unwrap().entry_type.is_directory());
        assert!(with_dirs.get("src/util").unwrap().entry_type.is_directory());
        assert!(with_dirs.get("tests").unwrap().entry_type.is_directory());
        // Original files still present
        assert!(with_dirs.get("src/main.rs").unwrap().entry_type.is_file());
    }

    #[test]
    fn entry_parent_and_name() {
        let e = ProjectedEntry::file(oid(1), "src/util/helpers.rs");
        assert_eq!(e.parent(), Some("src/util"));
        assert_eq!(e.name(), "helpers.rs");

        let root_file = ProjectedEntry::file(oid(2), "README.md");
        assert_eq!(root_file.parent(), None);
        assert_eq!(root_file.name(), "README.md");
    }

    #[test]
    fn symlink_entry() {
        let e = ProjectedEntry::symlink("link.txt", "../target/file.txt");
        assert!(e.entry_type.is_symlink());
        assert_eq!(e.object, None);
        match &e.entry_type {
            ProjectedEntryType::Symlink { target } => {
                assert_eq!(target, "../target/file.txt");
            }
            _ => panic!("expected symlink"),
        }
    }

    #[test]
    fn same_object_multiple_paths() {
        let mut proj = PathProjection::new("ctx");
        let kernel = oid(42);
        proj.add(ProjectedEntry::file(kernel, "target/release/vesper"));
        proj.add(ProjectedEntry::file_with_mode(
            kernel,
            "boot/kernel8.img",
            0o755,
        ));

        // Same object, different paths
        assert_eq!(
            proj.get("target/release/vesper").unwrap().object,
            Some(kernel)
        );
        assert_eq!(proj.get("boot/kernel8.img").unwrap().object, Some(kernel));
    }

    #[test]
    fn tree_sorted() {
        let mut proj = PathProjection::new("ctx");
        proj.add(ProjectedEntry::file(oid(1), "c.txt"));
        proj.add(ProjectedEntry::file(oid(2), "a.txt"));
        proj.add(ProjectedEntry::file(oid(3), "b.txt"));

        let tree = proj.tree();
        assert_eq!(tree, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn files_iterator() {
        let mut proj = PathProjection::new("ctx");
        proj.add(ProjectedEntry::file(oid(1), "a.txt"));
        proj.add(ProjectedEntry::directory("dir"));
        proj.add(ProjectedEntry::symlink("link", "a.txt"));

        let files: Vec<_> = proj.files().collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.txt");
    }

    #[test]
    fn entry_type_constructors() {
        assert!(ProjectedEntryType::file().is_file());
        assert!(ProjectedEntryType::directory().is_directory());
        assert!(ProjectedEntryType::symlink("x").is_symlink());
        assert!(ProjectedEntryType::executable().is_file());
    }
}
