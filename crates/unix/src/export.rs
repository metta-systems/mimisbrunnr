//! [`Exporter`] — turn a [`PathProjection`] + a list of object ids into a
//! materialisation plan, then optionally execute it against a host directory
//! (DESIGN §12.4).
//!
//! Planning is pure: it only reads the projection and returns a `Vec<ExportEntry>`.
//! Execution is the only place this crate writes to disk; the engine layer
//! supplies a `content_provider` callback so we don't depend on any specific
//! blob source.

use std::fs;
use std::path::{Path, PathBuf};

use mimisbrunnr_types::ObjectId;

use crate::error::UnixError;
use crate::projection::PathProjection;

/// What execution should do at this entry's host_path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportAction {
    /// Materialise a regular file with content fetched via the
    /// `content_provider`.
    CreateFile,
    /// Create a symbolic link whose target is the given path (literal).
    CreateSymlink(PathBuf),
    /// Create a directory (mkdir -p).
    CreateDirectory,
}

/// One entry in an export plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportEntry {
    /// The Mímisbrunnr object whose content we'll write (or whose presence we
    /// reflect with a directory / symlink).
    pub oid: ObjectId,
    /// Relative path inside the eventual `target_root` passed to
    /// [`Exporter::execute`].
    pub host_path: PathBuf,
    /// What execution should do here.
    pub action: ExportAction,
}

/// Planning- and execution-side entry-point.
pub struct Exporter;

impl Exporter {
    /// Produce a plan for exporting `oids` under `projection`. Objects that
    /// don't appear in the projection are skipped silently — the engine is
    /// expected to filter beforehand if it cares.
    pub fn plan(projection: &PathProjection, oids: &[ObjectId]) -> Vec<ExportEntry> {
        let mut out = Vec::new();
        for &oid in oids {
            if let Some(rel) = projection.lookup(oid) {
                out.push(ExportEntry {
                    oid,
                    host_path: PathBuf::from(rel),
                    action: ExportAction::CreateFile,
                });
            }
        }
        out
    }

    /// Execute a plan against `target_root`. Calls `content_provider` for
    /// each [`ExportAction::CreateFile`] entry to fetch the bytes to write.
    ///
    /// `target_root` does not need to exist; missing parent directories are
    /// created as needed.
    pub fn execute(
        plan: &[ExportEntry],
        target_root: &Path,
        content_provider: &dyn Fn(ObjectId) -> Option<Vec<u8>>,
    ) -> Result<(), UnixError> {
        // Sort so that directories come before their contents. Lexicographic
        // ordering of host_path is enough since "a" < "a/b".
        let mut sorted: Vec<&ExportEntry> = plan.iter().collect();
        sorted.sort_by(|a, b| a.host_path.cmp(&b.host_path));

        for entry in sorted {
            let dst = target_root.join(&entry.host_path);
            match &entry.action {
                ExportAction::CreateDirectory => {
                    fs::create_dir_all(&dst)?;
                }
                ExportAction::CreateFile => {
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let raw = content_provider(entry.oid)
                        .ok_or_else(|| UnixError::MissingContent(entry.oid.to_u64()))?;
                    fs::write(&dst, raw)?;
                }
                ExportAction::CreateSymlink(target) => {
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    #[cfg(unix)]
                    {
                        std::os::unix::fs::symlink(target, &dst)?;
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = target;
                        log::warn!(
                            "export: symlink {} skipped (not supported on this platform)",
                            dst.display()
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mimisbrunnr_types::TagId;
    use tempfile::TempDir;

    use super::*;

    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0, local)
    }

    #[test]
    fn plan_yields_correct_host_paths() {
        let mut p = PathProjection::new(TagId::new(1), PathBuf::from("/r"));
        p.add(oid(1), "src/main.rs".into()).unwrap();
        p.add(oid(2), "README.md".into()).unwrap();
        let plan = Exporter::plan(&p, &[oid(1), oid(2), oid(3)]);
        // oid(3) is missing from the projection — skipped.
        assert_eq!(plan.len(), 2);
        let by_oid: std::collections::HashMap<_, _> =
            plan.iter().map(|e| (e.oid, e.host_path.clone())).collect();
        assert_eq!(by_oid[&oid(1)], PathBuf::from("src/main.rs"));
        assert_eq!(by_oid[&oid(2)], PathBuf::from("README.md"));
        for e in &plan {
            assert_eq!(e.action, ExportAction::CreateFile);
        }
    }

    #[test]
    fn execute_creates_files_with_content() {
        let mut p = PathProjection::new(TagId::new(1), PathBuf::from("/r"));
        p.add(oid(1), "deep/path/main.rs".into()).unwrap();
        p.add(oid(2), "top.txt".into()).unwrap();

        let plan = Exporter::plan(&p, &[oid(1), oid(2)]);
        let tmp = TempDir::new().unwrap();
        let provider = |o: ObjectId| -> Option<Vec<u8>> {
            match o.local_seq() {
                1 => Some(b"fn main() {}".to_vec()),
                2 => Some(b"hello".to_vec()),
                _ => None,
            }
        };

        Exporter::execute(&plan, tmp.path(), &provider).unwrap();

        assert_eq!(
            fs::read_to_string(tmp.path().join("deep/path/main.rs")).unwrap(),
            "fn main() {}"
        );
        assert_eq!(
            fs::read_to_string(tmp.path().join("top.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn execute_errors_when_provider_returns_none() {
        let mut p = PathProjection::new(TagId::new(1), PathBuf::from("/r"));
        p.add(oid(1), "missing.txt".into()).unwrap();
        let plan = Exporter::plan(&p, &[oid(1)]);
        let tmp = TempDir::new().unwrap();
        let provider = |_o: ObjectId| -> Option<Vec<u8>> { None };
        let err = Exporter::execute(&plan, tmp.path(), &provider).unwrap_err();
        assert!(matches!(err, UnixError::MissingContent(_)));
    }

    #[cfg(unix)]
    #[test]
    fn execute_creates_symlink() {
        let tmp = TempDir::new().unwrap();
        let plan = vec![ExportEntry {
            oid: oid(1),
            host_path: PathBuf::from("link"),
            action: ExportAction::CreateSymlink(PathBuf::from("../target")),
        }];
        let provider = |_o: ObjectId| -> Option<Vec<u8>> { None };
        Exporter::execute(&plan, tmp.path(), &provider).unwrap();
        let link = tmp.path().join("link");
        assert!(link.is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("../target"));
    }

    #[test]
    fn execute_creates_directory() {
        let tmp = TempDir::new().unwrap();
        let plan = vec![ExportEntry {
            oid: oid(1),
            host_path: PathBuf::from("a/b/c"),
            action: ExportAction::CreateDirectory,
        }];
        let provider = |_o: ObjectId| -> Option<Vec<u8>> { None };
        Exporter::execute(&plan, tmp.path(), &provider).unwrap();
        assert!(tmp.path().join("a/b/c").is_dir());
    }
}
