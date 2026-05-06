//! [`Importer`] — walk a host filesystem subtree and produce a stream of
//! [`ImportEntry`]s ready for the engine layer to ingest (DESIGN §12.5).
//!
//! Side-effect-only operations (filesystem reads); does **not** create
//! Mímisbrunnr objects, hash blobs, or assert tags. Those concerns live in
//! the engine. This crate's contribution is shape: deciding which host paths
//! become which kinds of entry, with what relative path under the chosen
//! context, and producing the path attribute (via [`build_path_attr`]) for
//! the engine to attach.
//!
//! Symlinks are recorded as [`ImportKind::Symlink`] by default and are *not*
//! followed; pass [`Importer::with_follow_symlinks`] if you want to chase
//! them.

use std::fs;
use std::path::{Path, PathBuf};

use mimisbrunnr_types::{Assertion, TagId};

use crate::error::UnixError;
use crate::storage::build_path_attr;

/// What kind of host-side filesystem entity an [`ImportEntry`] represents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportKind {
    /// Regular file.
    File,
    /// Symbolic link with its raw target. The target is the literal contents
    /// of the link, not a resolved path.
    Symlink(PathBuf),
    /// Directory. Mímisbrunnr does not store directory objects (DESIGN
    /// §12.2), but the entry is emitted so callers that want to mirror an
    /// empty tree can still see them.
    Directory,
}

/// One entry produced by [`Importer::scan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEntry {
    /// Absolute host path.
    pub host_path: PathBuf,
    /// Path relative to the importer's root, in Unix forward-slash form.
    pub relative_path: String,
    /// File / Symlink / Directory.
    pub kind: ImportKind,
    /// Reported size in bytes (0 for symlinks and directories).
    pub size_bytes: u64,
    /// Modification time in nanoseconds since the Unix epoch. `0` if the
    /// platform doesn't expose it.
    pub modified_ns: i64,
}

/// Recursive host-tree walker.
pub struct Importer {
    root: PathBuf,
    context: TagId,
    include_hidden: bool,
    follow_symlinks: bool,
}

impl Importer {
    /// New importer rooted at `root`, tagging entries with `context` (passed
    /// through unchanged when the engine eventually constructs path
    /// assertions via [`build_path_attr`]).
    pub fn new(root: &Path, context: TagId) -> Self {
        Self {
            root: root.to_path_buf(),
            context,
            include_hidden: false,
            follow_symlinks: false,
        }
    }

    /// Include or exclude dotfiles / dot-directories. Default: false.
    pub fn with_hidden(mut self, include: bool) -> Self {
        self.include_hidden = include;
        self
    }

    /// Follow symlinks (treat them as files / directories) instead of
    /// recording them as `Symlink`. Default: false.
    pub fn with_follow_symlinks(mut self, follow: bool) -> Self {
        self.follow_symlinks = follow;
        self
    }

    /// The context tag this importer is scoped to.
    pub fn context(&self) -> TagId {
        self.context
    }

    /// Walk the host tree and return every entry found.
    pub fn scan(self) -> Result<Vec<ImportEntry>, UnixError> {
        let mut out = Vec::new();
        let root = self.root.clone();
        self.walk(&root, &mut out)?;
        Ok(out)
    }

    /// Inner recursive walker.
    fn walk(&self, dir: &Path, out: &mut Vec<ImportEntry>) -> Result<(), UnixError> {
        let read = fs::read_dir(dir)?;
        for entry in read {
            let entry = entry?;
            let host_path = entry.path();

            // Hidden filtering: any component whose final segment starts with
            // a dot. Skip `.` / `..` since read_dir doesn't yield them anyway.
            if !self.include_hidden
                && let Some(name) = host_path.file_name().and_then(|n| n.to_str())
                && name.starts_with('.')
            {
                continue;
            }

            // symlink_metadata so we see the link itself, not its target.
            let lmeta = match fs::symlink_metadata(&host_path) {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("import: stat failed for {}: {e}", host_path.display());
                    continue;
                }
            };

            let relative_path = match host_path.strip_prefix(&self.root) {
                Ok(rel) => match rel.to_str() {
                    Some(s) => s.replace('\\', "/"),
                    None => {
                        return Err(UnixError::NonUtf8Path(host_path.clone()));
                    }
                },
                Err(_) => {
                    // Shouldn't happen — host_path was built from `dir` which
                    // descends from `self.root`. Defensive fallback.
                    host_path.to_string_lossy().into_owned()
                }
            };

            let modified_ns = lmeta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);

            if lmeta.file_type().is_symlink() {
                if self.follow_symlinks {
                    // Resolve once via metadata() (follows links). If that
                    // fails, log and skip rather than aborting the whole
                    // scan.
                    match fs::metadata(&host_path) {
                        Ok(meta) => {
                            if meta.is_dir() {
                                out.push(ImportEntry {
                                    host_path: host_path.clone(),
                                    relative_path,
                                    kind: ImportKind::Directory,
                                    size_bytes: 0,
                                    modified_ns,
                                });
                                self.walk(&host_path, out)?;
                            } else if meta.is_file() {
                                out.push(ImportEntry {
                                    host_path,
                                    relative_path,
                                    kind: ImportKind::File,
                                    size_bytes: meta.len(),
                                    modified_ns,
                                });
                            }
                        }
                        Err(e) => {
                            log::warn!(
                                "import: dangling symlink at {}: {e}",
                                host_path.display()
                            );
                        }
                    }
                } else {
                    let target = fs::read_link(&host_path).unwrap_or_default();
                    out.push(ImportEntry {
                        host_path,
                        relative_path,
                        kind: ImportKind::Symlink(target),
                        size_bytes: 0,
                        modified_ns,
                    });
                }
            } else if lmeta.is_dir() {
                out.push(ImportEntry {
                    host_path: host_path.clone(),
                    relative_path,
                    kind: ImportKind::Directory,
                    size_bytes: 0,
                    modified_ns,
                });
                self.walk(&host_path, out)?;
            } else if lmeta.is_file() {
                out.push(ImportEntry {
                    host_path,
                    relative_path,
                    kind: ImportKind::File,
                    size_bytes: lmeta.len(),
                    modified_ns,
                });
            }
        }
        Ok(())
    }

    /// Derive the assertions to attach to the object created from `entry`.
    ///
    /// At this layer we only emit the path attribute. Type tags (`file`,
    /// `directory`, `symlink`), content-derived tags (mime sniff, hashes),
    /// and ownership / mode attributes are wired by the engine + ontology
    /// layer that has the resolved tag ids in hand.
    pub fn derive_assertions(
        &self,
        unix_path_tag: TagId,
        entry: &ImportEntry,
    ) -> Result<Vec<Assertion>, UnixError> {
        let attr = build_path_attr(unix_path_tag, self.context, &entry.relative_path)?;
        Ok(vec![attr])
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use mimisbrunnr_types::Value;
    use tempfile::TempDir;

    use super::*;

    fn ctx() -> TagId {
        TagId::new(7)
    }

    #[cfg(unix)]
    #[test]
    fn scan_walks_tree_with_dirs_files_and_symlink() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // 2 dirs
        fs::create_dir_all(root.join("src/inner")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        // 3 files
        fs::write(root.join("README.md"), "# hi").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("docs/intro.md"), "intro").unwrap();
        // 1 symlink (to README.md)
        std::os::unix::fs::symlink("README.md", root.join("link-to-readme")).unwrap();

        let entries = Importer::new(root, ctx()).scan().unwrap();

        let mut files = 0;
        let mut dirs = 0;
        let mut symlinks = 0;
        for e in &entries {
            match &e.kind {
                ImportKind::File => files += 1,
                ImportKind::Directory => dirs += 1,
                ImportKind::Symlink(target) => {
                    symlinks += 1;
                    assert_eq!(target, &PathBuf::from("README.md"));
                }
            }
        }
        assert_eq!(files, 3);
        // src, src/inner, docs
        assert_eq!(dirs, 3);
        assert_eq!(symlinks, 1);

        // Relative paths use forward slashes and are not absolute.
        for e in &entries {
            assert!(!e.relative_path.starts_with('/'));
        }

        // README.md size and a non-empty modified_ns for the file.
        let readme = entries
            .iter()
            .find(|e| e.relative_path == "README.md")
            .unwrap();
        assert!(matches!(readme.kind, ImportKind::File));
        assert_eq!(readme.size_bytes, "# hi".len() as u64);
    }

    #[test]
    fn with_hidden_false_skips_dotfiles() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("visible.txt"), "v").unwrap();
        fs::write(root.join(".hidden"), "h").unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD"), "ref").unwrap();

        let entries = Importer::new(root, ctx()).scan().unwrap();
        assert!(entries.iter().any(|e| e.relative_path == "visible.txt"));
        assert!(entries.iter().all(|e| !e.relative_path.starts_with('.')));
        assert!(
            !entries
                .iter()
                .any(|e| e.relative_path.contains(".git"))
        );
    }

    #[test]
    fn with_hidden_true_includes_dotfiles() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("visible.txt"), "v").unwrap();
        fs::write(root.join(".hidden"), "h").unwrap();

        let entries = Importer::new(root, ctx())
            .with_hidden(true)
            .scan()
            .unwrap();
        assert!(entries.iter().any(|e| e.relative_path == "visible.txt"));
        assert!(entries.iter().any(|e| e.relative_path == ".hidden"));
    }

    #[test]
    fn derive_assertions_returns_path_attr() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.txt"), "a").unwrap();
        let entries = Importer::new(root, ctx()).scan().unwrap();
        let entry = entries
            .iter()
            .find(|e| e.relative_path == "a.txt")
            .unwrap();
        let unix_path = TagId::new(11);
        // Build a fresh importer (scan consumes self) to call derive_assertions.
        let importer = Importer::new(root, ctx());
        let assertions = importer.derive_assertions(unix_path, entry).unwrap();
        assert_eq!(assertions.len(), 1);
        match &assertions[0] {
            Assertion::Attr { key, value } => {
                assert_eq!(*key, unix_path);
                match value {
                    Value::Scoped { context, inner } => {
                        assert_eq!(*context, ctx());
                        assert!(matches!(inner.as_ref(), Value::Text(s) if s == "a.txt"));
                    }
                    other => panic!("expected Scoped, got {other:?}"),
                }
            }
            other => panic!("expected Attr, got {other:?}"),
        }
    }
}
